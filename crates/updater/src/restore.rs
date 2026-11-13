//! Restoring a backup (ADR-0058): `monitorflow-updater restore FILE`, run
//! by `deploy/restore.sh` in a one-off updater container, on the
//! deployment of a fresh install or on the one the backup came from.

use std::path::Path;

use monitorflow_common::release::Version;
use monitorflow_common::server_update::backup::{Manifest, DATABASE};

use crate::backup::{check_serverdata, extract_database, extract_serverdata, Owner};
use crate::host::Host;
use crate::retired_ca;

/// Replaces the database and the server's data with those of `archive`,
/// then starts the deployment. `owner` is who owns the server data,
/// [`Owner::SERVER`] but in tests; `work` is an empty directory for the
/// dump; `say` reports each step.
pub async fn restore(
    host: &dyn Host,
    archive: &Path,
    serverdata_dir: &Path,
    owner: Owner,
    work: &Path,
    say: &mut dyn FnMut(&str),
) -> Result<Manifest, String> {
    let manifest = extract_database(archive, work).await?;
    say(&format!(
        "backup of {} ({} backup), server {}",
        manifest.created_at.format("%Y-%m-%d %H:%M UTC"),
        manifest.kind.as_str(),
        manifest.server_version.as_deref().unwrap_or("unknown")
    ));
    let image = host
        .image_version()
        .await
        .map_err(|e| format!("cannot tell the server image's version: {e}"))?;
    if let Some(taken) = &manifest.server_version {
        let taken: Version = taken
            .parse()
            .map_err(|e| format!("the backup names version `{taken}`: {e}"))?;
        if taken > image {
            return Err(format!(
                "this backup comes from MonitorFlow {taken} and the server image is {image}, \
                 which cannot read its database: install {taken} or later, then restore"
            ));
        }
    }
    if manifest.serverdata && !serverdata_dir.is_dir() {
        return Err(format!(
            "{} is not mounted: take deploy/docker-compose.yml from the release",
            serverdata_dir.display()
        ));
    }
    // Before anything stops: a refused backup leaves the deployment as
    // it was.
    let checked = if manifest.serverdata {
        Some(check_serverdata(archive, owner).await?)
    } else {
        None
    };

    say("stopping the server and the web interface");
    host.stop_server().await.map_err(|e| e.to_string())?;
    say("restoring the database");
    host.restore_database(&work.join(DATABASE))
        .await
        .map_err(|e| format!("database restore failed: {e}"))?;
    if let Some(checked) = checked {
        say("restoring the server data");
        // The database is restored already: what cannot be read is lost
        // with a warning, the restore goes on.
        let retired = retired_ca::read(serverdata_dir).unwrap_or_else(|e| {
            say(&format!(
                "warning: cannot read the retired certificate authorities: {e}"
            ));
            retired_ca::Retired::default()
        });
        extract_serverdata(checked, serverdata_dir).await?;
        match retired_ca::put_back(serverdata_dir, &retired) {
            Ok(Some(warning)) => say(&format!("warning: {warning}")),
            Ok(None) => {}
            Err(e) => say(&format!(
                "warning: cannot keep the retired certificate authorities: {e}"
            )),
        }
    } else {
        say("the backup has no server data: the current one is kept");
    }
    say("starting MonitorFlow");
    host.start_all().await.map_err(|e| e.to_string())?;
    say(&format!("restored; server {image} runs"));
    Ok(manifest)
}
