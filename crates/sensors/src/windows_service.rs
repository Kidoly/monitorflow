//! State of Windows services, from the Service Control Manager. The
//! analogue of the `systemd` sensor, as its own kind. See ADR-0048.
//!
//! Metrics (prefixed with the configured name), with the service name as
//! instance:
//! - `installed`: 1 when the service exists, so a missing service reports
//!   0 everywhere and can alert;
//! - `active`: 1 when the service is running;
//! - `failed`: 1 when it stopped with an error exit code;
//! - `state`: the SCM state, 1 stopped, 2 start pending, 3 stop pending,
//!   4 running, 5 continue pending, 6 pause pending, 7 paused; 0 when not
//!   installed.
//!
//! Parameter `services` (required): a comma-separated list of service
//! names (the key name, like `W32Time`, not the display name), compared
//! without case like Windows does.
//!
//! The sensor queries all its services once per collection on the
//! blocking pool, bounded by the deadline. Its parameters are checked on
//! every platform so the server can validate them; it only runs on
//! Windows.

use std::collections::BTreeSet;

use tokio::time::Instant;

use crate::registry::{BuildError, Params};
use crate::{Sample, Sensor, SensorError};

/// Most services one sensor may watch.
pub const MAX_SERVICES: usize = 64;
/// Longest service name, `MAX_SERVICE_NAME_LEN` in Windows.
const NAME_MAX: usize = 256;
/// `SERVICE_RUNNING`.
const RUNNING: u32 = 4;
/// `SERVICE_STOPPED`.
const STOPPED: u32 = 1;
/// `ERROR_SERVICE_SPECIFIC_ERROR`: the service's own code says why.
const SPECIFIC_ERROR: u32 = 1066;
/// `ERROR_SERVICE_NEVER_STARTED`.
const NEVER_STARTED: u32 = 1077;

/// Checks a service name: at most 256 bytes of `[A-Za-z0-9 _.$@{}-]`,
/// without leading or trailing space and not starting with `-`. Windows
/// forbids `/` and `\`; the rest of its charset is not used by real
/// services and would only make odd labels.
pub fn service_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX
        && name.trim() == name
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " _.$@{}-".contains(c));
    ok.then_some(()).ok_or_else(|| {
        format!(
            "`{}` is not a service name: at most {NAME_MAX} bytes of [A-Za-z0-9 _.$@{{}}-]",
            name.chars().take(64).collect::<String>().escape_debug()
        )
    })
}

/// What the Service Control Manager says about an installed service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatus {
    /// `dwCurrentState`, 1 to 7.
    pub state: u32,
    /// `dwWin32ExitCode`.
    pub exit_code: u32,
    /// `dwServiceSpecificExitCode`, meaningful when `exit_code` is
    /// `ERROR_SERVICE_SPECIFIC_ERROR`.
    pub specific_exit_code: u32,
}

impl ServiceStatus {
    /// Stopped with an error, not merely stopped or never started.
    pub fn failed(&self) -> bool {
        self.state == STOPPED
            && match self.exit_code {
                0 | NEVER_STARTED => false,
                SPECIFIC_ERROR => self.specific_exit_code != 0,
                _ => true,
            }
    }
}

/// Samples for `service`; `None` when it is not installed.
pub fn samples(service: &str, status: Option<ServiceStatus>) -> Vec<Sample> {
    let sample = |metric: &str, value: f64| Sample::metric(metric, value).with_instance(service);
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    vec![
        sample("installed", flag(status.is_some())),
        sample("active", flag(status.is_some_and(|s| s.state == RUNNING))),
        sample("failed", flag(status.is_some_and(|s| s.failed()))),
        sample("state", status.map_or(0.0, |s| f64::from(s.state))),
    ]
}

/// Queries the state of services, in the order asked; `None` for those
/// that are not installed.
#[async_trait::async_trait]
pub trait Scm: Send + Sync + 'static {
    async fn query(
        &self,
        services: &[String],
        deadline: Instant,
    ) -> Result<Vec<Option<ServiceStatus>>, SensorError>;
}

#[cfg(windows)]
mod scm;
#[cfg(windows)]
pub use scm::Manager;

/// Stands for the Service Control Manager where there is none.
#[cfg(not(windows))]
#[derive(Debug, Default)]
pub struct Manager;

#[cfg(not(windows))]
#[async_trait::async_trait]
impl Scm for Manager {
    async fn query(
        &self,
        _services: &[String],
        _deadline: Instant,
    ) -> Result<Vec<Option<ServiceStatus>>, SensorError> {
        Err(SensorError::Permanent(
            "Windows services can only be watched on Windows".into(),
        ))
    }
}

pub struct ServiceSensor {
    services: Vec<String>,
    scm: Box<dyn Scm>,
}

impl std::fmt::Debug for ServiceSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceSensor")
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

impl ServiceSensor {
    /// Watches `services` (already checked by [`service_name`]) through
    /// `scm`.
    pub fn new(services: Vec<String>, scm: impl Scm) -> Self {
        Self {
            services,
            scm: Box::new(scm),
        }
    }

    pub(crate) fn from_params(params: &mut Params<'_>) -> Result<Self, BuildError> {
        let Some(names) = params.list("services")? else {
            return Err(params.bad("services", "is required"));
        };
        if names.len() > MAX_SERVICES {
            return Err(params.bad(
                "services",
                format!("may list at most {MAX_SERVICES} services"),
            ));
        }
        let mut seen = BTreeSet::new();
        for name in &names {
            service_name(name).map_err(|reason| params.bad("services", reason))?;
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(params.bad("services", format!("lists `{name}` twice")));
            }
        }
        Ok(Self::new(names, Manager))
    }
}

#[async_trait::async_trait]
impl Sensor for ServiceSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let states = self.scm.query(&self.services, deadline).await?;
        if states.len() != self.services.len() {
            return Err(SensorError::Transient(format!(
                "{} service states, {} asked",
                states.len(),
                self.services.len()
            )));
        }
        Ok(self
            .services
            .iter()
            .zip(states)
            .flat_map(|(service, status)| samples(service, status))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn build(services: &str) -> Result<(), BuildError> {
        let params = BTreeMap::from([("services".to_string(), services.to_string())]);
        crate::registry::build("windows_service", &params).map(|_| ())
    }

    #[test]
    fn names_are_checked() {
        for ok in [
            "W32Time",
            "MSSQL$SQLEXPRESS",
            "CDPUserSvc_4a8b1",
            "Dhcp",
            "a b",
            "{1-2}",
        ] {
            assert_eq!(service_name(ok), Ok(()), "{ok}");
        }
        let long = "s".repeat(NAME_MAX + 1);
        for bad in [
            "",
            " sp",
            "sp ",
            "-x",
            "a\\b",
            "a/b",
            "a,b",
            "é",
            "a\n",
            long.as_str(),
        ] {
            assert!(service_name(bad).is_err(), "{bad:?}");
        }
        assert!(build("W32Time, Dhcp").is_ok());
        assert!(build("").is_err());
        let error = build("w32time,W32Time").unwrap_err().to_string();
        assert!(error.contains("twice"), "{error}");
        let many = (0..=MAX_SERVICES)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>();
        assert!(build(&many.join(",")).is_err());
        let error = crate::registry::build("windows_service", &BTreeMap::new())
            .err()
            .unwrap();
        assert!(error.to_string().contains("is required"), "{error}");
    }

    #[test]
    fn failures_need_an_error_code() {
        let status = |state, exit_code, specific_exit_code| ServiceStatus {
            state,
            exit_code,
            specific_exit_code,
        };
        assert!(!status(STOPPED, 0, 0).failed());
        assert!(!status(STOPPED, NEVER_STARTED, 0).failed());
        assert!(!status(STOPPED, SPECIFIC_ERROR, 0).failed());
        assert!(status(STOPPED, SPECIFIC_ERROR, 3).failed());
        assert!(status(STOPPED, 1067, 0).failed());
        assert!(!status(RUNNING, 1067, 0).failed());
    }

    struct Fake(Vec<Option<ServiceStatus>>);

    #[async_trait::async_trait]
    impl Scm for Fake {
        async fn query(
            &self,
            _services: &[String],
            _deadline: Instant,
        ) -> Result<Vec<Option<ServiceStatus>>, SensorError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn samples_per_service() {
        let running = ServiceStatus {
            state: RUNNING,
            exit_code: 0,
            specific_exit_code: 0,
        };
        let sensor = ServiceSensor::new(
            vec!["W32Time".into(), "Gone".into()],
            Fake(vec![Some(running), None]),
        );
        let samples = sensor.collect(Instant::now()).await.unwrap();
        let got: Vec<(&str, &str, f64)> = samples
            .iter()
            .map(|s| (s.instance.as_str(), s.sensor.as_str(), s.value))
            .collect();
        assert_eq!(
            got,
            [
                ("W32Time", "installed", 1.0),
                ("W32Time", "active", 1.0),
                ("W32Time", "failed", 0.0),
                ("W32Time", "state", 4.0),
                ("Gone", "installed", 0.0),
                ("Gone", "active", 0.0),
                ("Gone", "failed", 0.0),
                ("Gone", "state", 0.0),
            ]
        );
        let short = ServiceSensor::new(vec!["a".into(), "b".into()], Fake(vec![None]));
        assert!(!short
            .collect(Instant::now())
            .await
            .unwrap_err()
            .is_permanent());
    }
}
