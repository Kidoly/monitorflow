//! The update service's side of MSI packages (ADR-0188), Windows only:
//! whether a product is installed, the checked file held open, and
//! `msiexec` as LocalSystem.

use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use monitorflow_sensors::powershell::Job;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
    MsiQueryProductStateW, INSTALLSTATE_DEFAULT,
};

use super::{
    download, failed, install_args, remove_args, result, verify, MsiPackage, INSTALL_TIMEOUT,
};
use crate::packages::windows_update::{Installed, Reply, Request};

/// Where the service downloads MSI files and `msiexec` writes its logs:
/// under `%ProgramData%\MonitorFlow`, which only SYSTEM and
/// Administrators can write (ADR-0050).
pub fn dir() -> PathBuf {
    monitorflow_common::config::windows_base_dir()
        .join("update")
        .join("msi")
}

/// Answers an MSI request, already checked, or `None` for any other. Runs
/// on the blocking pool, inside the service's runtime.
pub fn handle(request: &Request) -> Option<Reply> {
    let runtime = tokio::runtime::Handle::current();
    let installed = match request {
        Request::MsiInstall { packages } => runtime.block_on(install(packages, &dir())),
        Request::MsiRemove { packages } => runtime.block_on(remove(packages, &dir())),
        _ => return None,
    };
    Some(Reply::Installed(installed))
}

/// Whether Windows Installer lists `product_code` as installed for the
/// machine or a user.
pub fn is_installed(product_code: &str) -> bool {
    let wide: Vec<u16> = product_code.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated UTF-16 string that outlives the call.
    unsafe { MsiQueryProductStateW(wide.as_ptr()) == INSTALLSTATE_DEFAULT }
}

/// `%SystemRoot%\System32\msiexec.exe`.
fn msiexec() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
        .join("msiexec.exe")
}

/// Runs `msiexec` with `args` and returns its exit code, or `None` when
/// it did not finish in [`INSTALL_TIMEOUT`].
async fn run(args: &[String]) -> Result<Option<i32>, String> {
    let not_started = |e: std::io::Error| format!("cannot start msiexec: {e}");
    let job = Job::new().map_err(not_started)?;
    let mut command = tokio::process::Command::new(msiexec());
    for arg in args {
        // Windows Installer parses its command line itself: see
        // `install_args`.
        command.raw_arg(arg);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(not_started)?;
    job.assign(&mut child).map_err(not_started)?;
    let finished = tokio::time::timeout(INSTALL_TIMEOUT, child.wait()).await;
    drop(job);
    match finished {
        Ok(Ok(status)) => Ok(Some(status.code().unwrap_or(-1))),
        Ok(Err(error)) => Err(format!("waiting for msiexec: {error}")),
        Err(_) => Ok(None),
    }
}

/// Installs the packages not installed yet, one at a time, each from its
/// downloaded and checked file, under `dir`.
pub async fn install(packages: &[MsiPackage], dir: &Path) -> Installed {
    let mut installed = Installed::default();
    for package in packages {
        if is_installed(&package.product_code) {
            installed.unchanged.push(package.product_code.clone());
            continue;
        }
        let file = match download(package, dir).await {
            Ok(file) => file,
            Err(error) => {
                installed.results.push(failed(package, &error));
                continue;
            }
        };
        // Held open without write or delete sharing until msiexec is done,
        // and checked again through that handle: nothing can change the
        // file between the check and the install.
        let (path, sha256, size) = (file.clone(), package.sha256.clone(), package.size);
        let held = tokio::task::spawn_blocking(move || {
            let mut held = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)
                .map_err(|e| format!("cannot open the file: {e}"))?;
            verify(&mut held, &sha256, size).map(|()| held)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|checked| checked);
        let held = match held {
            Ok(held) => held,
            Err(error) => {
                let _ = std::fs::remove_file(&file);
                installed.results.push(failed(package, &error));
                continue;
            }
        };
        tracing::info!(product_code = %package.product_code, name = %package.name, sha256 = %package.sha256, "installing an MSI package");
        let args = install_args(&file, &package.properties, &package.log_in(dir));
        let ran = run(&args).await;
        drop(held);
        let _ = std::fs::remove_file(&file);
        push(&mut installed, package, ran, true);
    }
    installed
}

/// Removes the packages installed, one at a time.
pub async fn remove(packages: &[MsiPackage], dir: &Path) -> Installed {
    let mut installed = Installed::default();
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::warn!(%error, "cannot create the directory of msiexec's logs");
    }
    for package in packages {
        if !is_installed(&package.product_code) {
            installed.unchanged.push(package.product_code.clone());
            continue;
        }
        tracing::info!(product_code = %package.product_code, name = %package.name, "removing an MSI package");
        let ran = run(&remove_args(&package.product_code, &package.log_in(dir))).await;
        push(&mut installed, package, ran, false);
    }
    installed
}

/// Adds the result of one run of `msiexec` to `installed`, checking that
/// the product is then installed, or gone.
fn push(
    installed: &mut Installed,
    package: &MsiPackage,
    ran: Result<Option<i32>, String>,
    install: bool,
) {
    let code = match ran {
        Ok(Some(code)) => code,
        Ok(None) => {
            let minutes = INSTALL_TIMEOUT.as_secs() / 60;
            installed.results.push(failed(
                package,
                &format!("msiexec did not finish in {minutes} minutes"),
            ));
            return;
        }
        Err(error) => {
            installed.results.push(failed(package, &error));
            return;
        }
    };
    let (line, reboot) = result(package, code);
    if line.result.succeeded() && is_installed(&package.product_code) != install {
        let why = if install {
            "msiexec succeeded, but Windows lists no product with this ProductCode: the file \
             installs another one"
        } else {
            "msiexec succeeded, but the product is still installed"
        };
        installed.results.push(failed(package, why));
        return;
    }
    installed.reboot_required |= reboot;
    installed.results.push(line);
}
