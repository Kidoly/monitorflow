//! MSI packages of the software catalogue (ADR-0188), the update
//! service's side. A `software` command names each by its ProductCode,
//! with the SHA-256, size and URLs of its file for an install. The update
//! service, as LocalSystem, downloads the file into its own directory,
//! which the agent's account cannot write, checks its size and digest
//! against the signed command, holds it open so that nobody can change it,
//! checks it again and only then runs `msiexec /i <file> /qn /norestart`
//! with the command's public properties; `msiexec /x {ProductCode} /qn
//! /norestart` removes one. The ProductCode also says whether the product
//! is installed: an install of an installed one, or a removal of a missing
//! one, changes nothing.
//!
//! This part builds everywhere, so Linux tests cover it: the requests,
//! the command lines, exit codes and the checked download. [`run`] runs
//! them, Windows only.

#[cfg(windows)]
pub mod run;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::proto::v1::{MsiFile, MsiProperty, SoftwarePackage};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::packages::windows_update::{InstallResult, Outcome};

/// Longest run of one `msiexec`, as for a winget package.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Connecting, and every wait for more of a file.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// An MSI package on the pipe, from a signed `software` command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MsiPackage {
    pub product_code: String,
    /// The catalogue entry's name, for the output.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The file, for an install; empty for a removal.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Public properties, `(name, value)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<(String, String)>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl MsiPackage {
    /// The MSI package `package` stands for, if it is one.
    pub fn of(package: &SoftwarePackage) -> Option<Self> {
        let file = package.msi.as_ref()?;
        Some(Self {
            product_code: package.id.clone(),
            name: file.name.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
            urls: file.urls.clone(),
            properties: file
                .properties
                .iter()
                .map(|p| (p.name.clone(), p.value.clone()))
                .collect(),
        })
    }

    /// The command's package again, for its check.
    pub fn to_package(&self) -> SoftwarePackage {
        SoftwarePackage {
            id: self.product_code.clone(),
            source: String::new(),
            msi: Some(MsiFile {
                sha256: self.sha256.clone(),
                size: self.size,
                urls: self.urls.clone(),
                properties: self
                    .properties
                    .iter()
                    .map(|(name, value)| MsiProperty {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect(),
                name: self.name.clone(),
            }),
        }
    }

    /// What the output calls it: its name, else its ProductCode.
    pub fn title(&self) -> &str {
        if self.name.is_empty() {
            &self.product_code
        } else {
            &self.name
        }
    }

    /// Where its file is downloaded to, under `dir`.
    pub fn file_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.msi", self.sha256))
    }

    /// Where `msiexec` writes its log, under `dir`: one per product, the
    /// last run's, for the host's administrator.
    pub fn log_in(&self, dir: &Path) -> PathBuf {
        let code = self.product_code.trim_matches(['{', '}']);
        dir.join(format!("msiexec-{code}.log"))
    }
}

/// The arguments of `msiexec.exe` that install `file` with `properties`,
/// logging to `log`, as raw arguments: Windows Installer reads
/// `NAME="value"` itself, which a quoted argument would hide. The values
/// were checked to hold no `"` ([`monitorflow_common::software::valid_msi_value`])
/// and the paths are the service's own.
pub fn install_args(file: &Path, properties: &[(String, String)], log: &Path) -> Vec<String> {
    let mut args = vec![
        "/i".to_owned(),
        format!("\"{}\"", file.display()),
        "/qn".to_owned(),
        "/norestart".to_owned(),
        "/l*v".to_owned(),
        format!("\"{}\"", log.display()),
    ];
    args.extend(
        properties
            .iter()
            .map(|(name, value)| format!("{name}=\"{value}\"")),
    );
    args
}

/// The arguments of `msiexec.exe` that remove `product_code`.
pub fn remove_args(product_code: &str, log: &Path) -> Vec<String> {
    vec![
        "/x".to_owned(),
        product_code.to_owned(),
        "/qn".to_owned(),
        "/norestart".to_owned(),
        "/l*v".to_owned(),
        format!("\"{}\"", log.display()),
    ]
}

/// What an exit code of `msiexec` means: whether it succeeded, whether
/// Windows needs a restart, and why it failed.
pub fn exit(code: i32) -> (bool, bool, &'static str) {
    match code {
        0 => (true, false, ""),
        // ERROR_SUCCESS_REBOOT_REQUIRED; ERROR_SUCCESS_REBOOT_INITIATED,
        // which /norestart should prevent.
        3010 | 1641 => (true, true, ""),
        1602 => (false, false, "cancelled"),
        1603 => (false, false, "fatal error during installation"),
        1605 => (false, false, "the product is not installed"),
        1618 => (false, false, "another installation is in progress"),
        1619 => (false, false, "the package could not be opened"),
        1620 => (false, false, "the file is not a valid package"),
        1625 => (false, false, "system policy forbids this installation"),
        1633 => (false, false, "the package is not for this platform"),
        1638 => (false, false, "another version of this product is installed"),
        _ => (false, false, "msiexec failed"),
    }
}

/// The result line of `package` after `msiexec` exited with `code`.
pub fn result(package: &MsiPackage, code: i32) -> (InstallResult, bool) {
    let (ok, reboot, why) = exit(code);
    let title = if ok {
        package.title().to_owned()
    } else {
        format!("{}: {why} (msiexec exit code {code})", package.title())
    };
    (
        InstallResult {
            name: package.product_code.clone(),
            title,
            result: if ok {
                Outcome::Succeeded
            } else {
                Outcome::Failed
            },
            hresult: code,
        },
        reboot,
    )
}

/// A failed result of `package` that `msiexec` did not run for, `why`.
pub fn failed(package: &MsiPackage, why: &str) -> InstallResult {
    InstallResult {
        name: package.product_code.clone(),
        title: format!("{}: {why}", package.title()),
        result: Outcome::Failed,
        hresult: -1,
    }
}

/// Checks that `file` holds `size` bytes of SHA-256 `sha256`, reading it
/// from its start. Blocking.
pub fn verify(file: &mut std::fs::File, sha256: &str, size: u64) -> Result<(), String> {
    use std::io::{Read, Seek};

    file.rewind()
        .map_err(|e| format!("cannot read the file: {e}"))?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 1 << 16];
    let mut read = 0u64;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("cannot read the file: {e}"))?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > size {
            return Err("the file is larger than the command says".into());
        }
        context.update(&buffer[..n]);
    }
    if read != size {
        return Err(format!("the file has {read} bytes, not {size}"));
    }
    if hex(context.finish().as_ref()) != sha256 {
        return Err("the file's SHA-256 is not the command's".into());
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads the file of `package` into `dir`, from each of its URLs in
/// turn until one gives exactly its size and digest, and returns where it
/// is. Redirects are not followed: mirror listeners never send one, and
/// on plain HTTP anyone in the middle could.
pub async fn download(package: &MsiPackage, dir: &Path) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let client = reqwest::Client::builder()
        .connect_timeout(DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("monitorflow-agent/{}", monitorflow_common::VERSION))
        .build()
        .map_err(|e| e.to_string())?;
    let dest = package.file_in(dir);
    let part = dest.with_extension("part");
    let mut errors = Vec::new();
    for url in &package.urls {
        match fetch(&client, url, &part, package.size).await {
            Ok(()) => {}
            Err(error) => {
                let _ = tokio::fs::remove_file(&part).await;
                errors.push(format!("{url}: {error}"));
                continue;
            }
        }
        let (path, sha256, size) = (part.clone(), package.sha256.clone(), package.size);
        let checked = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            verify(&mut file, &sha256, size)
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Err(error) = checked {
            let _ = tokio::fs::remove_file(&part).await;
            errors.push(format!("{url}: {error}"));
            continue;
        }
        tokio::fs::rename(&part, &dest)
            .await
            .map_err(|e| format!("cannot keep the file: {e}"))?;
        return Ok(dest);
    }
    Err(format!("cannot download the file: {}", errors.join("; ")))
}

/// Writes the body of `url` to `dest`, refusing more than `size` bytes.
async fn fetch(client: &reqwest::Client, url: &str, dest: &Path, size: u64) -> Result<(), String> {
    let mut response = tokio::time::timeout(DOWNLOAD_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| "timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    if response.content_length().is_some_and(|n| n != size) {
        return Err("the file is not the size the command says".into());
    }
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    let mut written = 0u64;
    loop {
        let chunk = tokio::time::timeout(DOWNLOAD_TIMEOUT, response.chunk())
            .await
            .map_err(|_| "timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        let Some(chunk) = chunk else { break };
        written += chunk.len() as u64;
        if written > size {
            return Err("the file is larger than the command says".into());
        }
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.sync_all().await.map_err(|e| e.to_string())?;
    Ok(())
}
