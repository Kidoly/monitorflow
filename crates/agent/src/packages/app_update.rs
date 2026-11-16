//! What the `packages update` root helper runs for Flatpak and snaps
//! (ADR-0140): fixed command lines at fixed paths, whose only variable
//! part is IDs the shared grammar accepted (ADR-0138) and that are
//! installed. A manager the host lacks, or an ID it does not hold, is
//! noted in the output and is not a failure.

use std::collections::HashSet;

use monitorflow_common::proto::v1::PackageManager;
use monitorflow_common::third_party::{key, valid_id};

use super::exec::Tools;
use super::flatpak::Flatpak;
use super::snap::Snap;
use super::update::Outcome;
use super::PackageError;

/// `flatpak` arguments updating the system-wide installation, all of it or
/// only `refs` (`app/<id>/<arch>/<branch>`). `-y` and `--noninteractive`
/// keep it from asking; `--` ends the options, although no valid ref
/// starts with `-`. Runtimes the updated apps need come along.
pub fn flatpak_update(refs: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["update", "--system", "--noninteractive", "-y"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    if !refs.is_empty() {
        args.push("--".into());
        args.extend(refs.iter().cloned());
    }
    args
}

/// `snap` arguments refreshing every snap, or only `names`, each on the
/// channel it tracks. No `--ignore-running`: snapd refuses to refresh a
/// snap whose applications run, and its error says so.
pub fn snap_refresh(names: &[String]) -> Vec<String> {
    let mut args = vec!["refresh".to_owned()];
    args.extend(names.iter().cloned());
    args
}

/// Updates `manager`'s apps named in `ids`, every pending one when empty,
/// as the current user (root in the helper), recording into `outcome`.
pub async fn run(manager: PackageManager, ids: &[String], tools: &Tools, outcome: &mut Outcome) {
    let name = key(manager).unwrap_or("unknown");
    if let Some(bad) = ids.iter().find(|id| !valid_id(manager, id)) {
        outcome.error = Some(PackageError::Helper(format!(
            "invalid {name} package ID: {}",
            bad.escape_debug()
        )));
        return;
    }
    let (present, program) = match manager {
        PackageManager::Flatpak => (Flatpak::new(tools.clone()).present(), &tools.flatpak),
        PackageManager::Snap => (Snap::new(tools.clone()).present(), &tools.snap),
        _ => {
            outcome.error = Some(PackageError::Helper(format!(
                "{name} updates are not available on this system"
            )));
            return;
        }
    };
    if !present {
        outcome.note(format!("{name}: not installed"));
        return;
    }
    let mut ids = ids.to_vec();
    if !ids.is_empty() {
        let installed = match installed(manager, tools).await {
            Ok(installed) => installed,
            Err(error) => {
                outcome.error = Some(error);
                return;
            }
        };
        let (kept, missing): (Vec<String>, Vec<String>) =
            ids.into_iter().partition(|id| installed.contains(id));
        if !missing.is_empty() {
            outcome.note(format!("{name}: not installed: {}", missing.join(" ")));
        }
        if kept.is_empty() {
            return;
        }
        ids = kept;
    }
    let args = match manager {
        PackageManager::Flatpak => flatpak_update(&ids),
        _ => snap_refresh(&ids),
    };
    outcome.upgraded = true;
    outcome.step(tools.direct(&[], program, &args).await);
}

/// The IDs `manager` holds, as its update command takes them.
async fn installed(
    manager: PackageManager,
    tools: &Tools,
) -> Result<HashSet<String>, PackageError> {
    Ok(if manager == PackageManager::Snap {
        let snaps = Snap::new(tools.clone()).installed().await?;
        snaps.into_iter().map(|s| s.name).collect()
    } else {
        let packages = Flatpak::new(tools.clone()).installed().await?;
        packages.into_iter().map(|p| p.id).collect()
    })
}

#[cfg(test)]
mod tests;
