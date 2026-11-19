//! Third-party apps from package reports (ADR-0138): the manager statuses
//! in `server_package_managers` and the apps in
//! `server_third_party_packages`, replaced by each report.

use monitorflow_common::proto::v1::{
    package_manager_status::State, PackageManagerStatus, PackageReport, PackageScope,
    ThirdPartyPackage,
};
use monitorflow_common::third_party;
use sqlx::PgConnection;

/// Apps kept per manager from one report.
pub const MAX_APPS_PER_MANAGER: usize = 5_000;

/// Whether `s` fits a text column of `max` bytes: PostgreSQL text takes
/// no NUL.
pub(super) fn fits(s: &str, max: usize) -> bool {
    s.len() <= max && !s.contains('\0')
}

/// The statuses of `report` the server knows, as (manager, state,
/// version, error), each manager once.
fn statuses(report: &PackageReport) -> Vec<(&'static str, &'static str, &str, &str)> {
    let mut out: Vec<(&'static str, &'static str, &str, &str)> = Vec::new();
    for status in &report.managers {
        let Some(manager) = third_party::key(status.manager()) else {
            continue;
        };
        let Some(state) = state_key(status) else {
            continue;
        };
        if out.iter().any(|s| s.0 == manager) {
            continue;
        }
        let version = if fits(&status.version, 128) {
            status.version.as_str()
        } else {
            ""
        };
        let error = if fits(&status.error, 1024) {
            status.error.as_str()
        } else {
            ""
        };
        out.push((manager, state, version, error));
    }
    out
}

fn state_key(status: &PackageManagerStatus) -> Option<&'static str> {
    match status.state() {
        State::Available => Some("available"),
        State::Absent => Some("absent"),
        State::Unusable => Some("unusable"),
        State::Unspecified => None,
    }
}

fn scope_key(app: &ThirdPartyPackage) -> Option<&'static str> {
    match app.scope() {
        PackageScope::Machine => Some("machine"),
        PackageScope::User => Some("user"),
        PackageScope::Unspecified => None,
    }
}

/// The apps of `report` that fit the table, for managers reported
/// `available`, at most [`MAX_APPS_PER_MANAGER`] each, pending ones first.
fn storable<'a>(
    report: &'a PackageReport,
    available: &[&'static str],
) -> Vec<(&'static str, &'static str, &'a ThirdPartyPackage)> {
    let mut apps: Vec<_> = report
        .third_party
        .iter()
        .filter_map(|app| {
            let manager = third_party::key(app.manager())?;
            let scope = scope_key(app)?;
            let ok = available.contains(&manager)
                && !app.id.is_empty()
                && fits(&app.id, 512)
                && fits(&app.name, 512)
                && fits(&app.version, 256)
                && fits(&app.available_version, 256)
                && fits(&app.origin, 256);
            ok.then_some((manager, scope, app))
        })
        .collect();
    apps.sort_by_key(|(manager, _, app)| (*manager, app.available_version.is_empty()));
    let mut kept = Vec::with_capacity(apps.len());
    let mut count = 0;
    let mut current = "";
    for app in apps {
        if app.0 != current {
            current = app.0;
            count = 0;
        }
        count += 1;
        if count <= MAX_APPS_PER_MANAGER {
            kept.push(app);
        }
    }
    kept
}

/// Replaces the third-party inventory of `server` with that of `report`,
/// inside the transaction that stores the rest of it.
pub(super) async fn store(
    conn: &mut PgConnection,
    server: uuid::Uuid,
    report: &PackageReport,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM server_third_party_packages WHERE server_id = $1")
        .bind(server)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM server_package_managers WHERE server_id = $1")
        .bind(server)
        .execute(&mut *conn)
        .await?;
    let statuses = statuses(report);
    if statuses.is_empty() {
        return Ok(());
    }
    let column =
        |i: usize| -> Vec<&str> { statuses.iter().map(|s| [s.0, s.1, s.2, s.3][i]).collect() };
    sqlx::query(
        "INSERT INTO server_package_managers (server_id, manager, state, version, error)
         SELECT $1, manager, state, NULLIF(version, ''), NULLIF(error, '')
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[])
             AS m (manager, state, version, error)",
    )
    .bind(server)
    .bind(column(0))
    .bind(column(1))
    .bind(column(2))
    .bind(column(3))
    .execute(&mut *conn)
    .await?;
    let available: Vec<&'static str> = statuses
        .iter()
        .filter(|s| s.1 == "available")
        .map(|s| s.0)
        .collect();
    let apps = storable(report, &available);
    if apps.is_empty() {
        return Ok(());
    }
    let text = |f: fn(&ThirdPartyPackage) -> &String| -> Vec<&str> {
        apps.iter().map(|a| f(a.2).as_str()).collect()
    };
    sqlx::query(
        "INSERT INTO server_third_party_packages (server_id, manager, package_id, scope, name,
             version, available_version, origin)
         SELECT $1, manager, id, scope, NULLIF(name, ''), version, NULLIF(available, ''),
             NULLIF(origin, '')
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[],
                     $8::text[])
             AS a (manager, id, scope, name, version, available, origin)
         ON CONFLICT DO NOTHING",
    )
    .bind(server)
    .bind(apps.iter().map(|a| a.0).collect::<Vec<_>>())
    .bind(text(|a| &a.id))
    .bind(apps.iter().map(|a| a.1).collect::<Vec<_>>())
    .bind(text(|a| &a.name))
    .bind(text(|a| &a.version))
    .bind(text(|a| &a.available_version))
    .bind(text(|a| &a.origin))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::PackageManager;

    use super::*;

    fn app(manager: PackageManager, id: &str, available: &str) -> ThirdPartyPackage {
        ThirdPartyPackage {
            manager: manager.into(),
            id: id.into(),
            version: "1".into(),
            available_version: available.into(),
            scope: PackageScope::Machine.into(),
            ..ThirdPartyPackage::default()
        }
    }

    fn status(manager: i32, state: State) -> PackageManagerStatus {
        PackageManagerStatus {
            manager,
            state: state.into(),
            ..PackageManagerStatus::default()
        }
    }

    #[test]
    fn unknown_and_repeated_statuses_are_left_out() {
        let report = PackageReport {
            managers: vec![
                status(PackageManager::Snap.into(), State::Absent),
                status(PackageManager::Snap.into(), State::Available),
                status(99, State::Available),
                status(PackageManager::Flatpak.into(), State::Unspecified),
            ],
            ..PackageReport::default()
        };
        assert_eq!(statuses(&report), vec![("snap", "absent", "", "")]);
    }

    #[test]
    fn apps_of_managers_not_available_or_unknown_are_left_out() {
        let mut unscoped = app(PackageManager::Snap, "lxd", "");
        unscoped.scope = 0;
        let report = PackageReport {
            third_party: vec![
                app(PackageManager::Snap, "firefox", "132"),
                app(PackageManager::Flatpak, "app/org.a.b/x86_64/stable", ""),
                unscoped,
                app(PackageManager::Snap, "", ""),
                app(PackageManager::Snap, "a\0b", ""),
                ThirdPartyPackage {
                    manager: 42,
                    ..app(PackageManager::Snap, "x", "")
                },
            ],
            ..PackageReport::default()
        };
        let kept = storable(&report, &["snap"]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].2.id, "firefox");
    }

    #[test]
    fn pending_apps_are_kept_first_within_the_cap() {
        let mut apps: Vec<ThirdPartyPackage> = (0..MAX_APPS_PER_MANAGER)
            .map(|i| app(PackageManager::Snap, &format!("snap{i}"), ""))
            .collect();
        apps.push(app(PackageManager::Snap, "pending", "2"));
        apps.push(app(
            PackageManager::Flatpak,
            "app/org.a.b/x86_64/stable",
            "",
        ));
        let report = PackageReport {
            third_party: apps,
            ..PackageReport::default()
        };
        let kept = storable(&report, &["snap", "flatpak"]);
        assert_eq!(kept.len(), MAX_APPS_PER_MANAGER + 1);
        assert!(kept.iter().any(|a| a.2.id == "pending"));
        assert!(kept.iter().any(|a| a.0 == "flatpak"));
    }
}
