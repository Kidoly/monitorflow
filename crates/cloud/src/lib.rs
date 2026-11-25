//! Cloud accounts (ADR-0183): the virtual machines of an AWS account or
//! an Azure subscription, their state, status checks and basic metrics,
//! read through the providers' APIs by the central server.
//!
//! Two kinds, `aws` and `azure`. A collection lists the account's
//! machines and writes them as the `vm` samples of
//! `monitorflow_sensors::virt`, so the server's inventory and matching of
//! ADR-0156 take them as they take a vCenter's, plus `up`: 1 when every
//! call worked, 0 with labels `reason` (`connect`, `login`, `permission`,
//! `api`) and `error`. Machines read before a failure are still written.
//!
//! Parameters: for `aws`, `key_id` and `secret` (an access key of a user
//! with read-only rights) and `regions` (comma separated); for `azure`,
//! `tenant`, `key_id` (the application's client ID), `secret` (its client
//! secret) and `subscription`. `what` is `inventory` (the default) or
//! `test`, which signs in and counts the machines, for "Add a device".
//! The server keeps the credentials sealed and adds them to the check it
//! runs; [`SECRET_PARAMS`] are never stored with the check nor shown. No
//! parameter names an endpoint: requests go to the providers' own.

pub mod aws;
pub mod azure;
pub mod http;
pub mod sensor;

use std::collections::BTreeMap;
use std::sync::Arc;

use monitorflow_sensors::virt::Vm;
use monitorflow_sensors::Sensor;

pub use sensor::CloudSensor;

/// The kind of AWS account checks.
pub const AWS: &str = "aws";
/// The kind of Azure subscription checks.
pub const AZURE: &str = "azure";
/// Every kind of this crate.
pub const KINDS: [&str; 2] = [AWS, AZURE];
/// Parameters that hold secrets.
pub const SECRET_PARAMS: [&str; 1] = ["secret"];
/// Parameters of the credentials, secret or not.
pub const CREDENTIAL_PARAMS: [&str; 5] = ["key_id", "secret", "regions", "tenant", "subscription"];
/// Most machines read per account.
pub const MAX_VMS: usize = 2000;
/// Most AWS regions per account.
pub const MAX_REGIONS: usize = 30;
/// Longest key or secret.
const MAX_SECRET: usize = 512;

/// A virtual machine of a cloud account: the shared model, its size and
/// the measures only clouds give.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudVm {
    pub vm: Vm,
    /// Its instance type or size, like `t3.micro` or `Standard_B2s`.
    pub size: Option<String>,
    pub measures: Vec<(&'static str, f64)>,
}

/// What a check does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Inventory,
    Test,
}

/// An account and how to read it.
#[derive(Clone, PartialEq, Eq)]
pub enum Account {
    Aws {
        key_id: String,
        secret: String,
        regions: Vec<String>,
    },
    Azure {
        tenant: String,
        client_id: String,
        secret: String,
        subscription: String,
    },
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aws {
                key_id, regions, ..
            } => f
                .debug_struct("Aws")
                .field("key_id", key_id)
                .field("regions", regions)
                .finish_non_exhaustive(),
            Self::Azure {
                tenant,
                client_id,
                subscription,
                ..
            } => f
                .debug_struct("Azure")
                .field("tenant", tenant)
                .field("client_id", client_id)
                .field("subscription", subscription)
                .finish_non_exhaustive(),
        }
    }
}

/// What a check reads and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub account: Account,
    pub what: What,
}

/// Whether `text` is a tenant: a GUID or a domain like
/// `contoso.onmicrosoft.com`.
fn valid_tenant(text: &str) -> bool {
    azure::valid_id(text)
        || (text.len() <= 253
            && text.contains('.')
            && text.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
}

/// Reads and checks the parameters of a check of `kind`. Errors never
/// quote a secret. A `host` is ignored: the providers' endpoints are
/// fixed.
pub fn parse(kind: &str, params: &BTreeMap<String, String>) -> Result<Target, String> {
    let allowed: &[&str] = match kind {
        AWS => &["key_id", "secret", "regions"],
        AZURE => &["tenant", "key_id", "secret", "subscription"],
        _ => return Err(format!("`{}` is not a cloud kind", kind.escape_debug())),
    };
    if let Some(unknown) = params
        .keys()
        .find(|k| !["host", "what"].contains(&k.as_str()) && !allowed.contains(&k.as_str()))
    {
        return Err(format!("unknown parameter `{}`", unknown.escape_debug()));
    }
    let what = match params.get("what").map(|w| w.trim()) {
        None | Some("" | "inventory") => What::Inventory,
        Some("test") => What::Test,
        Some(_) => return Err("parameter `what` must be `inventory` or `test`".into()),
    };
    let value = |name: &str| -> Result<String, String> {
        match params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            None => Err(format!("parameter `{name}` is required")),
            Some(v) if v.len() > MAX_SECRET || v.chars().any(char::is_control) => Err(format!(
                "parameter `{name}` must be at most {MAX_SECRET} bytes without control characters"
            )),
            Some(v) => Ok(v.to_owned()),
        }
    };
    let account = match kind {
        AWS => {
            let key_id = value("key_id")?;
            if !(16..=128).contains(&key_id.len())
                || !key_id.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err("parameter `key_id` must be an access key ID, like AKIA…".into());
            }
            let mut regions: Vec<String> = Vec::new();
            for region in value("regions")?.split(',').map(str::trim) {
                if !aws::valid_region(region) {
                    return Err(format!(
                        "parameter `regions` must list regions like `eu-west-3`, not `{}`",
                        region.escape_debug()
                    ));
                }
                if !regions.iter().any(|r| r == region) {
                    regions.push(region.to_owned());
                }
            }
            if regions.len() > MAX_REGIONS {
                return Err(format!("at most {MAX_REGIONS} regions"));
            }
            Account::Aws {
                key_id,
                secret: value("secret")?,
                regions,
            }
        }
        _ => {
            let tenant = value("tenant")?.to_ascii_lowercase();
            if !valid_tenant(&tenant) {
                return Err(
                    "parameter `tenant` must be a directory (tenant) ID or its domain".into(),
                );
            }
            let client_id = value("key_id")?.to_ascii_lowercase();
            if !azure::valid_id(&client_id) {
                return Err("parameter `key_id` must be the application (client) ID".into());
            }
            let subscription = value("subscription")?.to_ascii_lowercase();
            if !azure::valid_id(&subscription) {
                return Err("parameter `subscription` must be a subscription ID".into());
            }
            Account::Azure {
                tenant,
                client_id,
                secret: value("secret")?,
                subscription,
            }
        }
    };
    Ok(Target { account, what })
}

/// Builds the sensor of a check of `kind`.
pub fn build(kind: &str, params: &BTreeMap<String, String>) -> Result<Arc<dyn Sensor>, String> {
    let target = parse(kind, params)?;
    Ok(Arc::new(CloudSensor::new(target)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    const KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

    #[test]
    fn aws_parameters_are_checked() {
        let target = parse(
            AWS,
            &params(&[
                ("key_id", KEY),
                ("secret", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
                ("regions", "eu-west-3, us-east-1,eu-west-3"),
                ("host", "ec2.eu-west-3.amazonaws.com"),
            ]),
        )
        .unwrap();
        let Account::Aws { regions, .. } = &target.account else {
            panic!("{target:?}")
        };
        assert_eq!(regions, &["eu-west-3", "us-east-1"]);
        assert_eq!(target.what, What::Inventory);
        assert!(!format!("{target:?}").contains("EXAMPLEKEY"));
        for bad in [
            params(&[("key_id", KEY), ("regions", "eu-west-3")]),
            params(&[
                ("key_id", "short"),
                ("secret", "s"),
                ("regions", "eu-west-3"),
            ]),
            params(&[("key_id", KEY), ("secret", "s"), ("regions", "")]),
            params(&[
                ("key_id", KEY),
                ("secret", "s"),
                ("regions", "evil.example/x"),
            ]),
            params(&[
                ("key_id", KEY),
                ("secret", "a\nsecret"),
                ("regions", "eu-west-3"),
            ]),
            params(&[
                ("key_id", KEY),
                ("secret", "s"),
                ("regions", "eu-west-3"),
                ("endpoint", "x"),
            ]),
            params(&[
                ("key_id", KEY),
                ("secret", "s"),
                ("regions", "eu-west-3"),
                ("what", "all"),
            ]),
        ] {
            let error = parse(AWS, &bad).unwrap_err();
            assert!(!error.contains("a\nsecret"), "{error}");
        }
    }

    #[test]
    fn azure_parameters_are_checked() {
        let target = parse(
            AZURE,
            &params(&[
                ("tenant", "Contoso.onmicrosoft.com"),
                ("key_id", GUID),
                ("secret", "a client secret"),
                ("subscription", &GUID.to_uppercase()),
                ("what", "test"),
            ]),
        )
        .unwrap();
        assert_eq!(target.what, What::Test);
        let Account::Azure {
            tenant,
            subscription,
            ..
        } = &target.account
        else {
            panic!("{target:?}")
        };
        assert_eq!(tenant, "contoso.onmicrosoft.com");
        assert_eq!(subscription, GUID);
        assert!(!format!("{target:?}").contains("client secret"));
        for bad in [
            params(&[("tenant", GUID), ("key_id", GUID), ("subscription", GUID)]),
            params(&[
                ("tenant", "../x"),
                ("key_id", GUID),
                ("secret", "s"),
                ("subscription", GUID),
            ]),
            params(&[
                ("tenant", GUID),
                ("key_id", "app"),
                ("secret", "s"),
                ("subscription", GUID),
            ]),
            params(&[
                ("tenant", GUID),
                ("key_id", GUID),
                ("secret", "s"),
                ("subscription", "x"),
            ]),
            params(&[
                ("tenant", GUID),
                ("key_id", GUID),
                ("secret", "s"),
                ("subscription", GUID),
                ("regions", "x"),
            ]),
        ] {
            assert!(parse(AZURE, &bad).is_err(), "{bad:?}");
        }
        assert!(parse("gcp", &BTreeMap::new()).is_err());
    }
}
