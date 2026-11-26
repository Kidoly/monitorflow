//! Cloud accounts on the server (ADR-0183): an AWS account or an Azure
//! subscription is a device whose virtual machines are its inventory.
//! Its credentials are sealed and never shown back; its check runs on
//! the central server only, and gets the alerts that come with it.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use monitorflow_probe::ProbeFactory;
use monitorflow_sensors::Sensor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::api::sensors::RunBy;
use crate::secrets::{self, Column};
use crate::vmware::{self, Threshold};

/// How often an account is read: basic EC2 metrics come every 5 minutes.
pub const INTERVAL_MS: i64 = 300_000;
/// Its timeout: a few regions of a large account take a while.
pub const TIMEOUT_MS: i64 = 120_000;

/// Whether `kind` is a cloud account's check.
pub fn is_kind(kind: &str) -> bool {
    monitorflow_cloud::KINDS.contains(&kind)
}

/// Body of a device's cloud credentials. On a change, what is left out
/// is kept, the secret only while the provider and the identity it
/// belongs to (access key ID, or tenant and application) stay the same.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
#[schema(as = CloudCredentialsRequest)]
#[serde(deny_unknown_fields)]
pub struct CredentialsRequest {
    /// `aws` or `azure`. Required the first time; it cannot change.
    pub provider: Option<String>,
    /// AWS: the access key ID (`AKIA…`). Azure: the application (client)
    /// ID of the service principal.
    pub key_id: Option<String>,
    /// AWS: the secret access key. Azure: the client secret. Secret,
    /// never returned.
    pub secret: Option<String>,
    /// AWS: the regions to read, like `["eu-west-3", "us-east-1"]`.
    pub regions: Option<Vec<String>>,
    /// Azure: the directory (tenant) ID, or its domain.
    pub tenant: Option<String>,
    /// Azure: the subscription ID.
    pub subscription: Option<String>,
}

/// Credentials checked and ready to store or use.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub provider: String,
    pub key_id: String,
    pub secret: String,
    pub regions: Vec<String>,
    pub tenant: Option<String>,
    pub subscription: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("provider", &self.provider)
            .field("key_id", &self.key_id)
            .field("regions", &self.regions)
            .field("tenant", &self.tenant)
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// The check parameters they stand for.
    pub fn params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::from([
            ("key_id".to_string(), self.key_id.clone()),
            ("secret".to_string(), self.secret.clone()),
        ]);
        if self.provider == monitorflow_cloud::AWS {
            params.insert("regions".into(), self.regions.join(","));
        }
        if let Some(tenant) = &self.tenant {
            params.insert("tenant".into(), tenant.clone());
        }
        if let Some(subscription) = &self.subscription {
            params.insert("subscription".into(), subscription.clone());
        }
        params
    }

    /// What is stored beside the secret.
    fn settings(&self) -> Value {
        json!({
            "key_id": self.key_id, "regions": self.regions,
            "tenant": self.tenant, "subscription": self.subscription,
        })
    }

    /// Where the device's address points: the provider's API.
    pub fn address(&self) -> String {
        match self.regions.first() {
            Some(region) if self.provider == monitorflow_cloud::AWS => {
                format!("ec2.{region}.amazonaws.com")
            }
            _ => "management.azure.com".into(),
        }
    }
}

/// Checks `request`, the stored `before` filling what it leaves out. The
/// error is for the client and quotes no secret.
pub fn check(
    request: &CredentialsRequest,
    before: Option<&Credentials>,
) -> Result<Credentials, String> {
    let given = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let provider = match (given(&request.provider), before) {
        (Some(p), Some(b)) if p != b.provider => {
            return Err("`provider` cannot change: remove the credentials first".into())
        }
        (Some(p), _) => p,
        (None, Some(b)) => b.provider.clone(),
        (None, None) => return Err("`provider` is required: `aws` or `azure`".into()),
    };
    if !is_kind(&provider) {
        return Err("`provider` must be `aws` or `azure`".into());
    }
    let aws = provider == monitorflow_cloud::AWS;
    let key_id = given(&request.key_id)
        .or_else(|| before.map(|b| b.key_id.clone()))
        .ok_or("`key_id` is required")?;
    let tenant = if aws {
        None
    } else {
        Some(
            given(&request.tenant)
                .or_else(|| before.and_then(|b| b.tenant.clone()))
                .ok_or("`tenant` is required")?
                .to_ascii_lowercase(),
        )
    };
    let subscription = if aws {
        None
    } else {
        Some(
            given(&request.subscription)
                .or_else(|| before.and_then(|b| b.subscription.clone()))
                .ok_or("`subscription` is required")?
                .to_ascii_lowercase(),
        )
    };
    let regions = if aws {
        match &request.regions {
            Some(regions) => regions.iter().map(|r| r.trim().to_owned()).collect(),
            None => before.map(|b| b.regions.clone()).unwrap_or_default(),
        }
    } else {
        Vec::new()
    };
    // A kept secret never goes to another identity.
    let same_identity =
        before.is_some_and(|b| b.key_id.eq_ignore_ascii_case(&key_id) && b.tenant == tenant);
    let secret = request
        .secret
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| before.filter(|_| same_identity).map(|b| b.secret.clone()))
        .ok_or("`secret` is required")?;
    let mut credentials = Credentials {
        provider,
        key_id,
        secret,
        regions,
        tenant,
        subscription,
    };
    let target = monitorflow_cloud::parse(&credentials.provider, &credentials.params())?;
    // What the check reads, in its own form.
    match target.account {
        monitorflow_cloud::Account::Aws {
            key_id, regions, ..
        } => {
            credentials.key_id = key_id;
            credentials.regions = regions;
        }
        monitorflow_cloud::Account::Azure {
            tenant,
            client_id,
            subscription,
            ..
        } => {
            credentials.key_id = client_id;
            credentials.tenant = Some(tenant);
            credentials.subscription = Some(subscription);
        }
    }
    credentials.secret = credentials.secret.trim().to_owned();
    Ok(credentials)
}

/// What the API shows of a device's cloud credentials: never the secret.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = CloudCredentials)]
pub struct View {
    /// `aws` or `azure`.
    pub provider: String,
    /// The access key ID, or the application (client) ID.
    pub key_id: String,
    /// AWS: the regions read; empty for Azure.
    pub regions: Vec<String>,
    /// Azure: the tenant; `null` for AWS.
    pub tenant: Option<String>,
    /// Azure: the subscription; `null` for AWS.
    pub subscription: Option<String>,
    pub secret_set: bool,
    pub updated_at: DateTime<Utc>,
}

type Stored = (String, Value, String, DateTime<Utc>);

const SELECT: &str = "SELECT provider, settings, secret, updated_at
                      FROM cloud_credentials WHERE server_id = $1";

fn read(provider: String, settings: &Value, secret: String) -> Credentials {
    let text = |k: &str| settings[k].as_str().map(str::to_owned);
    Credentials {
        provider,
        key_id: text("key_id").unwrap_or_default(),
        secret,
        regions: settings["regions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_owned))
            .collect(),
        tenant: text("tenant"),
        subscription: text("subscription"),
    }
}

/// The credentials of `server`, opened; `Err` when they do not open.
pub async fn load(
    db: &mut PgConnection,
    server: Uuid,
) -> Result<Option<Result<Credentials, String>>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(row.map(|(provider, settings, sealed, _)| {
        let secret = secrets::open(Column::CloudCredentials, &sealed)
            .map_err(|e| e.to_string())?
            .expose()
            .to_owned();
        Ok(read(provider, &settings, secret))
    }))
}

/// What the API shows of the credentials of `server`.
pub async fn view(db: &mut PgConnection, server: Uuid) -> Result<Option<View>, sqlx::Error> {
    let row: Option<Stored> = sqlx::query_as(SELECT)
        .bind(server)
        .fetch_optional(&mut *db)
        .await?;
    Ok(row.map(|(provider, settings, sealed, updated_at)| {
        let c = read(provider, &settings, String::new());
        View {
            provider: c.provider,
            key_id: c.key_id,
            regions: c.regions,
            tenant: c.tenant,
            subscription: c.subscription,
            secret_set: !sealed.is_empty(),
            updated_at,
        }
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot seal the cloud secret: {0}")]
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
    let sealed = secrets::seal(Column::CloudCredentials, &credentials.secret)
        .map_err(|e| SaveError::Seal(e.to_string()))?;
    sqlx::query(
        "INSERT INTO cloud_credentials (server_id, provider, settings, secret)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (server_id) DO UPDATE SET provider = $2, settings = $3, secret = $4,
             updated_at = now()",
    )
    .bind(server)
    .bind(&credentials.provider)
    .bind(credentials.settings())
    .bind(sealed)
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// What the audit log keeps of credentials: never the secret.
pub fn audit(credentials: &Credentials) -> Value {
    let mut out = credentials.settings();
    out["provider"] = json!(credentials.provider);
    out
}

/// The account's API stops answering, or a call is refused, for two
/// collections; and a machine's status check fails for two.
const fn rules(up: &'static str, failed: &'static str) -> [Threshold; 2] {
    [
        Threshold {
            metric: up,
            comparison: "<",
            warning: None,
            critical: Some(1.0),
            duration_s: 600,
        },
        Threshold {
            metric: failed,
            comparison: ">",
            warning: None,
            critical: Some(0.0),
            duration_s: 600,
        },
    ]
}

/// What an AWS account gets.
pub const AWS_THRESHOLDS: [Threshold; 2] = rules(
    "monitorflow_aws_up",
    "monitorflow_aws_vm_status_checks_failed",
);

/// What an Azure subscription gets, Azure's availability standing for
/// the status checks.
pub const AZURE_THRESHOLDS: [Threshold; 2] = rules(
    "monitorflow_azure_up",
    "monitorflow_azure_vm_status_checks_failed",
);

/// The alerts of `provider`.
pub fn thresholds(provider: &str) -> &'static [Threshold] {
    if provider == monitorflow_cloud::AWS {
        &AWS_THRESHOLDS
    } else {
        &AZURE_THRESHOLDS
    }
}

/// The central server, which alone runs cloud checks.
pub fn central() -> RunBy {
    RunBy {
        probe: Some("central"),
        key: monitorflow_probe::CENTRAL.into(),
        ..RunBy::default()
    }
}

/// Adds the check of `provider` to `server`, run by the central server,
/// and its alerts; what the server has already stays.
pub async fn add_check(
    db: &mut PgConnection,
    server: Uuid,
    provider: &str,
) -> Result<bool, sqlx::Error> {
    let added = sqlx::query(
        "INSERT INTO sensor_configs (server_id, kind, name, instance, interval_ms, timeout_ms, params, probe, probe_relay_id)
         VALUES ($1, $2, $2, $3, $4, $5, '{}', 'central', NULL)
         ON CONFLICT ON CONSTRAINT sensor_configs_key DO NOTHING",
    )
    .bind(server)
    .bind(provider)
    .bind(monitorflow_probe::CENTRAL)
    .bind(INTERVAL_MS)
    .bind(TIMEOUT_MS)
    .execute(&mut *db)
    .await?
    .rows_affected()
        > 0;
    vmware::add_thresholds(db, server, thresholds(provider)).await?;
    Ok(added)
}

/// The central server's probe kinds: those of
/// [`monitorflow_webcheck::CentralProbeKinds`] and the cloud accounts,
/// which relays never run.
#[derive(Debug, Default, Clone, Copy)]
pub struct CentralKinds;

impl ProbeFactory for CentralKinds {
    fn build(
        &self,
        kind: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Arc<dyn Sensor>, String> {
        if is_kind(kind) {
            monitorflow_cloud::build(kind, params)
        } else {
            monitorflow_webcheck::CentralProbeKinds.build(kind, params)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

    fn aws() -> Credentials {
        check(
            &CredentialsRequest {
                provider: Some("aws".into()),
                key_id: Some(" AKIAIOSFODNN7EXAMPLE ".into()),
                secret: Some("a secret key".into()),
                regions: Some(vec!["eu-west-3".into(), " us-east-1".into()]),
                ..CredentialsRequest::default()
            },
            None,
        )
        .unwrap()
    }

    #[test]
    fn credentials_are_checked_and_keep_their_secret() {
        let first = aws();
        assert_eq!(first.key_id, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(first.regions, ["eu-west-3", "us-east-1"]);
        assert_eq!(first.address(), "ec2.eu-west-3.amazonaws.com");
        assert!(!format!("{first:?}").contains("a secret key"));
        assert!(!audit(&first).to_string().contains("a secret key"));
        assert_eq!(audit(&first)["provider"], "aws");
        // A change of regions keeps the secret.
        let again = check(
            &CredentialsRequest {
                regions: Some(vec!["eu-central-1".into()]),
                ..CredentialsRequest::default()
            },
            Some(&first),
        )
        .unwrap();
        assert_eq!(again.secret, "a secret key");
        assert_eq!(again.regions, ["eu-central-1"]);
        // Another key needs its own secret.
        let other = CredentialsRequest {
            key_id: Some("AKIAOTHERKEYEXAMPLE0".into()),
            ..CredentialsRequest::default()
        };
        assert_eq!(
            check(&other, Some(&first)).unwrap_err(),
            "`secret` is required"
        );
        let moved = CredentialsRequest {
            provider: Some("azure".into()),
            ..CredentialsRequest::default()
        };
        assert!(check(&moved, Some(&first))
            .unwrap_err()
            .contains("cannot change"));
        for bad in [
            CredentialsRequest::default(),
            CredentialsRequest {
                provider: Some("gcp".into()),
                ..CredentialsRequest::default()
            },
            CredentialsRequest {
                provider: Some("aws".into()),
                key_id: Some("AKIAIOSFODNN7EXAMPLE".into()),
                secret: Some("bad\nsecret".into()),
                regions: Some(vec!["eu-west-3".into()]),
                ..CredentialsRequest::default()
            },
            CredentialsRequest {
                provider: Some("aws".into()),
                key_id: Some("AKIAIOSFODNN7EXAMPLE".into()),
                secret: Some("s".into()),
                regions: Some(vec![]),
                ..CredentialsRequest::default()
            },
        ] {
            let error = check(&bad, None).unwrap_err();
            assert!(!error.contains("bad\nsecret"), "{error}");
        }
    }

    #[test]
    fn azure_credentials_name_a_tenant_and_a_subscription() {
        let request = CredentialsRequest {
            provider: Some("azure".into()),
            key_id: Some(GUID.to_uppercase()),
            secret: Some("client secret".into()),
            tenant: Some(GUID.into()),
            subscription: Some(GUID.into()),
            regions: Some(vec!["ignored".into()]),
        };
        let first = check(&request, None).unwrap();
        assert_eq!(first.key_id, GUID);
        assert!(first.regions.is_empty());
        assert_eq!(first.address(), "management.azure.com");
        let params = first.params();
        assert!(monitorflow_cloud::parse("azure", &params).is_ok());
        let stored = read(
            first.provider.clone(),
            &first.settings(),
            first.secret.clone(),
        );
        assert_eq!(stored, first);
        // Another tenant needs the secret again.
        let moved = CredentialsRequest {
            tenant: Some("contoso.onmicrosoft.com".into()),
            ..CredentialsRequest::default()
        };
        assert!(check(&moved, Some(&first)).is_err());
    }

    #[test]
    fn default_alerts_name_the_checks_metrics() {
        for (provider, rules) in [("aws", &AWS_THRESHOLDS), ("azure", &AZURE_THRESHOLDS)] {
            for t in rules {
                assert!(
                    t.metric.starts_with(&format!("monitorflow_{provider}_")),
                    "{}",
                    t.metric
                );
            }
            assert_eq!(rules[0].comparison, "<");
            assert_eq!(rules[1].comparison, ">");
        }
        assert!(CentralKinds.build("azure", &BTreeMap::new()).is_err());
        assert!(CentralKinds
            .build(
                "ping",
                &BTreeMap::from([("host".into(), "192.0.2.1".into())])
            )
            .is_ok());
    }
}
