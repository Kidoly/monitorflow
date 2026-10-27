//! Agent releases on a mirroring relay (ADR-0053): each cycle copies the
//! newest [`KEEP`] releases the upstream's index lists, from its
//! `/agent/<version>/`, into `<mirror_dir>/agent/<version>/`, which the
//! relay's listener serves to its agents. A release is checked like the
//! agents check it, against the release keys built into the relay, before
//! it is served; agents check it again.

use std::path::Path;
use std::str::FromStr;

use monitorflow_common::release::{
    self, cached_versions, Platform, PublicKey, Version, SIGNATURE, SUMS, VERSION_FILE,
};

use super::fetch::Fetcher;

/// Releases kept: the newest and the one before, for agents still on it.
pub const KEEP: usize = 2;
/// Largest agent binary copied.
pub const MAX_BINARY: u64 = 512 * 1024 * 1024;
const MAX_SMALL: u64 = 64 * 1024;

/// Copies the newest [`KEEP`] of `listed` that `dir` lacks and removes the
/// others. Returns the versions copied; an error stops at the first
/// release that cannot be copied, leaving the others as they were.
pub async fn sync(
    fetcher: &Fetcher,
    dir: &Path,
    listed: &[String],
    keys: &[PublicKey],
) -> Result<Vec<Version>, String> {
    let mut wanted: Vec<Version> = listed
        .iter()
        .filter_map(|v| Version::from_str(v).ok())
        .collect();
    wanted.sort_by(|a, b| b.cmp(a));
    wanted.dedup();
    wanted.truncate(KEEP);
    let held = cached(dir).await?;
    let mut copied = Vec::new();
    for version in &wanted {
        if !held.contains(version) {
            copy(fetcher, dir, version, keys).await?;
            copied.push(version.clone());
        }
    }
    for old in held.iter().filter(|v| !wanted.contains(v)) {
        let path = dir.join(old.to_string());
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        tracing::info!(version = %old, "old agent release removed");
    }
    Ok(copied)
}

async fn cached(dir: &Path) -> Result<Vec<Version>, String> {
    let owned = dir.to_owned();
    tokio::task::spawn_blocking(move || cached_versions(&owned))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// Downloads, checks and publishes one release.
async fn copy(
    fetcher: &Fetcher,
    dir: &Path,
    version: &Version,
    keys: &[PublicKey],
) -> Result<(), String> {
    let base = format!("agent/{version}");
    let get = |name: &'static str, limit: u64| {
        let path = format!("{base}/{name}");
        async move {
            fetcher
                .bytes(&path, limit)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("the upstream has no {path}"))
        }
    };
    let sums = get(SUMS, MAX_SMALL).await?;
    let signature = get(SIGNATURE, MAX_SMALL).await?;
    let version_file = get(VERSION_FILE, MAX_SMALL).await?;
    let verified = release::verify(&sums, &signature, &version_file, keys)
        .map_err(|e| format!("agent release {version}: {e}"))?;
    if &verified.version != version {
        return Err(format!(
            "agent release {version}: VERSION says {}",
            verified.version
        ));
    }
    let mut files = vec![
        (SUMS, sums),
        (SIGNATURE, signature),
        (VERSION_FILE, version_file),
    ];
    for platform in [
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::WindowsX86_64,
    ] {
        let name = platform.agent_asset();
        if verified.digest(name).is_err() {
            continue;
        }
        let path = format!("{base}/{name}");
        // The upstream may hold only some binaries.
        let Some(bytes) = fetcher
            .bytes(&path, MAX_BINARY)
            .await
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        verified
            .check(name, &bytes)
            .map_err(|e| format!("agent release {version}: {e}"))?;
        files.push((name, bytes));
    }
    if files.len() == 3 {
        return Err(format!(
            "agent release {version}: the upstream has no binary"
        ));
    }
    // Built under a dot name, which the listener does not serve, then
    // renamed into place.
    let staging = dir.join(format!(".{version}.part"));
    let target = dir.join(version.to_string());
    let fail = |path: &Path| {
        let path = path.display().to_string();
        move |e: std::io::Error| format!("{path}: {e}")
    };
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(fail(&staging))?;
    for (name, bytes) in files {
        let path = staging.join(name);
        tokio::fs::write(&path, bytes).await.map_err(fail(&path))?;
    }
    let _ = tokio::fs::remove_dir_all(&target).await;
    tokio::fs::rename(&staging, &target)
        .await
        .map_err(fail(&target))?;
    tracing::info!(%version, "agent release copied");
    Ok(())
}

#[cfg(test)]
mod tests;
