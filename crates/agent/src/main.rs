//! MonitorFlow agent entry point.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::enroll::{self, EnrollError};
use monitorflow_agent::renew::{Renewal, RenewalSettings};
use monitorflow_agent::sensor_config::{self, Builtin, ConfigFile};
use monitorflow_agent::sensors::Sensors;
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::upload::{Uplink, UploadSettings};
use monitorflow_agent::{os, shutdown};
use monitorflow_common::config::{self, AgentConfig};
use monitorflow_common::logging;
use monitorflow_sensors::{metric_name, registry, Sample};

#[derive(Parser, Debug)]
#[command(name = "monitorflow-agent", version, about = "MonitorFlow agent")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        long,
        env = "MONITORFLOW_CONFIG",
        default_value = "/etc/monitorflow/agent.toml"
    )]
    config: std::path::PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Debug helpers for the built-in sensors.
    #[command(subcommand)]
    Sensors(SensorsCommand),
}

#[derive(Subcommand, Debug)]
enum SensorsCommand {
    /// Collect each sensor twice, one second apart, and print the second
    /// round. Needs no configuration.
    ///
    /// Example: `sensors probe systemd --param units=ssh,cron`.
    Probe {
        /// Sensor kinds to run; when empty, every kind that needs no
        /// parameters.
        kinds: Vec<String>,
        /// A parameter as `name=value`, given to every kind listed.
        #[arg(long = "param", value_name = "NAME=VALUE", value_parser = parse_param)]
        params: Vec<(String, String)>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Sensors(SensorsCommand::Probe { kinds, params })) = cli.command {
        return probe(kinds, params.into_iter().collect()).await;
    }
    let config: AgentConfig = config::load(&cli.config)?;
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting agent");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    // Before enrolling, so an unsupported host never spends a token.
    let host = os::detect().context("cannot start on this host")?;
    tracing::info!(hostname = %host.hostname, os_family = %host.os_family, os_version = %host.os_version, "host detected");

    let (trigger, stop) = shutdown::channel();
    shutdown::on_signals(trigger).context("cannot install signal handlers")?;
    let identity = match enroll::ensure_identity(&config, Backoff::default(), stop.clone()).await {
        Ok(identity) => identity,
        Err(EnrollError::Shutdown) => return Ok(()),
        Err(error) => return Err(error).context("no usable agent identity"),
    };
    tracing::info!(peer = %identity.id, not_after = ?identity.not_after, "identity loaded");
    // The renewal task logs an expired certificate with the steps to enroll
    // again; the stream keeps retrying in case the operator does.
    let (renewal, updates) = Renewal::new(
        RenewalSettings::from_config(&config),
        config.tls_paths(),
        identity.clone(),
    );
    tokio::spawn(renewal.run(stop.clone()));
    let (stream_end, sensors_end) = sensor_config::link();
    let uplink = Uplink::new(UploadSettings::default());
    let sensors = Sensors::new(
        Arc::new(Builtin),
        ConfigFile::in_dir(&config.data_dir),
        uplink.clone(),
    );
    let sensors = tokio::spawn(sensors.run(sensors_end, stop.clone()));
    // The stream only returns on shutdown or a local error.
    let result = Agent::new(StreamSettings::from_config(&config), identity, host)
        .follow(updates)
        .with_sensors(stream_end)
        .with_upload(uplink)
        .run(stop)
        .await
        .context("cannot connect upstream");
    if result.is_ok() {
        let _ = sensors.await;
    }
    result
}

fn parse_param(arg: &str) -> Result<(String, String), String> {
    arg.split_once('=')
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .ok_or_else(|| "expected NAME=VALUE".to_owned())
}

/// One round of samples per sensor kind on stdout, in Prometheus text
/// format. The first round only primes delta-based sensors like `cpu`.
async fn probe(kinds: Vec<String>, params: BTreeMap<String, String>) -> anyhow::Result<()> {
    let sensors = if kinds.is_empty() {
        // Kinds that need parameters, like `systemd`, refuse none.
        registry::KINDS
            .iter()
            .filter_map(|kind| Some((kind.to_string(), registry::build(kind, &params).ok()?)))
            .collect::<Vec<_>>()
    } else {
        kinds
            .into_iter()
            .map(|kind| Ok((kind.clone(), registry::build(&kind, &params)?)))
            .collect::<Result<Vec<_>, registry::BuildError>>()?
    };
    let deadline = || tokio::time::Instant::now() + Duration::from_secs(10);
    for (_, sensor) in &sensors {
        let _ = sensor.collect(deadline()).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    for (kind, sensor) in &sensors {
        match sensor.collect(deadline()).await {
            Ok(samples) => {
                for sample in samples {
                    let sensor = metric_name(kind, &sample.sensor);
                    println!("{}", Sample { sensor, ..sample });
                }
            }
            Err(error) => eprintln!("{kind}: {error}"),
        }
    }
    Ok(())
}
