//! SNMP devices on the server (ADR-0149): their credentials, sealed and
//! never shown back, and the templates that give a kind of device its
//! checks and thresholds.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use monitorflow_snmp::Group;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::secrets::{self, Column};

/// Body of the credentials of a device: `version` and what it needs. On a
/// change, a secret left out keeps the stored one when the version stays.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = SnmpCredentialsRequest)]
#[serde(deny_unknown_fields)]
pub struct CredentialsRequest {
    /// `v2c` or `v3`.
    #[schema(example = "v3")]
    pub version: String,
    /// 161 by default.
    pub port: Option<u16>,
    /// v2c: the community. Secret.
    pub community: Option<String>,
    /// v3: the user.
    pub user: Option<String>,
    /// v3: `sha`, `sha256`, `sha384`, `sha512` or `md5`; absent for no
    /// authentication.
    pub auth_protocol: Option<String>,
    /// v3: at least 8 characters. Secret.
    pub auth_password: Option<String>,
    /// v3: `aes128`, `aes256` or `aes256c` (Cisco's key extension); absent
    /// for no privacy. Needs authentication.
    pub priv_protocol: Option<String>,
    /// v3: at least 8 characters. Secret.
    pub priv_password: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secrets { <hidden> }")
    }
}

/// The sealed part.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Secrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priv_password: Option<String>,
}

/// Credentials checked and ready to store or use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub version: String,
    pub port: u16,
    pub user: Option<String>,
    pub auth_protocol: Option<String>,
    pub priv_protocol: Option<String>,
    pub secrets: Secrets,
}

impl Credentials {
    /// The check parameters they stand for.
    pub fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                params.insert(k.to_owned(), v.clone());
            }
        };
        put("version", &Some(self.version.clone()));
        put("community", &self.secrets.community);
        put("user", &self.user);
        put("auth_protocol", &self.auth_protocol);
        put("auth_password", &self.secrets.auth_password);
        put("priv_protocol", &self.priv_protocol);
        put("priv_password", &self.secrets.priv_password);
        params.insert("port".into(), self.port.to_string());
        params
    }
}

/// Checks `request`, with the stored `before` filling secrets left out
/// when the version is the same. The error is for the client and quotes
/// no secret.
pub fn check(
    request: &CredentialsRequest,
    before: Option<&Credentials>,
) -> Result<Credentials, String> {
    let version = request.version.trim().to_owned();
    if version != "v2c" && version != "v3" {
        return Err("`version` must be `v2c` or `v3`".into());
    }
    let kept = before.filter(|b| b.version == version);
    let secret = |given: &Option<String>, stored: Option<&Option<String>>| {
        given
            .clone()
            .filter(|v| !v.is_empty())
            .or_else(|| stored.and_then(Clone::clone))
    };
    let trimmed = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let credentials = Credentials {
        port: request.port.unwrap_or(161),
        user: if version == "v3" {
            trimmed(&request.user)
        } else {
            None
        },
        auth_protocol: if version == "v3" {
            trimmed(&request.auth_protocol)
        } else {
            None
        },
        priv_protocol: if version == "v3" {
            trimmed(&request.priv_protocol)
        } else {
            None
        },
        secrets: Secrets {
            community: if version == "v2c" {
                secret(&request.community, kept.map(|b| &b.secrets.community))
            } else {
                None
            },
            auth_password: if version == "v3" {
                secret(
                    &request.auth_password,
                    kept.map(|b| &b.secrets.auth_password),
                )
            } else {
                None
            },
            priv_password: if version == "v3" {
                secret(
                    &request.priv_password,
                    kept.map(|b| &b.secrets.priv_password),
                )
            } else {
                None
            },
        },
        version,
    };
    if credentials.port == 0 {
        return Err("`port` must be between 1 and 65535".into());
    }
    let mut params = credentials.params();
    params.insert("host".into(), "192.0.2.1".into());
    monitorflow_snmp::parse(&params)?;
    Ok(credentials)
}

/// What the API shows of a device's credentials: never a secret.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = SnmpCredentials)]
pub struct View {
    pub version: String,
    pub port: u16,
    pub user: Option<String>,
    pub auth_protocol: Option<String>,
    pub priv_protocol: Option<String>,
    /// Whether a community is stored, for v2c.
    pub community_set: bool,
    pub auth_password_set: bool,
    pub priv_password_set: bool,
    pub updated_at: DateTime<Utc>,
}

type Stored = (
    String,
    i32,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    DateTime<Utc>,
);

fn open(row: Stored) -> Result<(Credentials, DateTime<Utc>), String> {
    let (version, port, user, auth_protocol, priv_protocol, sealed, updated_at) = row;
    let secret = secrets::open(Column::SnmpCredentials, &sealed).map_err(|e| e.to_string())?;
    let secrets: Secrets = serde_json::from_str(secret.expose())
        .map_err(|_| "stored SNMP secrets do not read".to_string())?;
    Ok((
        Credentials {
            version,
            port: u16::try_from(port).unwrap_or(161),
            user,
            auth_protocol,
            priv_protocol,
            secrets,
        },
        updated_at,
    ))
}

const SELECT: &str =
    "SELECT version, port, username, auth_protocol, priv_protocol, secret, updated_at
                      FROM snmp_credentials WHERE server_id = $1";

/// The credentials of `server`, opened; `Err` when they do not open.
pub async fn load(
    db: &mut PgConnection,
    server: Uuid,
) -> Result<Option<Result<Credentials, String>>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(row.map(|row| open(row).map(|(c, _)| c)))
}

/// What the API shows of the credentials of `server`.
pub async fn view(db: &mut PgConnection, server: Uuid) -> Result<Option<View>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(row.map(
        |(version, port, user, auth_protocol, priv_protocol, sealed, updated_at)| {
            // Which secrets are set is in the clear columns' shape.
            let secrets = secrets::open(Column::SnmpCredentials, &sealed)
                .ok()
                .and_then(|s| serde_json::from_str::<Secrets>(s.expose()).ok())
                .unwrap_or_default();
            View {
                community_set: secrets.community.is_some(),
                auth_password_set: secrets.auth_password.is_some(),
                priv_password_set: secrets.priv_password.is_some(),
                version,
                port: u16::try_from(port).unwrap_or(161),
                user,
                auth_protocol,
                priv_protocol,
                updated_at,
            }
        },
    ))
}

/// Stores `credentials` for `server`, sealed, replacing any.
pub async fn save(
    db: &mut PgConnection,
    server: Uuid,
    credentials: &Credentials,
) -> Result<(), SaveError> {
    let plain =
        serde_json::to_string(&credentials.secrets).map_err(|e| SaveError::Seal(e.to_string()))?;
    let sealed = secrets::seal(Column::SnmpCredentials, &plain)
        .map_err(|e| SaveError::Seal(e.to_string()))?;
    sqlx::query(
        "INSERT INTO snmp_credentials (server_id, version, port, username, auth_protocol, priv_protocol, secret)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (server_id) DO UPDATE SET version = $2, port = $3, username = $4,
             auth_protocol = $5, priv_protocol = $6, secret = $7, updated_at = now()",
    )
    .bind(server)
    .bind(&credentials.version)
    .bind(i32::from(credentials.port))
    .bind(&credentials.user)
    .bind(&credentials.auth_protocol)
    .bind(&credentials.priv_protocol)
    .bind(sealed)
    .execute(&mut *db)
    .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot seal the SNMP secrets: {0}")]
    Seal(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// What the audit log keeps of credentials: never a secret.
pub fn audit(credentials: &Credentials) -> serde_json::Value {
    json!({
        "version": credentials.version, "port": credentials.port, "user": credentials.user,
        "auth_protocol": credentials.auth_protocol, "priv_protocol": credentials.priv_protocol,
    })
}

/// The name of a group's check.
pub fn check_name(group: Group) -> &'static str {
    match group {
        Group::System => "snmp",
        Group::Interfaces => "snmp_if",
        Group::Host => "snmp_host",
        Group::Ups => "snmp_ups",
        Group::Temperature => "snmp_temp",
        Group::Printer => "snmp_printer",
        Group::Detect => "snmp_detect",
    }
}

/// A threshold a template adds.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, utoipa::ToSchema)]
#[schema(as = SnmpTemplateThreshold)]
pub struct Threshold {
    pub metric: &'static str,
    #[schema(example = ">")]
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

const ANSWERS: Threshold = rule("monitorflow_snmp_up", "<", None, Some(1.0), 2);
const IN_USE: Threshold = rule(
    "monitorflow_snmp_if_in_utilization_ratio",
    ">",
    Some(0.9),
    None,
    10,
);
const OUT_USE: Threshold = rule(
    "monitorflow_snmp_if_out_utilization_ratio",
    ">",
    Some(0.9),
    None,
    10,
);
const IN_ERRORS: Threshold = rule(
    "monitorflow_snmp_if_in_errors_per_second",
    ">",
    Some(1.0),
    None,
    10,
);
const HOT: Threshold = rule(
    "monitorflow_snmp_temp_celsius",
    ">",
    Some(70.0),
    Some(80.0),
    5,
);
const CPU: Threshold = rule(
    "monitorflow_snmp_host_cpu_usage_ratio",
    ">",
    Some(0.9),
    None,
    10,
);
const MEMORY: Threshold = rule(
    "monitorflow_snmp_host_memory_used_ratio",
    ">",
    Some(0.95),
    None,
    10,
);
const DISK: Threshold = rule(
    "monitorflow_snmp_host_storage_used_ratio",
    ">",
    Some(0.85),
    Some(0.95),
    5,
);
const ON_BATTERY: Threshold = rule("monitorflow_snmp_ups_on_battery", ">=", None, Some(1.0), 0);
const BATTERY_LOW: Threshold = rule("monitorflow_snmp_ups_battery_low", ">=", None, Some(1.0), 0);
const RUNTIME: Threshold = rule(
    "monitorflow_snmp_ups_runtime_seconds",
    "<",
    Some(1200.0),
    Some(600.0),
    1,
);
const SUPPLY: Threshold = rule(
    "monitorflow_snmp_printer_supply_level_ratio",
    "<",
    Some(0.1),
    Some(0.02),
    0,
);

/// A kind of device: its groups and thresholds.
#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
#[schema(as = SnmpTemplate)]
pub struct Template {
    /// `switch`, `router`, `firewall`, `printer`, `ups`, `nas` or `generic`.
    pub key: &'static str,
    /// What its checks poll: `system`, `interfaces`, `host`, `ups`,
    /// `temperature`, `printer`.
    #[schema(value_type = Vec<String>)]
    #[serde(serialize_with = "groups")]
    pub groups: &'static [Group],
    pub thresholds: &'static [Threshold],
}

fn groups<S: serde::Serializer>(groups: &&'static [Group], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(groups.iter().map(|g| g.as_str()))
}

use Group::{Host, Interfaces, Printer, System, Temperature, Ups};

pub const TEMPLATES: [Template; 7] = [
    Template {
        key: "switch",
        groups: &[System, Interfaces, Temperature],
        thresholds: &[ANSWERS, IN_USE, OUT_USE, IN_ERRORS, HOT],
    },
    Template {
        key: "router",
        groups: &[System, Interfaces, Host, Temperature],
        thresholds: &[ANSWERS, IN_USE, OUT_USE, IN_ERRORS, CPU, HOT],
    },
    Template {
        key: "firewall",
        groups: &[System, Interfaces, Host, Temperature],
        thresholds: &[ANSWERS, IN_USE, OUT_USE, IN_ERRORS, CPU, MEMORY, HOT],
    },
    Template {
        key: "printer",
        groups: &[System, Printer],
        thresholds: &[ANSWERS, SUPPLY],
    },
    Template {
        key: "ups",
        groups: &[System, Ups],
        thresholds: &[ANSWERS, ON_BATTERY, BATTERY_LOW, RUNTIME],
    },
    Template {
        key: "nas",
        groups: &[System, Interfaces, Host, Temperature],
        thresholds: &[ANSWERS, CPU, MEMORY, DISK, HOT],
    },
    Template {
        key: "generic",
        groups: &[System, Interfaces],
        thresholds: &[ANSWERS, IN_USE, OUT_USE],
    },
];

pub fn template(key: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.key == key)
}

/// The template that fits what a device answered: its description and
/// whether each group answers.
pub fn suggest(descr: &str, has: impl Fn(Group) -> bool) -> &'static str {
    let descr = descr.to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| descr.contains(w));
    if has(Ups) {
        "ups"
    } else if has(Printer) {
        "printer"
    } else if says(&[
        "fortigate",
        "fortios",
        "pfsense",
        "opnsense",
        "palo alto",
        "pan-os",
        "sonicwall",
        "firewall",
        "asa",
    ]) {
        "firewall"
    } else if says(&[
        "routeros",
        "mikrotik",
        "junos",
        "cisco ios",
        "ios xe",
        "router",
        "edgeos",
        "vyos",
    ]) {
        "router"
    } else if says(&[
        "synology",
        "qnap",
        "truenas",
        "freenas",
        "nas",
        "diskstation",
    ]) || has(Host) && has(Interfaces)
    {
        "nas"
    } else if has(Interfaces) {
        "switch"
    } else {
        "generic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v3() -> CredentialsRequest {
        CredentialsRequest {
            version: "v3".into(),
            user: Some(" monitor ".into()),
            auth_protocol: Some("sha256".into()),
            auth_password: Some("auth-password".into()),
            priv_protocol: Some("aes128".into()),
            priv_password: Some("priv-password".into()),
            ..CredentialsRequest::default()
        }
    }

    #[test]
    fn credentials_are_checked_and_keep_their_secrets() {
        let v2c = check(
            &CredentialsRequest {
                version: "v2c".into(),
                community: Some("public".into()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(v2c.params()["community"], "public");
        assert_eq!(v2c.params()["port"], "161");
        let first = check(&v3(), None).unwrap();
        assert_eq!(first.user.as_deref(), Some("monitor"));
        // A change without the passwords keeps them.
        let again = check(
            &CredentialsRequest {
                auth_password: None,
                priv_password: None,
                port: Some(1161),
                ..v3()
            },
            Some(&first),
        )
        .unwrap();
        assert_eq!(again.secrets, first.secrets);
        assert_eq!(again.port, 1161);
        // Moving to v2c drops the v3 secrets and needs a community.
        assert!(check(
            &CredentialsRequest {
                version: "v2c".into(),
                ..Default::default()
            },
            Some(&first)
        )
        .unwrap_err()
        .contains("community"));
        for bad in [
            CredentialsRequest {
                version: "v1".into(),
                ..Default::default()
            },
            CredentialsRequest {
                auth_password: Some("short".into()),
                ..v3()
            },
            CredentialsRequest {
                auth_protocol: Some("sha3".into()),
                ..v3()
            },
            CredentialsRequest { user: None, ..v3() },
        ] {
            let error = check(&bad, None).unwrap_err();
            assert!(
                !error.contains("password\"") && !error.contains("short"),
                "{error}"
            );
        }
        let shown = format!("{first:?}");
        assert!(!shown.contains("auth-password"), "{shown}");
        assert!(audit(&first)
            .to_string()
            .find("password")
            .is_none_or(|_| !audit(&first).to_string().contains("auth-password")));
    }

    #[test]
    fn templates_name_existing_metrics_and_groups() {
        for template in TEMPLATES {
            assert_eq!(template.groups[0], System, "{}", template.key);
            assert_eq!(template.thresholds[0], ANSWERS);
            for threshold in template.thresholds {
                let short = threshold.metric.trim_start_matches("monitorflow_");
                assert!(
                    template
                        .groups
                        .iter()
                        .any(|g| short.starts_with(&format!("{}_", check_name(*g)))),
                    "{}",
                    threshold.metric
                );
            }
        }
        assert!(template("ups").is_some() && template("toaster").is_none());
        let json = serde_json::to_value(TEMPLATES[0]).unwrap();
        assert_eq!(
            json["groups"],
            serde_json::json!(["system", "interfaces", "temperature"])
        );
    }

    #[test]
    fn suggestions_follow_what_answers() {
        let only = |groups: &'static [Group]| move |g: Group| groups.contains(&g);
        assert_eq!(suggest("Test UPS", only(&[Ups, Interfaces])), "ups");
        assert_eq!(suggest("office laser", only(&[Printer, Host])), "printer");
        assert_eq!(
            suggest("FortiGate-60F v7.2", only(&[Interfaces, Host])),
            "firewall"
        );
        assert_eq!(suggest("RouterOS RB4011", only(&[Interfaces])), "router");
        assert_eq!(suggest("Linux nas 6.1", only(&[Interfaces, Host])), "nas");
        assert_eq!(
            suggest(
                "Test 24-port gigabit switch",
                only(&[Interfaces, Temperature])
            ),
            "switch"
        );
        assert_eq!(suggest("Something", only(&[])), "generic");
    }
}
