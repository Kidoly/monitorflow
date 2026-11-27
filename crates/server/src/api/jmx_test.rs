//! `POST /api/servers/{id}/jmx/test`: the certificate of a Jolokia over
//! HTTPS, seen from the probe that would run the Java check (ADR-0183),
//! so a person can trust a self-signed one by its fingerprint, as for
//! vCenter (ADR-0156). Jolokia is read, with the password given, only
//! when the certificate is trusted.

use std::collections::BTreeMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::Json;
use monitorflow_common::proto::v1::Sample;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::access::authorize;
use super::auth::CurrentUser;
use super::devices::{check_address, run_once, TestedCertificate};
use super::sensors::{body, parse_id, run_by, Owner};
use super::{ApiError, ApiState};

/// Body of `POST /api/servers/{id}/jmx/test`: the Java check's settings.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JmxTestRequest {
    /// `central` or the ID of a relay of the server's site. Absent: a
    /// device's default probe; a server with an agent must name one, as
    /// the test runs from a probe.
    #[serde(default)]
    pub probe: Option<String>,
    /// Jolokia's address; absent: the server's.
    #[serde(default)]
    pub host: Option<String>,
    /// 8778 when absent.
    #[serde(default)]
    pub port: Option<u16>,
    /// `/jolokia/` when absent.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    /// Sent only once the certificate is trusted.
    #[serde(default)]
    pub password: Option<String>,
    /// The SHA-256 fingerprint to trust, as a previous test showed it.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// What a Jolokia over HTTPS answered.
#[derive(Debug, Serialize, PartialEq, utoipa::ToSchema)]
pub struct JmxTested {
    /// Whether a TLS handshake worked at the address.
    pub answers: bool,
    /// Why not, or why Jolokia could not be read, in words; `null`
    /// otherwise.
    pub error: Option<String>,
    pub certificate: Option<TestedCertificate>,
    /// Whether Jolokia answered the check's reads; `null` when they were
    /// not tried, as the certificate is not trusted yet and the password
    /// is never sent then.
    pub reads: Option<bool>,
    /// The JVM's name and version, after a read.
    pub vm: Option<String>,
    pub version: Option<String>,
    /// `central` or the relay that asked.
    pub probe: String,
}

/// What the samples of a `jmx` test say.
pub fn jmx_tested(samples: &[Sample], probe: &str) -> JmxTested {
    let find = |name: &str| samples.iter().find(|s| s.sensor.ends_with(name));
    let label = |s: Option<&Sample>, name: &str| {
        s.and_then(|s| s.labels.get(name))
            .filter(|v| !v.is_empty())
            .cloned()
    };
    let answers = find("_answers");
    let up = find("_up");
    let answered = answers.is_some_and(|s| s.value >= 1.0);
    let read = up.filter(|u| u.value >= 1.0);
    JmxTested {
        answers: answered,
        error: if answered {
            label(up.filter(|u| u.value < 1.0), "error")
        } else {
            label(answers, "error")
        },
        certificate: find("_certificate_trusted").map(TestedCertificate::of),
        reads: up.map(|u| u.value >= 1.0),
        vm: label(read, "vm"),
        version: label(read, "version"),
        probe: probe.to_owned(),
    }
}

/// `POST /api/servers/{id}/jmx/test`: connects once from the probe that
/// would run the check and shows the certificate; reads Jolokia only when
/// it is trusted. Operators of the server's site, as it sends a password
/// to the probe. Nothing is stored.
pub async fn test_jmx(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<JmxTestRequest>, JsonRejection>,
) -> Result<Json<JmxTested>, ApiError> {
    let id = parse_id(&id)?;
    let request = body(request)?;
    let by = {
        let mut conn = state.pool.acquire().await?;
        let site: Option<Uuid> =
            sqlx::query_scalar("SELECT site_id FROM servers WHERE id = $1 AND revoked_at IS NULL")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        authorize(&user, site, true)?;
        run_by(&mut conn, Owner::Server(id), request.probe.as_deref()).await?
    };
    if by.probe.is_none() {
        return Err(ApiError::Invalid(
            "the test runs from the central server or a relay: set `probe`".into(),
        ));
    }
    let host = request
        .host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .or(by.address.as_deref())
        .ok_or_else(|| ApiError::Invalid("`host` is required: the server has no address".into()))?;
    let mut params = BTreeMap::from([
        ("host".to_owned(), check_address(host)?),
        ("tls".to_owned(), "verify".to_owned()),
        ("what".to_owned(), "test".to_owned()),
    ]);
    let given = [
        ("port", request.port.map(|p| p.to_string())),
        ("path", request.path),
        ("user", request.user.map(|u| u.trim().to_owned())),
        ("password", request.password),
        ("fingerprint", request.fingerprint),
    ];
    for (key, value) in given {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            params.insert(key.to_owned(), value);
        }
    }
    monitorflow_apps::build(
        monitorflow_apps::JMX,
        &params,
        monitorflow_apps::Place::Check,
    )
    .map_err(|e| ApiError::Invalid(e.to_string()))?;
    let mut config = monitorflow_sensors::SensorConfig::new(
        monitorflow_apps::JMX,
        std::time::Duration::from_secs(60),
    );
    config.timeout = Some(std::time::Duration::from_secs(25));
    config.params = params;
    let check = monitorflow_probe::Check {
        device: "test".into(),
        kind: monitorflow_apps::JMX.into(),
        config,
    };
    let samples = run_once(&state, &by, check).await?;
    Ok(Json(jmx_tested(&samples, &by.key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(sensor: &str, value: f64, labels: &[(&str, &str)]) -> Sample {
        Sample {
            sensor: sensor.into(),
            value,
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Sample::default()
        }
    }

    fn certificate(trusted: f64) -> Sample {
        sample(
            "jmx_certificate_trusted",
            trusted,
            &[
                ("fingerprint", "AB:CD"),
                ("by_roots", "false"),
                ("subject", "jvm.example.test"),
                ("not_after", "1900000000"),
            ],
        )
    }

    #[test]
    fn an_untrusted_certificate_is_shown_without_reads() {
        let tested = jmx_tested(
            &[sample("jmx_answers", 1.0, &[]), certificate(0.0)],
            "central",
        );
        assert!(tested.answers);
        assert_eq!(tested.reads, None);
        assert_eq!(tested.error, None);
        let certificate = tested.certificate.unwrap();
        assert_eq!(certificate.fingerprint, "AB:CD");
        assert!(!certificate.trusted && !certificate.by_roots);
        assert_eq!(certificate.not_after.unwrap().timestamp(), 1_900_000_000);
    }

    #[test]
    fn a_trusted_certificate_says_what_was_read() {
        let up = sample(
            "jmx_up",
            1.0,
            &[("vm", "OpenJDK 64-Bit Server VM"), ("version", "21.0.5")],
        );
        let tested = jmx_tested(
            &[sample("jmx_answers", 1.0, &[]), certificate(1.0), up],
            "relay-1",
        );
        assert_eq!(tested.reads, Some(true));
        assert_eq!(tested.version.as_deref(), Some("21.0.5"));
        assert_eq!(tested.probe, "relay-1");
        let refused = sample(
            "jmx_up",
            0.0,
            &[("reason", "login"), ("error", "Jolokia refused the login")],
        );
        let tested = jmx_tested(
            &[sample("jmx_answers", 1.0, &[]), certificate(1.0), refused],
            "central",
        );
        assert_eq!(tested.reads, Some(false));
        assert_eq!(tested.error.as_deref(), Some("Jolokia refused the login"));
        assert_eq!(tested.vm, None);
    }

    #[test]
    fn no_handshake_says_why() {
        let down = sample(
            "jmx_answers",
            0.0,
            &[("reason", "tls"), ("error", "no TLS handshake")],
        );
        let tested = jmx_tested(&[down], "central");
        assert!(!tested.answers);
        assert_eq!(tested.error.as_deref(), Some("no TLS handshake"));
        assert!(tested.certificate.is_none());
    }
}
