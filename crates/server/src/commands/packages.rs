//! Package reports from agents (ADR-0031): the latest one per server in
//! `server_packages`, with `servers.reboot_required` and
//! `servers.packages_reported_at`. Windows agents report their updates
//! with a title, classification, severity and size (ADR-0049).

use monitorflow_common::proto::v1::{Package, PackageReport};
use sqlx::PgPool;

/// Packages kept from one report; a Debian or RHEL host has a few
/// thousand.
pub const MAX_PACKAGES: usize = 50_000;

#[derive(Debug, Clone)]
pub struct PackageStore {
    pool: PgPool,
}

/// Fits the columns of `server_packages`.
fn storable(package: &Package) -> bool {
    let fits = |s: &str, max: usize| s.len() <= max && !s.contains('\0');
    !package.name.is_empty()
        && fits(&package.name, 256)
        && fits(&package.arch, 64)
        && fits(&package.version, 256)
        && fits(&package.upgradable_version, 256)
        && fits(&package.title, 1024)
        && fits(&package.classification, 128)
        && fits(&package.severity, 32)
        && i64::try_from(package.size_bytes).is_ok()
}

impl PackageStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Replaces the inventory of the agent `peer_id` with `report`. False
    /// when nothing changed: no such server, or a report collected no
    /// later than the stored one, as when two arrive out of order. Entries
    /// that do not fit the schema are dropped.
    pub async fn replace(
        &self,
        peer_id: &str,
        report: &PackageReport,
    ) -> Result<bool, sqlx::Error> {
        let collected_at =
            (report.collected_at_unix_ms > 0).then(|| report.collected_at_unix_ms as f64 / 1000.0);
        let mut tx = self.pool.begin().await?;
        let server: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM servers
             WHERE peer_id = $1
               AND (packages_reported_at IS NULL
                    OR packages_reported_at < COALESCE(to_timestamp($2), now()))
             FOR UPDATE",
        )
        .bind(peer_id)
        .bind(collected_at)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(server) = server else {
            return Ok(false);
        };
        let packages: Vec<&Package> = report
            .packages
            .iter()
            .filter(|p| storable(p))
            .take(MAX_PACKAGES)
            .collect();
        let column = |f: fn(&Package) -> &String| -> Vec<&str> {
            packages.iter().map(|p| f(p).as_str()).collect()
        };
        sqlx::query("DELETE FROM server_packages WHERE server_id = $1")
            .bind(server)
            .execute(&mut *tx)
            .await?;
        let sizes: Vec<i64> = packages.iter().map(|p| p.size_bytes as i64).collect();
        let reboots: Vec<bool> = packages.iter().map(|p| p.reboot_required).collect();
        sqlx::query(
            "INSERT INTO server_packages (server_id, name, arch, version, upgradable_version,
                 title, classification, severity, size_bytes, reboot_required)
             SELECT $1, name, arch, version, NULLIF(upgradable, ''), NULLIF(title, ''),
                 NULLIF(classification, ''), NULLIF(severity, ''), NULLIF(size, 0), reboot
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[],
                         $7::text[], $8::text[], $9::bigint[], $10::boolean[])
                 AS p (name, arch, version, upgradable, title, classification, severity,
                       size, reboot)
             ON CONFLICT DO NOTHING",
        )
        .bind(server)
        .bind(column(|p| &p.name))
        .bind(column(|p| &p.arch))
        .bind(column(|p| &p.version))
        .bind(column(|p| &p.upgradable_version))
        .bind(column(|p| &p.title))
        .bind(column(|p| &p.classification))
        .bind(column(|p| &p.severity))
        .bind(sizes)
        .bind(reboots)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE servers SET reboot_required = $2,
                 packages_reported_at = COALESCE(to_timestamp($3), now())
             WHERE id = $1",
        )
        .bind(server)
        .bind(report.reboot_required)
        .bind(collected_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
