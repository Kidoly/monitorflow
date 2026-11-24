//! The update service's side of winget (ADR-0139), Windows only: reads
//! the App Installer policy, checks that PowerShell 7 and the module are
//! there, and runs the fixed scripts in PowerShell 7 as LocalSystem.

use std::path::PathBuf;
use std::time::Duration;

use monitorflow_sensors::powershell::{self, Job};
use tokio::io::AsyncReadExt;

use super::{
    encoded, not_attempted, parse_scan, parse_upgrade, plan_install, plan_remove, script, select,
    source_turned_off, timed_out, App, Apps, Availability, Paths, Planned, Policy, Upgraded,
    ENV_EXCLUDE, ENV_ID, ENV_SOURCE, INSTALL_SCRIPT, NO_MODULE, NO_POWERSHELL, REMOVE_SCRIPT,
    SCAN_SCRIPT, SCAN_TIMEOUT, TURNED_OFF, UPGRADE_SCRIPT, UPGRADE_TIMEOUT,
};
use crate::packages::windows_update::source::open;
use crate::packages::windows_update::{Installed, Reply, Request, WingetPackage};

const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\AppInstaller";
/// Most output read from a script: a scan of 5,000 apps is about 1 MiB.
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;

/// Answers a winget request, already checked, or `None` for any other.
/// Runs on the blocking pool, inside the service's runtime.
pub fn handle(request: &Request) -> Option<Reply> {
    let runtime = tokio::runtime::Handle::current();
    match request {
        Request::WingetScan => Some(Reply::Apps(runtime.block_on(scan()))),
        Request::WingetUpgrade { ids, hold } => Some(
            runtime
                .block_on(upgrade(ids, hold))
                .map_or_else(Reply::Error, Reply::Installed),
        ),
        Request::WingetInstall { packages } => Some(
            runtime
                .block_on(change(packages, true))
                .map_or_else(Reply::Error, Reply::Installed),
        ),
        Request::WingetRemove { packages } => Some(
            runtime
                .block_on(change(packages, false))
                .map_or_else(Reply::Error, Reply::Installed),
        ),
        _ => None,
    }
}

/// The App Installer policy of this host.
fn policy() -> Result<Policy, String> {
    let key = open(POLICY_KEY)?;
    let [app_installer, command_line, default_source] =
        Policy::VALUES.map(|name| key.as_ref().and_then(|k| k.get_u32(name).ok()));
    Ok(Policy {
        enable_app_installer: app_installer,
        enable_command_line: command_line,
        enable_default_source: default_source,
    })
}

/// `%ProgramFiles%` of 64-bit programs, from the service's environment.
fn paths() -> Paths {
    let program_files = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map_or_else(|| PathBuf::from(r"C:\Program Files"), PathBuf::from);
    Paths::under(&program_files)
}

/// Whether winget can run here, and with what policy.
fn ready() -> Result<(Paths, Policy), Apps> {
    let policy = policy()
        .map_err(|e| Apps::unusable(&format!("cannot read the App Installer policy: {e}")))?;
    if policy.disabled() {
        return Err(Apps::unusable(TURNED_OFF));
    }
    let paths = paths();
    if !paths.pwsh.is_file() {
        return Err(Apps::absent(NO_POWERSHELL));
    }
    if !paths.module().is_dir() {
        return Err(Apps::absent(NO_MODULE));
    }
    Ok((paths, policy))
}

/// The apps winget lists from the sources policy leaves.
pub async fn scan() -> Apps {
    match ready() {
        Ok((paths, policy)) => scan_with(&paths, &policy).await,
        Err(apps) => apps,
    }
}

async fn scan_with(paths: &Paths, policy: &Policy) -> Apps {
    let exclude = policy.excluded().join(",");
    match run(paths, SCAN_SCRIPT, &[(ENV_EXCLUDE, &exclude)], SCAN_TIMEOUT).await {
        Ok(Some(stdout)) => {
            let (apps, invalid) = parse_scan(&stdout, policy);
            if invalid > 0 {
                tracing::warn!(
                    invalid,
                    "winget apps with an ID outside the grammar are left out"
                );
            }
            apps
        }
        Ok(None) => Apps::unusable(&format!(
            "the winget scan did not finish in {} minutes",
            SCAN_TIMEOUT.as_secs() / 60
        )),
        Err(error) => Apps::unusable(&error),
    }
}

/// Installs (`install`) or removes `packages`, one at a time, from a
/// fresh scan: what is already as asked is left alone (ADR-0188).
pub async fn change(packages: &[WingetPackage], install: bool) -> Result<Installed, String> {
    let (paths, policy) = ready().map_err(|apps| apps.error)?;
    let apps = scan_with(&paths, &policy).await;
    if apps.state != Availability::Available {
        return Err(apps.error);
    }
    let planned = if install {
        let packages: Vec<(String, String)> = packages
            .iter()
            .map(|p| (p.id.clone(), p.source.clone()))
            .collect();
        plan_install(&packages, &apps.apps)
    } else {
        let ids: Vec<String> = packages.iter().map(|p| p.id.clone()).collect();
        plan_remove(&ids, &apps.apps)
    };
    let (body, verb) = if install {
        (INSTALL_SCRIPT, "installing")
    } else {
        (REMOVE_SCRIPT, "removing")
    };
    let mut installed = Installed::default();
    for planned in planned {
        let app = match planned {
            Planned::Unchanged(id) => {
                installed.unchanged.push(id);
                continue;
            }
            Planned::Run(app) => app,
        };
        if install && !policy.allows(&app.source) {
            installed.results.push(source_turned_off(&app));
            continue;
        }
        tracing::info!(id = %app.id, source = %app.source, "{verb} a winget package");
        run_one(&paths, body, &app, &mut installed).await?;
    }
    Ok(installed)
}

/// Runs `body` for `app` and adds its result to `installed`.
async fn run_one(
    paths: &Paths,
    body: &str,
    app: &App,
    installed: &mut Installed,
) -> Result<(), String> {
    let env = [(ENV_ID, app.id.as_str()), (ENV_SOURCE, app.source.as_str())];
    let result = match run(paths, body, &env, UPGRADE_TIMEOUT).await {
        Ok(Some(stdout)) => parse_upgrade(app, &stdout),
        Ok(None) => Upgraded::Done(timed_out(app, UPGRADE_TIMEOUT), false),
        Err(error) => return Err(error),
    };
    match result {
        Upgraded::Done(result, reboot) => {
            installed.reboot_required |= reboot;
            installed.results.push(result);
        }
        Upgraded::NotPending => installed.not_pending.push(app.id.clone()),
    }
    Ok(())
}

/// Updates the pending apps among `ids`, every one when empty, but those
/// `hold` names, one at a time, from a fresh scan.
pub async fn upgrade(ids: &[String], hold: &[String]) -> Result<Installed, String> {
    let (paths, policy) = ready().map_err(|apps| apps.error)?;
    let apps = scan_with(&paths, &policy).await;
    if apps.state != Availability::Available {
        return Err(apps.error);
    }
    let (picked, left) = select(ids, hold, &apps.apps);
    let over = left.over;
    let mut installed = Installed {
        not_pending: left.not_pending,
        held: left.held,
        ..Installed::default()
    };
    for app in picked {
        tracing::info!(id = %app.id, source = %app.source, version = %app.available, "updating a winget app");
        run_one(&paths, UPGRADE_SCRIPT, app, &mut installed).await?;
    }
    installed
        .results
        .extend(over.into_iter().map(not_attempted));
    Ok(installed)
}

/// Runs the script `body` in PowerShell 7 with `env`, and returns its
/// stdout, or `None` when it did not finish in `timeout`. Everything it
/// started is killed when it returns.
async fn run(
    paths: &Paths,
    body: &str,
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<Option<Vec<u8>>, String> {
    let not_started = |e: std::io::Error| format!("cannot start {}: {e}", paths.pwsh.display());
    let job = Job::new().map_err(not_started)?;
    let mut command = powershell::command(&paths.pwsh).map_err(not_started)?;
    command
        .env("PSModulePath", &paths.modules)
        .envs(env.iter().copied())
        .args(powershell::OPTIONS)
        .arg("-EncodedCommand")
        .arg(encoded(&script(body)));
    let mut child = command.spawn().map_err(not_started)?;
    job.assign(&mut child).map_err(not_started)?;
    let mut stdout = child.stdout.take().expect("piped");
    let finished = tokio::time::timeout(timeout, async {
        let mut output = Vec::new();
        (&mut stdout)
            .take(MAX_OUTPUT)
            .read_to_end(&mut output)
            .await?;
        child.wait().await?;
        Ok::<_, std::io::Error>(output)
    })
    .await;
    drop(job);
    match finished {
        Ok(Ok(output)) => Ok(Some(output)),
        Ok(Err(error)) => Err(format!("reading winget's output: {error}")),
        Err(_) => Ok(None),
    }
}
