//! SNMP checks of devices (ADR-0149), run from a probe (ADR-0148).
//!
//! One kind, `snmp`, whose `what` parameter picks a group of the common
//! MIBs; each group is one [`Sensor`] that polls what it needs and turns
//! it into samples, named after the configured check (`snmp_if` gives
//! `snmp_if_in_bytes_per_second`):
//!
//! | `what` | MIB | Metrics, by instance |
//! | --- | --- | --- |
//! | `system` | SNMPv2-MIB | `up`, `uptime_seconds` |
//! | `interfaces` | IF-MIB, 64-bit counters when there are | per interface: `oper_up`, `admin_up`, `if_index` (its ifIndex, which names it in traffic flows), `in_bytes_per_second`, `out_bytes_per_second`, `in_errors_per_second`, `out_errors_per_second`, `in_discards_per_second`, `out_discards_per_second`, `speed_bits_per_second`, `in_utilization_ratio`, `out_utilization_ratio` |
//! | `host` | HOST-RESOURCES-MIB | `cpu_usage_ratio`, `memory_used_ratio`; per storage: `storage_used_ratio`, `storage_used_bytes`, `storage_size_bytes` |
//! | `ups` | UPS-MIB | `battery_charge_ratio`, `runtime_seconds`, `on_battery`, `battery_low` |
//! | `temperature` | ENTITY-SENSOR-MIB | per sensor: `celsius` |
//! | `printer` | Printer-MIB | per supply: `supply_level_ratio` |
//! | `detect` | all of the above | `answers` (labels `descr`, `name`, `object_id`) and one `has` per group, for "Add a device" |
//!
//! Rates come from the difference between two collections, so the first
//! one reports none. Counters that go back (a reboot, a wrap of a 32-bit
//! counter) give no rate for that collection.
//!
//! Parameters: `host` (required), `port` (161), `version` (`v1`, `v2c` or
//! `v3`; `v1` since ADR-0157), `community` for v1 and v2c; for v3 `user`, `auth_protocol` (`sha`,
//! `sha256`, `sha384`, `sha512`, `md5`) and `auth_password`, and
//! `priv_protocol` (`aes128`, `aes256`, `aes256c` for Cisco's key
//! extension) and `priv_password`. The server keeps the credentials sealed
//! and adds them to the check it sends a probe; [`SECRET_PARAMS`] are
//! never stored with the check nor shown.

pub mod custom;
pub mod library;
pub mod mib;
mod mibs;
pub mod read;
mod sensor;
mod session;

use std::collections::BTreeMap;
use std::sync::Arc;

pub use custom::CUSTOM_KIND;
pub use mibs::{Group, Val, GROUPS};
pub use read::{build_read, parse_read, Read, READ_KIND};
pub use sensor::SnmpSensor;

use monitorflow_sensors::Sensor;

/// The kind of the checks this crate builds.
pub const KIND: &str = "snmp";
/// Parameters that hold secrets.
pub const SECRET_PARAMS: [&str; 3] = ["community", "auth_password", "priv_password"];
/// Parameters of the credentials, secret or not.
pub const CREDENTIAL_PARAMS: [&str; 7] = [
    "version",
    "community",
    "user",
    "auth_protocol",
    "auth_password",
    "priv_protocol",
    "priv_password",
];

/// How to authenticate to a device.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    V1 {
        community: String,
    },
    V2c {
        community: String,
    },
    V3 {
        user: String,
        auth: Option<(AuthProtocol, String)>,
        privacy: Option<(PrivProtocol, String)>,
    },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V1 { .. } => f.write_str("V1 { community: <hidden> }"),
            Self::V2c { .. } => f.write_str("V2c { community: <hidden> }"),
            Self::V3 {
                user,
                auth,
                privacy,
            } => f
                .debug_struct("V3")
                .field("user", user)
                .field("auth", &auth.as_ref().map(|(p, _)| p))
                .field("privacy", &privacy.as_ref().map(|(p, _)| p))
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthProtocol {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl AuthProtocol {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "md5" => Self::Md5,
            "sha" | "sha1" => Self::Sha1,
            "sha256" => Self::Sha256,
            "sha384" => Self::Sha384,
            "sha512" => Self::Sha512,
            _ => return None,
        })
    }

    fn library(self) -> async_snmp::AuthProtocol {
        match self {
            Self::Md5 => async_snmp::AuthProtocol::Md5,
            Self::Sha1 => async_snmp::AuthProtocol::Sha1,
            Self::Sha256 => async_snmp::AuthProtocol::Sha256,
            Self::Sha384 => async_snmp::AuthProtocol::Sha384,
            Self::Sha512 => async_snmp::AuthProtocol::Sha512,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivProtocol {
    Aes128,
    /// AES-256 with the Blumenthal key extension, net-snmp's `AES-256`.
    Aes256,
    /// AES-256 with the Reeder key extension, Cisco's.
    Aes256C,
}

impl PrivProtocol {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "aes" | "aes128" => Self::Aes128,
            "aes256" => Self::Aes256,
            "aes256c" => Self::Aes256C,
            _ => return None,
        })
    }

    fn library(self) -> async_snmp::PrivProtocol {
        match self {
            Self::Aes128 => async_snmp::PrivProtocol::Aes128,
            Self::Aes256 => async_snmp::PrivProtocol::Aes256Blumenthal,
            Self::Aes256C => async_snmp::PrivProtocol::Aes256Reeder,
        }
    }
}

/// What a check polls and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub credentials: Credentials,
    pub group: Group,
}

/// Longest community, user name or password.
const MAX_SECRET: usize = 256;
/// Shortest v3 password, as RFC 3414 implementations require.
const MIN_PASSWORD: usize = 8;

/// Reads and checks the parameters of an `snmp` check. Errors never
/// quote a secret.
pub fn parse(params: &BTreeMap<String, String>) -> Result<Target, String> {
    if let Some(unknown) = params.keys().find(|k| {
        !["host", "port", "what"].contains(&k.as_str()) && !CREDENTIAL_PARAMS.contains(&k.as_str())
    }) {
        return Err(format!("unknown parameter `{}`", unknown.escape_debug()));
    }
    let device = device(params)?;
    let group = match params
        .get("what")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        None => Group::System,
        Some(what) => Group::parse(what).ok_or_else(|| {
            format!(
                "parameter `what` must be one of: {}",
                GROUPS
                    .iter()
                    .map(|g| g.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
    };
    Ok(Target {
        host: device.host,
        port: device.port,
        credentials: device.credentials,
        group,
    })
}

/// A device to poll: where and with what credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub host: String,
    pub port: u16,
    pub credentials: Credentials,
}

/// Reads `host`, `port` and the credentials of any kind of this crate,
/// leaving other parameters to the caller. Errors never quote a secret.
pub fn device(params: &BTreeMap<String, String>) -> Result<Device, String> {
    let get = |name: &str| params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
    let host = get("host").ok_or("parameter `host` is required")?;
    monitorflow_sensors::port::parse_host(host).map_err(|e| format!("parameter `host`: {e}"))?;
    let port = match get("port") {
        None => 161,
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or("parameter `port` must be between 1 and 65535")?,
    };
    let secret = |name: &str| -> Result<Option<String>, String> {
        match params.get(name) {
            None => Ok(None),
            Some(value) if value.is_empty() => Ok(None),
            Some(value) if value.len() > MAX_SECRET || value.chars().any(char::is_control) => Err(
                format!("parameter `{name}` must be at most {MAX_SECRET} bytes without control characters"),
            ),
            Some(value) => Ok(Some(value.clone())),
        }
    };
    let credentials = match get("version").unwrap_or("v2c") {
        "v1" | "1" => Credentials::V1 {
            community: secret("community")?.ok_or("a v1 check needs a `community`")?,
        },
        "v2c" | "2c" | "2" => Credentials::V2c {
            community: secret("community")?.ok_or("a v2c check needs a `community`")?,
        },
        "v3" | "3" => {
            let user = secret("user")?.ok_or("a v3 check needs a `user`")?;
            let password = |name: &str| -> Result<String, String> {
                let value =
                    secret(name)?.ok_or_else(|| format!("parameter `{name}` is required"))?;
                if value.len() < MIN_PASSWORD {
                    return Err(format!(
                        "parameter `{name}` must be at least {MIN_PASSWORD} characters"
                    ));
                }
                Ok(value)
            };
            let auth = match get("auth_protocol") {
                None => None,
                Some(p) => Some((
                    AuthProtocol::parse(p).ok_or(
                        "parameter `auth_protocol` must be `sha`, `sha256`, `sha384`, `sha512` or `md5`",
                    )?,
                    password("auth_password")?,
                )),
            };
            let privacy = match get("priv_protocol") {
                None => None,
                Some(_) if auth.is_none() => {
                    return Err("privacy needs authentication: set `auth_protocol`".into())
                }
                Some(p) => Some((
                    PrivProtocol::parse(p).ok_or(
                        "parameter `priv_protocol` must be `aes128`, `aes256` or `aes256c`",
                    )?,
                    password("priv_password")?,
                )),
            };
            Credentials::V3 {
                user,
                auth,
                privacy,
            }
        }
        _ => return Err("parameter `version` must be `v1`, `v2c` or `v3`".into()),
    };
    Ok(Device {
        host: host.to_owned(),
        port,
        credentials,
    })
}

/// Builds the sensor of an `snmp` check.
pub fn build(params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, String> {
    Ok(Arc::new(SnmpSensor::new(parse(params)?)))
}

#[cfg(test)]
mod tests;
