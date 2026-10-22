//! MonitorFlow agent entry point.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::{self, Dispatcher, DispatcherSettings, Handlers, Journal};
use monitorflow_agent::enroll::{self, EnrollError};
use monitorflow_agent::packages::sources::{self, SystemCheck};
use monitorflow_agent::packages::update::{self, UpdateSpec};
use monitorflow_agent::packages::{
    self, ApplyUpdatesHandler, Reports, SourcesAction, SourcesHandler, Tools,
};
use monitorflow_agent::renew::{Renewal, RenewalSettings};
use monitorflow_agent::sensor_config::{self, Builtin, ConfigFile};
use monitorflow_agent::sensors::Sensors;
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::upload::{Uplink, UploadSettings};
use monitorflow_agent::{os, shutdown};
use monitorflow_common::config::{self, AgentConfig};
use monitorflow_common::proto::v1::ConfigureSources;
use monitorflow_common::{command, logging, tls};
use monitorflow_sensors::{metric_name, registry, Sample};
use prost::Message;

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
    /// Package source configuration, run as root through sudo by the
    /// agent (ADR-0032). Always works on `/`.
    #[command(subcommand)]
    Sources(SourcesCommand),
    /// Package updates, run as root through sudo by the agent (ADR-0036).
    #[command(subcommand)]
    Packages(PackagesCommand),
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum PackagesCommand {
    /// Read an update spec (JSON, like `{"kind": "named", "packages":
    /// ["openssl"]}`, kind `all`, `security` or `named`) on stdin, check
    /// it, then refresh and upgrade with the host's apt or dnf. Exits with
    /// the package manager's code when it fails.
    Update,
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum SourcesCommand {
    /// Read a `ConfigureSources` message (binary Protobuf) on stdin, point
    /// the package manager at the mirror and check it with `apt-get
    /// update` or `dnf makecache`; reverts on failure.
    Apply,
    /// Restore the sources `apply` changed.
    Revert,
}

#[derive(Subcommand, Debug)]
enum SensorsCommand {
    /// Collect each sensor twice, one second apart, and print the second
    /// round. Needs no configuration; when the agent's configuration file
    /// exists, its `[sensors]` limits apply (script and log directories).
    ///
    /// Examples: `sensors probe systemd --param units=ssh,cron`,
    /// `sensors probe script --param script=check_backup --param args=-w,2`,
    /// `sensors probe log --param path=/var/log/syslog --param pattern=error`
    /// (counts the lines written during the second between the rounds).
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
        let local = probe_limits(&cli.config)?;
        return probe(kinds, params.into_iter().collect(), &local).await;
    }
    if let Some(Command::Sources(command)) = cli.command {
        return tokio::task::spawn_blocking(move || sources(command)).await?;
    }
    if let Some(Command::Packages(PackagesCommand::Update)) = cli.command {
        return packages_update().await;
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
        Arc::new(Builtin::from_config(&config.sensors)),
        ConfigFile::in_dir(&config.data_dir),
        uplink.clone(),
    );
    let sensors = tokio::spawn(sensors.run(sensors_end, stop.clone()));
    let tools = Tools::default();
    let manager = packages::for_family(host.os_family, tools.clone());
    let (reports, latest_report) = Reports::new(manager.clone());
    let report_every = Duration::from_secs(config.packages.report_interval);
    tokio::spawn(reports.clone().run(report_every, stop.clone()));
    let mut handlers = Handlers::default();
    handlers.register(
        command::APPLY_UPDATES,
        Arc::new(ApplyUpdatesHandler::new(manager, reports, tools.clone())),
    );
    for (kind, action) in [
        (command::CONFIGURE_SOURCES, SourcesAction::Apply),
        (command::REVERT_SOURCES, SourcesAction::Revert),
    ] {
        let handler = SourcesHandler::new(action, host.os_family, tools.clone());
        handlers.register(kind, Arc::new(handler));
    }
    let (commands_stream, commands_end) = commands::link();
    let ca_certs =
        tls::load_certs(&config.tls_paths().ca_cert).context("loading the CA certificate")?;
    let dispatcher = Dispatcher::new(
        identity.peer_id(),
        ca_certs,
        handlers,
        Journal::in_dir(&config.data_dir),
        DispatcherSettings::default(),
    );
    tokio::spawn(dispatcher.run(commands_end, stop.clone()));
    // The stream only returns on shutdown or a local error.
    let result = Agent::new(StreamSettings::from_config(&config), identity, host)
        .follow(updates)
        .with_sensors(stream_end)
        .with_upload(uplink)
        .with_commands(commands_stream)
        .with_reports(latest_report)
        .run(stop)
        .await
        .context("cannot connect upstream");
    if result.is_ok() {
        let _ = sensors.await;
    }
    result
}

/// Prints the changed paths and the backup directory, one per line.
fn sources(command: SourcesCommand) -> anyhow::Result<()> {
    let root = std::path::Path::new("/");
    let outcome = match command {
        SourcesCommand::Apply => {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)?;
            let spec = ConfigureSources::decode(bytes.as_slice())
                .context("stdin is not a ConfigureSources message")?;
            sources::apply(&spec, root, &SystemCheck::default())?
        }
        SourcesCommand::Revert => sources::revert(root)?,
    };
    for path in &outcome.changed {
        println!("changed {}", path.display());
    }
    if let Some(dir) = &outcome.backup {
        println!("backup {}", dir.display());
    }
    Ok(())
}

/// Checks the spec before anything else, then prints each run's command
/// line and output, and the error last on stderr.
async fn packages_update() -> anyhow::Result<()> {
    use std::io::Write;
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::new();
    let limit = update::MAX_SPEC_BYTES as u64 + 1;
    tokio::io::stdin()
        .take(limit)
        .read_to_end(&mut bytes)
        .await?;
    let spec = UpdateSpec::parse(&bytes)?;
    let family = os::detect()
        .context("cannot tell the package manager")?
        .os_family;
    let outcome = update::run(&spec, family, &Tools::default()).await;
    let (mut stdout, mut stderr) = (std::io::stdout().lock(), std::io::stderr().lock());
    for run in &outcome.runs {
        writeln!(stdout, "$ {}", run.line)?;
        stdout.write_all(&run.stdout)?;
        stderr.write_all(&run.stderr)?;
    }
    if !outcome.upgraded && outcome.error.is_none() {
        writeln!(stdout, "nothing to upgrade")?;
    }
    stdout.flush()?;
    if let Some(error) = &outcome.error {
        writeln!(stderr, "Error: {error}")?;
        stderr.flush()?;
        std::process::exit(outcome.exit_code());
    }
    Ok(())
}

fn parse_param(arg: &str) -> Result<(String, String), String> {
    arg.split_once('=')
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .ok_or_else(|| "expected NAME=VALUE".to_owned())
}

/// The `[sensors]` limits of the agent's configuration file, or the
/// defaults when there is no file, so probing works before installing.
fn probe_limits(path: &std::path::Path) -> anyhow::Result<registry::Local> {
    if !path.exists() {
        eprintln!(
            "{} not found; using the default sensor limits",
            path.display()
        );
        return Ok(registry::Local::default());
    }
    let config: AgentConfig = config::load(path)?;
    Ok(sensor_config::local(&config.sensors))
}

/// One round of samples per sensor kind on stdout, in Prometheus text
/// format. The first round only primes delta-based sensors like `cpu`.
async fn probe(
    kinds: Vec<String>,
    params: BTreeMap<String, String>,
    local: &registry::Local,
) -> anyhow::Result<()> {
    let build = |kind: &str| registry::build_local(kind, &params, local);
    let sensors = if kinds.is_empty() {
        // Kinds that need parameters, like `systemd`, refuse none.
        registry::KINDS
            .iter()
            .filter_map(|kind| Some((kind.to_string(), build(kind).ok()?)))
            .collect::<Vec<_>>()
    } else {
        kinds
            .into_iter()
            .map(|kind| Ok((kind.clone(), build(&kind)?)))
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
