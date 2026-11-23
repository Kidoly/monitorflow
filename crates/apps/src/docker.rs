//! `docker`: the containers of the local Docker or Podman engine, read
//! from its API on the Unix socket: whether each one runs, its restarts,
//! its health check, and its processor and memory use.
//!
//! Parameters: `engine` (`docker`, the default, for
//! `/var/run/docker.sock`, or `podman` for `/run/podman/podman.sock`: the
//! server never names a socket path), `containers` (a regular expression
//! the names must match, all by default) and `stats` (`true` by default;
//! `false` skips processor and memory, one request per container).
//!
//! A container is watched while it runs, and also when stopped if its
//! restart policy is `always` or `unless-stopped`: one meant to keep
//! running. One-off containers that exited are left out.
//!
//! The socket gives root's power over the machine, so the agent's user
//! has no access to it until a person grants it (ADR-0183); the check only
//! reads (`GET`).

use std::path::PathBuf;
use std::time::Instant;

use monitorflow_sensors::{Sample, Sensor, SensorError};
use regex::Regex;
use serde_json::Value;

use crate::rates::{Rates, Round};
use crate::{down, gauge, label, ratio, within, Failure, Params, Place, Reason};

/// Containers looked at, at most.
const MAX_CONTAINERS: usize = 200;
/// Longest `containers` expression.
const MAX_PATTERN: usize = 256;

#[derive(Debug)]
pub struct Docker {
    socket: PathBuf,
    filter: Option<Regex>,
    stats: bool,
    rates: Rates,
}

impl Docker {
    pub(crate) fn from_params(
        p: &mut Params<'_>,
        place: Place<'_>,
    ) -> Result<Self, monitorflow_sensors::BuildError> {
        let socket = match p.get("engine") {
            None | Some("docker") => "/var/run/docker.sock",
            Some("podman") => "/run/podman/podman.sock",
            Some(_) => return Err(p.bad("engine", "must be `docker` or `podman`")),
        };
        let filter = match p.get("containers").filter(|s| !s.is_empty()) {
            None => None,
            Some(pattern) if pattern.len() > MAX_PATTERN => {
                return Err(p.bad("containers", "must be at most 256 bytes"))
            }
            Some(pattern) => Some(
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| p.bad("containers", format!("is not a regular expression: {e}")))?,
            ),
        };
        let stats = match p.get("stats") {
            None | Some("true") => true,
            Some("false") => false,
            Some(_) => return Err(p.bad("stats", "must be `true` or `false`")),
        };
        match place {
            Place::Agent(_) | Place::Check => {}
            Place::Relay | Place::Central => {
                return Err(p.refused("Docker is read by the server's own agent, not a probe"))
            }
        }
        if !cfg!(unix) && !matches!(place, Place::Check) {
            return Err(monitorflow_sensors::BuildError::Unsupported { kind: crate::DOCKER });
        }
        Ok(Self {
            socket: socket.into(),
            filter,
            stats,
            rates: Rates::default(),
        })
    }

    #[cfg(unix)]
    async fn get(&self, path: &str) -> Result<Value, Failure> {
        let answer = crate::unix_http::get(&self.socket, path)
            .await
            .map_err(|e| self.io_failure(&e))?;
        if answer.status != 200 {
            return Err(Failure::new(
                Reason::Protocol,
                format!("the engine answered {} to {path}", answer.status),
            ));
        }
        serde_json::from_slice(&answer.body)
            .map_err(|e| Failure::new(Reason::Protocol, format!("{path}: {e}")))
    }

    #[cfg(not(unix))]
    async fn get(&self, _path: &str) -> Result<Value, Failure> {
        Err(Failure::new(Reason::Connect, "Docker is read on Linux only"))
    }

    fn io_failure(&self, e: &std::io::Error) -> Failure {
        let socket = self.socket.display();
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => Failure::new(
                Reason::Permission,
                format!(
                    "the agent may not open {socket}: add its user to the docker group (usermod -aG docker monitorflow) and restart the agent"
                ),
            ),
            std::io::ErrorKind::NotFound => {
                Failure::new(Reason::Connect, format!("no engine socket at {socket}"))
            }
            _ => Failure::new(Reason::Connect, format!("{socket}: {e}")),
        }
    }

    async fn read(&self, round: &mut Round) -> Result<Vec<Sample>, Failure> {
        let started = Instant::now();
        let version = self.get("/version").await?;
        let mut out = vec![
            Sample::metric("up", 1.0).with_label(
                "version",
                label(version["Version"].as_str().unwrap_or(""), 64),
            ),
            Sample::metric("response_seconds", started.elapsed().as_secs_f64()),
        ];
        let listed = self.get("/containers/json?all=1").await?;
        let mut running = 0u32;
        let mut stopped = 0u32;
        for listed in listed.as_array().into_iter().flatten().take(MAX_CONTAINERS) {
            let Some(id) = listed["Id"].as_str().filter(|id| valid_id(id)) else {
                continue;
            };
            let name = listed["Names"][0]
                .as_str()
                .unwrap_or(id)
                .trim_start_matches('/');
            if self.filter.as_ref().is_some_and(|f| !f.is_match(name)) {
                continue;
            }
            let inspected = self.get(&format!("/containers/{id}/json")).await?;
            let Some(state) = Container::read(&inspected) else {
                continue;
            };
            if !state.watched() {
                continue;
            }
            if state.running {
                running += 1;
            } else {
                stopped += 1;
            }
            let name = label(name, 128);
            let image = label(listed["Image"].as_str().unwrap_or(""), 128);
            let stats = if self.stats && state.running {
                Some(self.get(&format!("/containers/{id}/stats?stream=false&one-shot=true")).await?)
            } else {
                None
            };
            out.extend(container_samples(&name, &image, id, &state, stats.as_ref(), round));
        }
        out.push(Sample::metric("containers_running", f64::from(running)));
        out.push(Sample::metric("containers_stopped", f64::from(stopped)));
        Ok(out)
    }
}

/// A container ID as the engine gives it: hexadecimal, so it is safe in a
/// path.
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What an inspection says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Container {
    pub status: String,
    pub running: bool,
    pub restart_policy: String,
    pub restarts: f64,
    /// 0 healthy, 1 starting, 2 unhealthy; `None` without a health check.
    pub health: Option<f64>,
}

impl Container {
    pub(crate) fn read(inspected: &Value) -> Option<Self> {
        let state = inspected.get("State")?;
        let status = state["Status"].as_str().unwrap_or("").to_owned();
        let health = match state["Health"]["Status"].as_str() {
            Some("healthy") => Some(0.0),
            Some("starting") => Some(1.0),
            Some("unhealthy") => Some(2.0),
            _ => None,
        };
        Some(Self {
            running: state["Running"].as_bool().unwrap_or(false) && status != "paused",
            status,
            restart_policy: inspected["HostConfig"]["RestartPolicy"]["Name"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            restarts: inspected["RestartCount"].as_f64().unwrap_or(0.0),
            health,
        })
    }

    /// Whether it is meant to run: it runs, or it restarts by itself.
    pub(crate) fn watched(&self) -> bool {
        self.running
            || matches!(self.status.as_str(), "restarting" | "paused")
            || matches!(self.restart_policy.as_str(), "always" | "unless-stopped")
    }
}

/// The samples of one container, named `name`.
pub(crate) fn container_samples(
    name: &str,
    image: &str,
    id: &str,
    state: &Container,
    stats: Option<&Value>,
    round: &mut Round,
) -> Vec<Sample> {
    let mut out = vec![
        Sample::metric("container_running", f64::from(u8::from(state.running)))
            .with_instance(name)
            .with_label("image", image)
            .with_label("state", label(&state.status, 32)),
        Sample::metric("container_restart_count", state.restarts).with_instance(name),
    ];
    // Keyed by ID: a container made again under the same name starts over.
    out.extend(
        round
            .delta(&format!("restarts:{id}"), state.restarts)
            .map(|d| Sample::metric("container_restarts", d).with_instance(name)),
    );
    out.extend(
        state
            .health
            .map(|h| Sample::metric("container_health", h).with_instance(name)),
    );
    if let Some(stats) = stats {
        let memory = &stats["memory_stats"];
        if let Some(usage) = memory["usage"].as_f64() {
            // As `docker stats` counts it: without the page cache the
            // kernel can drop.
            let cache = memory["stats"]["inactive_file"]
                .as_f64()
                .or_else(|| memory["stats"]["total_inactive_file"].as_f64())
                .unwrap_or(0.0);
            let used = (usage - cache).max(0.0);
            out.extend(gauge("container_memory_bytes", used).map(|s| s.with_instance(name)));
            if let Some(limit) = memory["limit"].as_f64() {
                out.extend(
                    ratio(used, limit).map(|r| Sample::metric("container_memory_ratio", r).with_instance(name)),
                );
            }
        }
        let cpu = &stats["cpu_stats"];
        if let (Some(used), Some(system)) = (
            cpu["cpu_usage"]["total_usage"].as_f64(),
            cpu["system_cpu_usage"].as_f64(),
        ) {
            let used = round.delta(&format!("cpu:{id}"), used);
            let system = round.delta(&format!("system:{id}"), system);
            if let (Some(used), Some(system)) = (used, system) {
                // A share of the whole machine, like the server's own
                // `cpu_usage_ratio`.
                out.extend(
                    ratio(used, system).map(|r| Sample::metric("container_cpu_ratio", r).with_instance(name)),
                );
            }
        }
    }
    out
}

#[async_trait::async_trait]
impl Sensor for Docker {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let mut round = self.rates.round();
        match within(deadline, self.read(&mut round)).await {
            Ok(samples) => {
                self.rates.finish(round);
                Ok(samples)
            }
            Err(failure) => {
                self.rates.reset();
                Ok(vec![down(None, &failure)])
            }
        }
    }
}
