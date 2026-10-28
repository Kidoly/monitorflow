//! The address hosts reach this server at, in the install commands of the
//! tokens page. The first of `server_names` by default; global
//! administrators pick another name or type an address. Stored in the one
//! row of `server_settings`; the ports are the published ones of the
//! configuration.

use std::net::IpAddr;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgExecutor;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::body;
use super::tokens::EnrollmentSettings;
use super::{ApiError, ApiState};

/// Body of `GET` and `PUT /api/settings/public-address`, and the
/// `address` object of a new token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicAddress {
    /// The address in the install commands.
    pub address: String,
    /// The first server name, used when none is chosen.
    pub default: String,
    /// The names the server's certificate is issued for.
    pub names: Vec<String>,
    /// Whether the certificate is issued for `address`; agents refuse to
    /// connect otherwise.
    pub certified: bool,
    /// `upstream_url` and `enroll_url` at `address`, with the published
    /// ports.
    pub upstream_url: String,
    pub enroll_url: String,
}

/// The view of `stored`, the default address when `None`.
pub fn view(settings: &EnrollmentSettings, stored: Option<&str>) -> PublicAddress {
    let default = settings.default_address().to_owned();
    let address = stored.unwrap_or(&default).to_owned();
    PublicAddress {
        certified: certifies(&settings.server_names, &address),
        upstream_url: settings.upstream_url(&address),
        enroll_url: settings.enroll_url(&address),
        names: settings.server_names.clone(),
        address,
        default,
    }
}

/// Whether a certificate for `names` is valid for `address`, as a TLS
/// client checks it: the same IP address, or the same DNS name ignoring
/// case, `*.` matching one label.
pub fn certifies(names: &[String], address: &str) -> bool {
    if let Ok(ip) = address.parse::<IpAddr>() {
        return names.iter().any(|name| name.parse() == Ok(ip));
    }
    names.iter().any(|name| {
        if name.eq_ignore_ascii_case(address) {
            return true;
        }
        match (name.strip_prefix("*."), address.split_once('.')) {
            (Some(parent), Some((label, rest))) => {
                !label.is_empty() && parent.eq_ignore_ascii_case(rest)
            }
            _ => false,
        }
    })
}

/// A typed address as stored: an IP address in its usual form, or a DNS
/// name in lowercase; `None` when empty. Ports are refused, the commands
/// use the published ones.
pub fn normalize(input: &str) -> Result<Option<String>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let unbracketed = input
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(input);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return Ok(Some(ip.to_string()));
    }
    if input.contains(':') {
        return Err(
            "the address takes no port: the install commands use the server's published ports"
                .into(),
        );
    }
    let name = input.trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if name.len() > 253 || !name.split('.').all(label_ok) {
        return Err(format!(
            "`{input}` is not a DNS name or IP address; use letters, digits, dots and dashes"
        ));
    }
    Ok(Some(name))
}

async fn stored(db: impl PgExecutor<'_>) -> Result<Option<String>, ApiError> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT public_address FROM server_settings")
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// The current address, read from the database.
pub async fn current(
    db: impl PgExecutor<'_>,
    settings: &EnrollmentSettings,
) -> Result<PublicAddress, ApiError> {
    Ok(view(settings, stored(db).await?.as_deref()))
}

fn enrollment(state: &ApiState) -> Result<&EnrollmentSettings, ApiError> {
    state
        .enrollment
        .as_deref()
        .ok_or_else(|| ApiError::Internal("enrollment is not configured".into()))
}

/// `GET /api/settings/public-address`: whoever creates tokens, the
/// administrators of a whole customer.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<PublicAddress>, ApiError> {
    if user.administered_sites() == Scope::Only(Vec::new()) {
        return Err(ApiError::Forbidden);
    }
    Ok(Json(current(&state.pool, enrollment(&state)?).await?))
}

/// Body of `PUT /api/settings/public-address`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRequest {
    /// A DNS name or IP address; `null` or empty goes back to the default.
    pub address: Option<String>,
}

/// `PUT /api/settings/public-address`: global administrators, since the
/// setting is server-wide; audited as `settings.public_address`. An
/// address the certificate does not cover is accepted, and `certified`
/// says so.
pub async fn set(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<SetRequest>, JsonRejection>,
) -> Result<Json<PublicAddress>, ApiError> {
    if user.administered_customers() != Scope::All {
        return Err(ApiError::Forbidden);
    }
    let request = body(request)?;
    let settings = enrollment(&state)?;
    let address = normalize(request.address.as_deref().unwrap_or_default())
        .map_err(ApiError::Invalid)?
        // The default is stored as NULL, so it follows the configuration.
        .filter(|address| address != settings.default_address());
    let mut tx = state.pool.begin().await?;
    // Locks the row, so concurrent changes audit the right old value.
    let old: Option<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT public_address FROM server_settings FOR UPDATE",
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    sqlx::query(
        "INSERT INTO server_settings (singleton, public_address) VALUES (true, $1)
         ON CONFLICT (singleton) DO UPDATE SET public_address = $1",
    )
    .bind(&address)
    .execute(&mut *tx)
    .await?;
    let actor = user.id.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "settings.public_address",
            target: Some(("settings", "public_address")),
            details: json!({ "old": old, "new": address }),
        },
    )
    .await?;
    tx.commit().await?;
    let view = view(settings, address.as_deref());
    tracing::info!(user = %user.id, address = %view.address, certified = view.certified, "changed the public address of install commands");
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(names: &[&str]) -> EnrollmentSettings {
        EnrollmentSettings {
            ca_fingerprint: [0; 32],
            server_names: names.iter().map(|&n| n.into()).collect(),
            grpc_port: 27443,
            enroll_port: 27445,
        }
    }

    #[test]
    fn the_first_name_is_the_default() {
        let view = view(&settings(&["mf.example", "10.0.0.2"]), None);
        assert_eq!(view.address, "mf.example");
        assert_eq!(view.default, "mf.example");
        assert!(view.certified);
        assert_eq!(view.upstream_url, "https://mf.example:27443");
        assert_eq!(view.enroll_url, "https://mf.example:27445");
        let other = super::view(&settings(&["mf.example"]), Some("monitor.lan"));
        assert_eq!(other.address, "monitor.lan");
        assert!(!other.certified);
        assert_eq!(other.upstream_url, "https://monitor.lan:27443");
        let none = super::view(&settings(&[]), None);
        assert_eq!(
            (none.address.as_str(), none.certified),
            ("localhost", false)
        );
    }

    #[test]
    fn certificates_match_as_tls_clients_check_them() {
        let names = ["MF.example".to_owned(), "*.lan".into(), "fd00::1".into()];
        assert!(certifies(&names, "mf.example"));
        assert!(certifies(&names, "monitor.lan"));
        assert!(!certifies(&names, "a.monitor.lan"));
        assert!(!certifies(&names, "lan"));
        assert!(certifies(&names, "fd00:0::1"));
        assert!(!certifies(&names, "10.0.0.2"));
        assert!(!certifies(&names, "other.example"));
    }

    #[test]
    fn addresses_are_normalized_or_refused() {
        assert_eq!(normalize("  "), Ok(None));
        assert_eq!(normalize(" MF.Example. "), Ok(Some("mf.example".into())));
        assert_eq!(normalize("10.0.0.2"), Ok(Some("10.0.0.2".into())));
        assert_eq!(normalize("[fd00:0::1]"), Ok(Some("fd00::1".into())));
        assert!(normalize("mf.example:7443")
            .unwrap_err()
            .contains("no port"));
        for bad in [
            "mf example",
            "mf.example/x",
            "-mf.example",
            "a..b",
            "$(id)",
            "é.example",
        ] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }
}
