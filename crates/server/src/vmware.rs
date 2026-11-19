//! vCenter and ESXi devices on the server (ADR-0156): their credentials,
//! the password sealed and never shown back, and the check and alerts a
//! device gets; and the alerts of "Watch Hyper-V".

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::api::sensors::RunBy;
use crate::secrets::{self, Column};

/// The name of a device's check, and so the prefix of its metrics.
pub const CHECK: &str = "vmware";
/// The name of a Hyper-V host's check.
pub const HYPERV_CHECK: &str = "hyperv";
/// How often both read the whole inventory.
pub const INTERVAL_MS: i64 = 300_000;
/// Their timeout: a large vCenter takes a while to list.
pub const TIMEOUT_MS: i64 = 120_000;

/// Body of a device's vCenter or ESXi credentials. On a change, a
/// password left out keeps the stored one.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = VmwareCredentialsRequest)]
#[serde(deny_unknown_fields)]
pub struct CredentialsRequest {
    /// 443 by default.
    pub port: Option<u16>,
    /// A user with the Read-only role, like `monitor@vsphere.local`.
    /// Required unless kept from the stored credentials.
    pub user: Option<String>,
    /// Its password. Secret.
    pub password: Option<String>,
    /// The SHA-256 fingerprint of a certificate the central server's or
    /// relay's roots do not trust, as `POST /api/devices/vmware/test`
    /// shows it; an empty string forgets it. Absent: kept.
    pub fingerprint: Option<String>,
}

/// Credentials checked and ready to store or use.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub port: u16,
    pub user: String,
    pub password: String,
    /// `AB:CD:…`, upper case.
    pub fingerprint: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("port", &self.port)
            .field("user", &self.user)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// The check parameters they stand for.
    pub fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::from([
            ("port".to_string(), self.port.to_string()),
            ("user".to_string(), self.user.clone()),
            ("password".to_string(), self.password.clone()),
        ]);
        if let Some(print) = &self.fingerprint {
            params.insert("fingerprint".into(), print.clone());
        }
        params
    }
}

/// A fingerprint in its stored form, or why not.
pub fn fingerprint(text: &str) -> Result<String, String> {
    monitorflow_vmware::tls::parse_fingerprint(text)
        .map(|f| monitorflow_vmware::tls::show_fingerprint(&f))
        .ok_or_else(|| "`fingerprint` must be a SHA-256 fingerprint: 64 hexadecimal digits".into())
}

/// Checks `request`, the stored `before` filling what it leaves out. The
/// error is for the client and quotes no secret.
pub fn check(
    request: &CredentialsRequest,
    before: Option<&Credentials>,
) -> Result<Credentials, String> {
    let user = request
        .user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .or_else(|| before.map(|b| b.user.clone()))
        .ok_or("`user` is required")?;
    let password = request
        .password
        .clone()
        .filter(|p| !p.is_empty())
        .or_else(|| before.map(|b| b.password.clone()))
        .ok_or("`password` is required")?;
    let fingerprint = match request.fingerprint.as_deref().map(str::trim) {
        None => before.and_then(|b| b.fingerprint.clone()),
        Some("") => None,
        Some(text) => Some(fingerprint(text)?),
    };
    let credentials = Credentials {
        port: request.port.or(before.map(|b| b.port)).unwrap_or(443),
        user,
        password,
        fingerprint,
    };
    if credentials.port == 0 {
        return Err("`port` must be between 1 and 65535".into());
    }
    let mut params = credentials.params();
    params.insert("host".into(), "192.0.2.1".into());
    monitorflow_vmware::parse(&params)?;
    Ok(credentials)
}

/// What the API shows of a device's credentials: never the password.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = VmwareCredentials)]
pub struct View {
    pub port: u16,
    pub user: String,
    /// The certificate trusted by its fingerprint; `null` when the roots
    /// are trusted.
    pub fingerprint: Option<String>,
    pub password_set: bool,
    pub updated_at: DateTime<Utc>,
}

type Stored = (i32, String, Option<String>, String, DateTime<Utc>);

const SELECT: &str = "SELECT port, username, fingerprint, secret, updated_at
                      FROM vmware_credentials WHERE server_id = $1";

/// The credentials of `server`, opened; `Err` when they do not open.
pub async fn load(
    db: &mut PgConnection,
    server: Uuid,
) -> Result<Option<Result<Credentials, String>>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(row.map(|(port, user, fingerprint, sealed, _)| {
        let password = secrets::open(Column::VmwareCredentials, &sealed)
            .map_err(|e| e.to_string())?
            .expose()
            .to_owned();
        Ok(Credentials {
            port: u16::try_from(port).unwrap_or(443),
            user,
            password,
            fingerprint,
        })
    }))
}

/// What the API shows of the credentials of `server`.
pub async fn view(db: &mut PgConnection, server: Uuid) -> Result<Option<View>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(
        row.map(|(port, user, fingerprint, sealed, updated_at)| View {
            port: u16::try_from(port).unwrap_or(443),
            user,
            fingerprint,
            password_set: !sealed.is_empty(),
            updated_at,
        }),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot seal the vCenter password: {0}")]
    Seal(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Stores `credentials` for `server`, sealed, replacing any.
pub async fn save(
    db: &mut PgConnection,
    server: Uuid,
    credentials: &Credentials,
) -> Result<(), SaveError> {
    let sealed = secrets::seal(Column::VmwareCredentials, &credentials.password)
        .map_err(|e| SaveError::Seal(e.to_string()))?;
    sqlx::query(
        "INSERT INTO vmware_credentials (server_id, port, username, fingerprint, secret)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (server_id) DO UPDATE SET port = $2, username = $3, fingerprint = $4,
             secret = $5, updated_at = now()",
    )
    .bind(server)
    .bind(i32::from(credentials.port))
    .bind(&credentials.user)
    .bind(&credentials.fingerprint)
    .bind(sealed)
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// What the audit log keeps of credentials: never the password.
pub fn audit(credentials: &Credentials) -> serde_json::Value {
    json!({"port": credentials.port, "user": credentials.user, "fingerprint": credentials.fingerprint})
}

/// A threshold that comes by default.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = VirtualizationThreshold)]
pub struct Threshold {
    pub metric: &'static str,
    pub comparison: &'static str,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: i32,
}

const fn rule(
    metric: &'static str,
    comparison: &'static str,
    warning: Option<f64>,
    critical: Option<f64>,
    minutes: i32,
) -> Threshold {
    Threshold {
        metric,
        comparison,
        warning,
        critical,
        duration_s: minutes * 60,
    }
}

/// Seven days, in seconds.
const WEEK: f64 = 7.0 * 86_400.0;

/// What a vCenter or ESXi device gets.
pub const VMWARE_THRESHOLDS: [Threshold; 7] = [
    rule("monitorflow_vmware_up", "<", None, Some(1.0), 2),
    rule("monitorflow_vmware_host_connected", "<", None, Some(1.0), 5),
    rule(
        "monitorflow_vmware_datastore_free_ratio",
        "<",
        Some(0.1),
        Some(0.05),
        10,
    ),
    rule(
        "monitorflow_vmware_vm_snapshot_age_seconds",
        ">",
        Some(WEEK),
        None,
        0,
    ),
    rule(
        "monitorflow_vmware_host_alarms_critical",
        ">",
        Some(0.0),
        None,
        0,
    ),
    rule(
        "monitorflow_vmware_datastore_alarms_critical",
        ">",
        Some(0.0),
        None,
        0,
    ),
    rule(
        "monitorflow_vmware_host_hardware_status",
        ">=",
        Some(1.0),
        Some(2.0),
        5,
    ),
];

/// What "Watch Hyper-V" adds.
pub const HYPERV_THRESHOLDS: [Threshold; 3] = [
    rule("monitorflow_hyperv_up", "<", Some(1.0), None, 15),
    rule(
        "monitorflow_hyperv_vm_snapshot_age_seconds",
        ">",
        Some(WEEK),
        None,
        0,
    ),
    rule(
        "monitorflow_hyperv_vm_replication_health",
        ">=",
        Some(1.0),
        Some(2.0),
        10,
    ),
];

/// Adds a check of `kind` named `name` to `server`, run by `by` (a probe,
/// or its agent with [`RunBy::default`]), and `thresholds`; what the
/// server has already stays.
pub async fn add_check(
    db: &mut PgConnection,
    server: Uuid,
    kind: &str,
    name: &str,
    by: &RunBy,
    thresholds: &[Threshold],
) -> Result<bool, sqlx::Error> {
    let added = sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, timeout_ms, params, probe, probe_relay_id)
         VALUES ($1, $2, $3, $4, $5, $6, '{}', $7, $8)
         ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING",
    )
    .bind(server)
    .bind(kind)
    .bind(name)
    .bind(&by.key)
    .bind(INTERVAL_MS)
    .bind(TIMEOUT_MS)
    .bind(by.probe)
    .bind(by.relay)
    .execute(&mut *db)
    .await?
    .rows_affected()
        > 0;
    add_thresholds(db, server, thresholds).await?;
    Ok(added)
}

/// Adds `thresholds` to `server` where it has none of that metric.
pub async fn add_thresholds(
    db: &mut PgConnection,
    server: Uuid,
    thresholds: &[Threshold],
) -> Result<(), sqlx::Error> {
    for t in thresholds {
        sqlx::query(
            "INSERT INTO thresholds (server_id, metric, comparison, warning, critical, duration_s)
             SELECT $1, $2, $3, $4, $5, $6
             WHERE NOT EXISTS (SELECT 1 FROM thresholds
                               WHERE server_id = $1 AND metric = $2 AND instance IS NULL)",
        )
        .bind(server)
        .bind(t.metric)
        .bind(t.comparison)
        .bind(t.warning)
        .bind(t.critical)
        .bind(t.duration_s)
        .execute(&mut *db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first() -> Credentials {
        check(
            &CredentialsRequest {
                user: Some(" monitor@vsphere.local ".into()),
                password: Some("a password".into()),
                fingerprint: Some("ab".repeat(32)),
                ..CredentialsRequest::default()
            },
            None,
        )
        .unwrap()
    }

    #[test]
    fn credentials_are_checked_and_keep_their_secret() {
        let first = first();
        assert_eq!(first.user, "monitor@vsphere.local");
        assert_eq!(first.port, 443);
        assert!(first.fingerprint.as_deref().unwrap().starts_with("AB:AB:"));
        assert!(!format!("{first:?}").contains("a password"));
        assert!(!audit(&first).to_string().contains("a password"));
        // A change without the password keeps it, and the fingerprint.
        let again = check(
            &CredentialsRequest {
                port: Some(8443),
                ..CredentialsRequest::default()
            },
            Some(&first),
        )
        .unwrap();
        assert_eq!(again.password, "a password");
        assert_eq!(again.fingerprint, first.fingerprint);
        assert_eq!(again.port, 8443);
        let forgotten = check(
            &CredentialsRequest {
                fingerprint: Some(String::new()),
                ..CredentialsRequest::default()
            },
            Some(&first),
        )
        .unwrap();
        assert!(forgotten.fingerprint.is_none());
        for bad in [
            CredentialsRequest::default(),
            CredentialsRequest {
                user: Some("u".into()),
                ..CredentialsRequest::default()
            },
            CredentialsRequest {
                user: Some("u".into()),
                password: Some("bad\npassword".into()),
                ..CredentialsRequest::default()
            },
            CredentialsRequest {
                user: Some("u".into()),
                password: Some("p".into()),
                fingerprint: Some("xyz".into()),
                ..CredentialsRequest::default()
            },
        ] {
            let error = check(&bad, None).unwrap_err();
            assert!(!error.contains("bad\npassword"), "{error}");
        }
        assert!(monitorflow_vmware::parse(&{
            let mut p = first.params();
            p.insert("host".into(), "vc".into());
            p
        })
        .is_ok());
    }

    #[test]
    fn default_alerts_name_the_checks_metrics() {
        for t in VMWARE_THRESHOLDS {
            assert!(t.metric.starts_with("monitorflow_vmware_"), "{}", t.metric);
        }
        for t in HYPERV_THRESHOLDS {
            assert!(t.metric.starts_with("monitorflow_hyperv_"), "{}", t.metric);
        }
        let old = VMWARE_THRESHOLDS
            .iter()
            .find(|t| t.metric.ends_with("snapshot_age_seconds"))
            .unwrap();
        assert_eq!(old.warning, Some(604_800.0));
    }
}
