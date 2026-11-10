//! Check plugins (ADR-0103): sensors of kind `plugin` run a WebAssembly
//! module the server granted this agent, in the sandbox of
//! `monitorflow-plugin`. A plugin runs only when the grant verifies against
//! the agent's certificate authority, the set of grants it came with is
//! not older than the newest this agent has seen, the package verifies
//! against the key it names, and `[plugins]` in `agent.toml` allows it.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use monitorflow_common::config::AgentPluginsConfig;
use monitorflow_common::plugin_grant;
use monitorflow_common::proto::v1::{PluginGrant, PluginGrantSet};
use monitorflow_plugin::http::ReqwestHttp;
use monitorflow_plugin::manifest::PLUGIN_PARAM;
use monitorflow_plugin::package::Package;
use monitorflow_plugin::{Kind, Manifest, Plugin, Runner};
use monitorflow_sensors::{self as sensors, validate, Sample, Sensor, SensorError};
use rustls::pki_types::CertificateDer;
use serde::Deserialize;
use serde_json::json;

use crate::sensor_config::FactoryError;

/// The certificate authority certificates the agent trusts now; they
/// change when the certificate authority does (ADR-0088).
pub type CaCerts = Arc<dyn Fn() -> Vec<CertificateDer<'static>> + Send + Sync>;

/// A granted plugin, ready to run.
struct Granted {
    manifest: Manifest,
    plugin: Arc<Plugin>,
}

/// The check plugins of the current configuration.
pub struct PluginChecks {
    agent_id: String,
    ca_certs: CaCerts,
    config: AgentPluginsConfig,
    runner: Runner,
    /// By name: the plugin, or why it cannot run.
    granted: Mutex<HashMap<String, Result<Arc<Granted>, String>>>,
    /// When the newest signed set of grants seen was made, Unix
    /// milliseconds; 0 before any. No older set is taken.
    newest: Mutex<i64>,
    /// Where `newest` is kept across restarts.
    state: Option<PathBuf>,
}

impl PluginChecks {
    pub fn new(agent_id: &str, ca_certs: CaCerts, config: AgentPluginsConfig) -> Self {
        let http = ReqwestHttp::new(
            ReqwestHttp::default_client(),
            tokio::runtime::Handle::current(),
        );
        Self::with_runner(agent_id, ca_certs, config, Runner::new(Arc::new(http)))
    }

    pub fn with_runner(
        agent_id: &str,
        ca_certs: CaCerts,
        config: AgentPluginsConfig,
        runner: Runner,
    ) -> Self {
        Self {
            agent_id: agent_id.to_owned(),
            ca_certs,
            config,
            runner,
            granted: Mutex::default(),
            newest: Mutex::default(),
            state: None,
        }
    }

    /// Remembers the newest set of grants in `path`, so that a restart
    /// does not make an older one acceptable again.
    pub fn with_state(mut self, path: PathBuf) -> Self {
        let newest = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        self.newest = Mutex::new(newest);
        self.state = Some(path);
        self
    }

    /// Whether `grants` are what the server lets this agent run now. The
    /// server signs them as a set with the time it was made, and no set
    /// older than the newest seen is taken: a relay, which forwards
    /// configurations, could otherwise send an old one again after a
    /// plugin was updated, turned off or removed.
    fn check_set(
        &self,
        grants: &[PluginGrant],
        set: Option<&PluginGrantSet>,
        ca_certs: &[CertificateDer<'_>],
    ) -> Result<(), String> {
        let mut newest = self.newest.lock().expect("not poisoned");
        let Some(set) = set else {
            // A server older than sets signs each grant alone; one that
            // sent a set before never stops.
            return match *newest {
                0 => Ok(()),
                _ => Err("the server's list of plugins is missing".into()),
            };
        };
        plugin_grant::verify_set(set, grants, &self.agent_id, ca_certs)
            .map_err(|e| format!("the server's list of plugins: {e}"))?;
        if set.issued_at_unix_ms < *newest {
            return Err(
                "the server's list of plugins is older than the one this server already has".into(),
            );
        }
        if set.issued_at_unix_ms > *newest {
            *newest = set.issued_at_unix_ms;
            self.remember(set.issued_at_unix_ms);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        if now >= set.expires_at_unix_ms {
            return Err(
                "the server's list of plugins has expired; a new one comes when this server connects"
                    .into(),
            );
        }
        Ok(())
    }

    /// Saves `newest`, off the runtime when there is one.
    fn remember(&self, newest: i64) {
        let Some(path) = self.state.clone() else {
            return;
        };
        let save = move || {
            let saved = monitorflow_common::client::identity::write_atomic(
                &path,
                newest.to_string().as_bytes(),
                0o600,
            );
            if let Err(error) = saved {
                tracing::warn!(%error, "cannot remember the newest list of plugins; it will not survive a restart");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(save)),
            Err(_) => save(),
        }
    }

    /// Replaces the plugins with those `grants` give, checking the set
    /// they come as, then each.
    pub fn prepare(&self, grants: &[PluginGrant], set: Option<&PluginGrantSet>) {
        let ca_certs = (self.ca_certs)();
        let refused = self.check_set(grants, set, &ca_certs).err();
        let granted = grants
            .iter()
            .map(|grant| {
                let checked = match &refused {
                    Some(why) => Err(why.clone()),
                    None => self.check(grant, &ca_certs).map(Arc::new),
                };
                if let Err(error) = &checked {
                    tracing::warn!(plugin = %grant.name, %error, "plugin refused");
                }
                (grant.name.clone(), checked)
            })
            .collect();
        *self.granted.lock().expect("not poisoned") = granted;
    }

    fn check(
        &self,
        grant: &PluginGrant,
        ca_certs: &[CertificateDer<'_>],
    ) -> Result<Granted, String> {
        plugin_grant::verify(grant, &self.agent_id, ca_certs)
            .map_err(|e| format!("the server's grant: {e}"))?;
        let package = Package::parse(&grant.package).map_err(|e| e.to_string())?;
        // The server trusted the signer; this checks the package is what
        // it signed, and keeps to the catalogue when asked to.
        let trusted = match self.config.official_only {
            true => Vec::new(),
            false => vec![package.key().map_err(|e| e.to_string())?],
        };
        let verified = package
            .verify(&grant.package, &trusted)
            .map_err(|e| match e {
                monitorflow_plugin::PackageError::UntrustedKey(_) => {
                    "this server runs only plugins signed by MonitorFlow ([plugins] official_only)"
                        .to_owned()
                }
                other => other.to_string(),
            })?;
        if verified.manifest.name != grant.name || verified.manifest.kind != Kind::Check {
            return Err(format!(
                "{} is not the check plugin it was granted as",
                grant.name
            ));
        }
        let plugin = Plugin::load(&verified.module).map_err(|e| e.to_string())?;
        Ok(Granted {
            manifest: verified.manifest,
            plugin: Arc::new(plugin),
        })
    }

    /// The sensor of a spec of kind `plugin`.
    pub fn build(&self, config: &sensors::SensorConfig) -> Result<Arc<dyn Sensor>, FactoryError> {
        if !self.config.checks {
            return Err(FactoryError::Refused(
                "plugin checks are turned off on this server ([plugins] checks = false in agent.toml)"
                    .into(),
            ));
        }
        let mut settings = config.params.clone();
        let name = settings.remove(PLUGIN_PARAM).unwrap_or_default();
        let granted = self
            .granted
            .lock()
            .expect("not poisoned")
            .get(&name)
            .cloned();
        let granted = match granted {
            Some(Ok(granted)) => granted,
            Some(Err(why)) => return Err(FactoryError::Refused(format!("plugin {name}: {why}"))),
            None => {
                return Err(FactoryError::Refused(format!(
                    "plugin {name} is not granted to this server"
                )))
            }
        };
        let values = granted
            .manifest
            .values(&settings)
            .map_err(|e| FactoryError::Invalid(e.to_string()))?;
        Ok(Arc::new(PluginSensor {
            name,
            granted,
            settings: values.settings,
            instance: config.instance.clone(),
            runner: self.runner.clone(),
        }))
    }
}

/// One check of a plugin.
struct PluginSensor {
    name: String,
    granted: Arc<Granted>,
    settings: BTreeMap<String, String>,
    instance: String,
    runner: Runner,
}

#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    samples: Vec<OutputSample>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputSample {
    metric: String,
    #[serde(default)]
    instance: String,
    value: f64,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

/// Most samples of one run.
const MAX_SAMPLES: usize = 100;

/// The samples a run returned, each checked like any sensor's.
fn samples(output: serde_json::Value) -> Result<Vec<Sample>, String> {
    let output: Output = serde_json::from_value(output).map_err(|e| format!("bad result: {e}"))?;
    if output.samples.len() > MAX_SAMPLES {
        return Err(format!("more than {MAX_SAMPLES} samples"));
    }
    output
        .samples
        .into_iter()
        .map(|s| {
            validate::sensor_name(&s.metric).map_err(|e| e.to_string())?;
            let sample = Sample {
                instance: s.instance,
                labels: s.labels,
                ..Sample::metric(s.metric, s.value)
            };
            validate::sample(&sample).map_err(|e| e.to_string())?;
            Ok(sample)
        })
        .collect()
}

#[async_trait]
impl Sensor for PluginSensor {
    async fn collect(&self, deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        let manifest = &self.granted.manifest;
        let input = json!({ "settings": self.settings, "instance": self.instance });
        let output = self
            .runner
            .run(
                &self.name,
                self.granted.plugin.clone(),
                input,
                manifest.host_rules(&self.settings),
                Vec::new(),
                timeout,
            )
            .await
            .map_err(|e| SensorError::Transient(e.to_string()))?;
        samples(output).map_err(SensorError::Transient)
    }
}

#[cfg(test)]
mod tests;
