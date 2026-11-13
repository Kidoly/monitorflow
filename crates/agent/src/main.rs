//! MonitorFlow agent entry point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::commands::ai_switch::AiSwitch;
use monitorflow_agent::commands::{self, Dispatcher, DispatcherSettings, Handlers, Journal};
use monitorflow_agent::enroll::{self, EnrollError};
use monitorflow_agent::init_config;
use monitorflow_agent::os::HostInfo;
use monitorflow_agent::plugins::PluginChecks;
use monitorflow_agent::renew::{Renewal, RenewalSettings};
#[cfg(unix)]
use monitorflow_agent::self_update;
use monitorflow_agent::self_update::{Connected, StatusResumer};
use monitorflow_agent::sensor_config::{self, Builtin, ConfigFile};
use monitorflow_agent::sensors::Sensors;
use monitorflow_agent::shutdown::Shutdown;
use monitorflow_agent::stream::{Agent, StreamSettings};
use monitorflow_agent::upload::{Uplink, UploadSettings};
use monitorflow_agent::{os, shutdown};
use monitorflow_common::config::{self, AgentConfig};
use monitorflow_common::{logging, tls};
use monitorflow_sensors::{metric_name, registry, Sample};

#[derive(Parser, Debug)]
#[command(name = "monitorflow-agent", version = monitorflow_common::VERSION, about = "MonitorFlow agent")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(long, env = "MONITORFLOW_CONFIG", default_value_os_t = default_config())]
    config: PathBuf,
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
    #[cfg(unix)]
    #[command(subcommand)]
    Sources(SourcesCommand),
    /// Package updates, run as root through sudo by the agent (ADR-0036).
    #[cfg(unix)]
    #[command(subcommand)]
    Packages(PackagesCommand),
    /// Local snapshots of the system volumes on LVM or ZFS, run as root
    /// through sudo by the agent (ADR-0094).
    #[cfg(unix)]
    #[command(subcommand)]
    Snapshots(SnapshotsCommand),
    /// Install a signed agent release, run as root through sudo by the
    /// agent (ADR-0053): reads a header line and the binary on stdin,
    /// verifies them, replaces /usr/bin/monitorflow-agent and starts the
    /// watchdog that restarts the agent and rolls back if it does not
    /// connect.
    #[cfg(unix)]
    SelfUpdate {
        #[command(subcommand)]
        step: Option<SelfUpdateStep>,
    },
    /// Records the certificate authority of the install command as the one
    /// the root helpers trust (ADR-0108). Run as root by the installer,
    /// with the token in MONITORFLOW_ENROLLMENT_TOKEN.
    #[cfg(unix)]
    #[command(hide = true)]
    PinCa,
    /// Run as the Windows service `MonitorFlowAgent`; only the service
    /// control manager starts the agent this way (ADR-0047).
    #[cfg(windows)]
    Service,
    /// Run as the Windows service `MonitorFlowUpdate` under LocalSystem,
    /// installing Windows updates for the agent (ADR-0049); only the
    /// service control manager starts it this way.
    #[cfg(windows)]
    UpdateService,
    /// Let this agent run the commands AI remediation sends, or stop it
    /// (ADR-0084), or let the web interface turn them on (ADR-0098): sets
    /// `commands.shell` or `commands.from_web` in `--config`, keeping the
    /// rest of the file. Restart the MonitorFlowAgent service afterwards.
    #[cfg(windows)]
    AiCommands {
        #[command(subcommand)]
        action: AiCommandsAction,
    },
    /// Check that only administrators and the agent's service own the
    /// folder of `--config` and what it holds, and may write there; the MSI
    /// package runs this before it writes the configuration (ADR-0050).
    #[cfg(windows)]
    CheckDataFolder,
    /// Write the configuration file `--config` with these keys, unless it
    /// already exists; the MSI package runs this (ADR-0050). Empty values
    /// count as unset.
    InitConfig {
        #[arg(long)]
        upstream_url: String,
        #[arg(long, default_value = "")]
        enroll_url: String,
        #[arg(long, default_value = "")]
        enrollment_token: String,
    },
}

#[cfg(windows)]
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum OnOff {
    On,
    Off,
}

#[cfg(windows)]
#[derive(Subcommand, Debug, Clone, Copy)]
enum AiCommandsAction {
    /// Run AI commands here, whatever the web interface last said.
    On,
    /// Refuse AI commands here, whatever the web interface last said.
    Off,
    /// `on` lets the web interface turn AI commands on and off; `off`
    /// lets it only turn them off.
    FromWeb {
        #[arg(value_enum)]
        state: OnOff,
    },
}

#[cfg(unix)]
fn default_config() -> PathBuf {
    "/etc/monitorflow/agent.toml".into()
}

#[cfg(windows)]
fn default_config() -> PathBuf {
    config::windows_base_dir().join("agent.toml")
}

#[cfg(unix)]
#[derive(Subcommand, Debug, Clone, Copy)]
enum PackagesCommand {
    /// Read an update spec (JSON, like `{"kind": "named", "packages":
    /// ["openssl"]}`, kind `all`, `security` or `named`) on stdin, check
    /// it, then refresh and upgrade with the host's apt or dnf. Exits with
    /// the package manager's code when it fails.
    Update,
}

#[cfg(unix)]
#[derive(Subcommand, Debug, Clone, Copy)]
enum SnapshotsCommand {
    /// Print the system volumes (/, /usr, /var, /opt, /boot) and
    /// MonitorFlow's snapshots of them, as JSON.
    Inspect,
    /// Read `{"name": "monitorflow-…"}` on stdin and snapshot every system
    /// volume on LVM or ZFS under that name, if there is room.
    Take,
    /// Read `{"name": "monitorflow-…"}` on stdin and delete MonitorFlow's
    /// snapshots of that name.
    Delete,
}

#[cfg(unix)]
#[derive(Subcommand, Debug, Clone, Copy)]
enum SelfUpdateStep {
    /// The watchdog, started by `self-update` in a transient systemd unit.
    #[command(hide = true)]
    Watch,
}

#[cfg(unix)]
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // The service dispatcher blocks this thread; the runtime starts in
    // the service's own thread.
    #[cfg(windows)]
    match cli.command {
        Some(Command::Service) => return service::run(cli.config),
        Some(Command::UpdateService) => return update_service::run(),
        _ => {}
    }
    tokio::runtime::Runtime::new()?.block_on(console(cli))
}

/// Started from a shell, or by systemd: a subcommand, or the agent until
/// a signal.
async fn console(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::Sensors(SensorsCommand::Probe { kinds, params })) => {
            let local = probe_limits(&cli.config)?;
            return probe(kinds, params.into_iter().collect(), &local).await;
        }
        #[cfg(unix)]
        Some(Command::Sources(command)) => {
            let path = cli.config.clone();
            return tokio::task::spawn_blocking(move || sources(&path, command)).await?;
        }
        #[cfg(unix)]
        Some(Command::Packages(PackagesCommand::Update)) => {
            return packages_update(cli.config.clone()).await
        }
        #[cfg(unix)]
        Some(Command::Snapshots(command)) => {
            let path = cli.config.clone();
            return tokio::task::spawn_blocking(move || snapshots(&path, command)).await?;
        }
        #[cfg(unix)]
        Some(Command::PinCa) => {
            let config: AgentConfig = config::load(&cli.config)?;
            let place = monitorflow_agent::helper_trust::Place::agent(&config.tls_paths());
            let token = config.enrollment_token.as_ref().map(|t| t.expose());
            return pin_ca(&place, token);
        }
        #[cfg(unix)]
        Some(Command::SelfUpdate { step }) => {
            let path = cli.config.clone();
            return tokio::task::spawn_blocking(move || self_update(&path, step)).await?;
        }
        #[cfg(windows)]
        Some(Command::Service | Command::UpdateService) => {
            unreachable!("started before the runtime")
        }
        #[cfg(windows)]
        Some(Command::AiCommands { action }) => {
            return set_ai_commands(&cli.config, action).await;
        }
        #[cfg(windows)]
        Some(Command::CheckDataFolder) => {
            use monitorflow_agent::packages::windows_update::{service_sid, AGENT_SERVICE};

            let dir = cli.config.parent().unwrap_or(Path::new(".")).to_owned();
            let agent = service_sid(AGENT_SERVICE);
            return tokio::task::spawn_blocking(move || {
                monitorflow_agent::data_folder::check(&dir, &agent)
            })
            .await?
            .map_err(|reason| {
                anyhow::anyhow!(
                    "{reason}. Someone other than an administrator made or changed it: check what \
                     it holds, or remove it, then install again"
                )
            });
        }
        Some(Command::InitConfig {
            upstream_url,
            enroll_url,
            enrollment_token,
        }) => {
            let settings =
                init_config::Settings::from_args(upstream_url, enroll_url, enrollment_token);
            let path = cli.config.clone();
            let outcome =
                tokio::task::spawn_blocking(move || init_config::write_new(&path, &settings))
                    .await??;
            let done = match outcome {
                init_config::Outcome::Written => "wrote",
                init_config::Outcome::Kept => "kept the existing",
            };
            println!("{done} {}", cli.config.display());
            return Ok(());
        }
        None => {}
    }
    let config = load(&cli.config, Logs::Stdout)?;
    let (trigger, stop) = shutdown::channel();
    shutdown::on_signals(trigger).context("cannot install signal handlers")?;
    run(config, stop).await
}

/// `ai-commands`: sets the key in the configuration file. `on` and `off`
/// also forget what the web interface last set, so they win over it.
#[cfg(windows)]
async fn set_ai_commands(path: &Path, action: AiCommandsAction) -> anyhow::Result<()> {
    use monitorflow_agent::ai_commands::{self, Key, Outcome};
    use monitorflow_agent::commands::ai_switch;

    let (key, on) = match action {
        AiCommandsAction::On => (Key::Shell, true),
        AiCommandsAction::Off => (Key::Shell, false),
        AiCommandsAction::FromWeb { state } => (Key::FromWeb, state == OnOff::On),
    };
    let file = path.to_owned();
    let (outcome, config) =
        tokio::task::spawn_blocking(move || ai_commands::set(&file, key, on)).await??;
    let forgot = match key {
        Key::Shell => {
            let data_dir = config.data_dir.clone();
            tokio::task::spawn_blocking(move || ai_switch::forget(&data_dir))
                .await?
                .with_context(|| {
                    format!(
                        "cannot remove the web interface's choice in {}",
                        config.data_dir.display()
                    )
                })?
        }
        Key::FromWeb => false,
    };
    let what = match (key, on) {
        (Key::Shell, true) => "AI commands are on",
        (Key::Shell, false) => "AI commands are off",
        (Key::FromWeb, true) => "the web interface may turn AI commands on and off",
        (Key::FromWeb, false) => "the web interface may only turn AI commands off",
    };
    if outcome == Outcome::Unchanged && !forgot {
        println!("{what} already ({})", path.display());
    } else {
        println!(
            "{what} in {}; restart the agent to apply it: Restart-Service MonitorFlowAgent",
            path.display()
        );
    }
    Ok(())
}

/// Where the agent's logs go.
#[derive(Debug, Clone, Copy)]
enum Logs {
    Stdout,
    /// `<data_dir>/logs/agent.log`, for a Windows service.
    #[cfg(windows)]
    File,
}

/// Loads the configuration and starts logging.
fn load(path: &Path, logs: Logs) -> anyhow::Result<AgentConfig> {
    let config: AgentConfig = config::load(path)?;
    // Before enrolling, like the configuration's own checks.
    sensor_config::local(&config.sensors)?;
    match logs {
        Logs::Stdout => logging::init(&config.log),
        #[cfg(windows)]
        Logs::File => {
            let file = config.data_dir.join("logs").join("agent.log");
            logging::init_file(&config.log, &file)
                .with_context(|| format!("cannot open the log file {}", file.display()))?;
        }
    }
    tracing::info!(config = %path.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting agent");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");
    Ok(config)
}

/// The agent, until `stop` fires.
async fn run(config: AgentConfig, stop: Shutdown) -> anyhow::Result<()> {
    // Before enrolling: Windows fails here without spending a token.
    let host = os::detect().context("cannot start on this host")?;
    tracing::info!(hostname = %host.hostname, os_family = %host.os_family, os_version = %host.os_version, "host detected");
    if !host.package_manager {
        tracing::info!("no package manager to drive on this host: package updates are unavailable, monitoring works normally");
    }

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
    let following = updates.clone();
    let plugin_checks = PluginChecks::new(
        identity.peer_id(),
        Arc::new(move || following.borrow().tls.ca_certs().to_vec()),
        config.plugins.clone(),
    )
    .with_state(config.data_dir.join("plugin-grants"));
    let plugin_checks = Arc::new(plugin_checks);
    let sensors = Sensors::new(
        Arc::new(
            Builtin::from_config(&config.sensors, &config.data_dir)?.with_plugins(plugin_checks),
        ),
        ConfigFile::in_dir(&config.data_dir),
        uplink.clone(),
    );
    let sensors = tokio::spawn(sensors.run(sensors_end, stop.clone()));
    // System logs (ADR-0091); the Windows Event Log is a follow-up.
    #[cfg(unix)]
    let logs = {
        use monitorflow_agent::journald;
        let logs = Uplink::new(journald::upload_settings());
        journald::start(&config.data_dir, logs.clone(), stop.clone());
        logs
    };
    let mut handlers = Handlers::default();
    let latest_report = platform_handlers(&config, &host, &mut handlers, &stop);
    let ai_switch = ai_handlers(&config, &mut handlers);
    let reporting = handlers.clone();
    let (commands_stream, commands_end) = commands::link();
    let ca_certs =
        tls::load_certs(&config.tls_paths().ca_cert).context("loading the CA certificate")?;
    let dispatcher = Dispatcher::new(
        identity.peer_id(),
        ca_certs,
        handlers,
        Journal::in_dir(&config.data_dir),
        DispatcherSettings::default(),
    )
    .following(updates.clone());
    // Answers a self_update the restart interrupted (ADR-0053).
    let dispatcher = dispatcher.with_resumer(Arc::new(StatusResumer::new(
        &monitorflow_agent::self_update::state_dir(),
    )));
    tokio::spawn(dispatcher.run(commands_end, stop.clone()));
    // The stream only returns on shutdown or a local error.
    let data_dir = config.data_dir.clone();
    let record_connected = Arc::new(move || {
        let data_dir = data_dir.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = Connected::record(&data_dir) {
                tracing::warn!(%error, "cannot record the connection for self-updates");
            }
        });
    });
    let mut agent = Agent::new(StreamSettings::from_config(&config), identity, host)
        .on_open(record_connected)
        .follow(updates)
        .with_sensors(stream_end)
        .with_upload(uplink)
        .with_commands(commands_stream)
        .with_command_handlers(reporting, ai_switch.subscribe());
    if let Some(reports) = latest_report {
        agent = agent.with_reports(reports);
    }
    #[cfg(unix)]
    {
        agent = agent.with_logs(logs);
    }
    let result = agent.run(stop).await.context("cannot connect upstream");
    if result.is_ok() {
        let _ = sensors.await;
    }
    result
}

/// `shell` and `ai_commands`, which share whether AI commands are on:
/// `commands.shell`, or what the server last set (ADR-0098).
fn ai_handlers(config: &AgentConfig, handlers: &mut Handlers) -> Arc<AiSwitch> {
    use monitorflow_agent::commands::ai_switch::AiSwitchHandler;
    use monitorflow_agent::commands::shell::ShellHandler;
    use monitorflow_common::command;

    let switch = Arc::new(AiSwitch::load(
        config.commands.shell,
        config.commands.from_web,
        &config.data_dir,
    ));
    handlers.register(
        command::SHELL,
        Arc::new(ShellHandler::switched(switch.clone())),
    );
    handlers.register(
        command::AI_COMMANDS,
        Arc::new(AiSwitchHandler::new(switch.clone())),
    );
    switch
}

/// Package reports every `packages.report_interval`, and the handlers of
/// `apply_updates` and the source commands; returns the reports
/// for the stream. Without a package manager there are no reports, and
/// the package commands are refused as unsupported (ADR-0063).
#[cfg(unix)]
fn platform_handlers(
    config: &AgentConfig,
    host: &HostInfo,
    handlers: &mut Handlers,
    stop: &Shutdown,
) -> Option<tokio::sync::watch::Receiver<Option<monitorflow_common::proto::v1::PackageReport>>> {
    use monitorflow_agent::commands::unsupported::Unsupported;
    use monitorflow_agent::packages::{
        self, ApplyUpdatesHandler, Reports, SourcesAction, SourcesHandler, Tools,
    };
    use monitorflow_agent::snapshots::{SnapshotAction, SnapshotHandler};
    use monitorflow_common::command;

    let tools = Tools::default();
    let manager =
        packages::for_family(host.os_family, tools.clone()).filter(|_| host.package_manager);
    let latest_report = manager.map(|manager| {
        let (reports, latest_report) = Reports::new(manager.clone());
        let report_every = Duration::from_secs(config.packages.report_interval);
        tokio::spawn(reports.clone().run(report_every, stop.clone()));
        handlers.register(
            command::APPLY_UPDATES,
            Arc::new(ApplyUpdatesHandler::new(
                manager,
                reports.clone(),
                tools.clone(),
            )),
        );
        for (kind, action) in [
            (command::CONFIGURE_SOURCES, SourcesAction::Apply),
            (command::REVERT_SOURCES, SourcesAction::Revert),
        ] {
            let handler = SourcesHandler::new(action, host.os_family, tools.clone());
            handlers.register(kind, Arc::new(handler));
        }
        for (kind, action) in [
            (command::TAKE_SNAPSHOT, SnapshotAction::Take),
            (command::DELETE_SNAPSHOT, SnapshotAction::Delete),
        ] {
            let handler = SnapshotHandler::new(action, tools.clone(), reports.clone());
            handlers.register(kind, Arc::new(handler));
        }
        latest_report
    });
    if latest_report.is_none() {
        for (kind, what) in [
            (command::APPLY_UPDATES, "package updates"),
            (command::CONFIGURE_SOURCES, "package sources"),
            (command::REVERT_SOURCES, "package sources"),
            (command::TAKE_SNAPSHOT, "snapshots"),
            (command::DELETE_SNAPSHOT, "snapshots"),
        ] {
            handlers.register(kind, Arc::new(Unsupported::new(what, "this system")));
        }
    }
    handlers.register(
        command::SELF_UPDATE,
        Arc::new(self_update::handler::SelfUpdateHandler::new(
            self_update::handler::SudoHelper {
                tools,
                product: self_update::AGENT,
            },
        )),
    );
    latest_report
}

/// Windows Update reports and `apply_updates` through the update service
/// (ADR-0049); the source commands are refused as unsupported (ADR-0047).
/// `shell` runs with Windows PowerShell (ADR-0084).
#[cfg(windows)]
fn platform_handlers(
    config: &AgentConfig,
    _host: &HostInfo,
    handlers: &mut Handlers,
    stop: &Shutdown,
) -> Option<tokio::sync::watch::Receiver<Option<monitorflow_common::proto::v1::PackageReport>>> {
    use monitorflow_agent::commands::unsupported::Unsupported;
    use monitorflow_agent::packages::windows_update::client::{
        WindowsApplyHandler, WindowsUpdates,
    };
    use monitorflow_agent::packages::windows_update::{pipe::PipeClient, PIPE_NAME};
    use monitorflow_agent::packages::Reports;
    use monitorflow_agent::self_update::handler::{SelfUpdateHandler, UpdateServiceInstaller};
    use monitorflow_common::command;

    let service = Arc::new(PipeClient::new(PIPE_NAME));
    let (reports, latest_report) = Reports::new(Arc::new(WindowsUpdates::new(service.clone())));
    let report_every = Duration::from_secs(config.packages.report_interval);
    tokio::spawn(reports.clone().run(report_every, stop.clone()));
    handlers.register(
        command::APPLY_UPDATES,
        Arc::new(WindowsApplyHandler::new(service.clone(), reports)),
    );
    handlers.register(
        command::SELF_UPDATE,
        Arc::new(SelfUpdateHandler::new(UpdateServiceInstaller {
            service,
            data_dir: config.data_dir.clone(),
        })),
    );
    for (kind, what) in [
        (command::CONFIGURE_SOURCES, "package sources"),
        (command::REVERT_SOURCES, "package sources"),
        (command::TAKE_SNAPSHOT, "snapshots"),
        (command::DELETE_SNAPSHOT, "snapshots"),
    ] {
        handlers.register(kind, Arc::new(Unsupported::new(what, "Windows")));
    }
    Some(latest_report)
}

/// What every root helper does first (ADR-0108): reads the command the
/// central server signed from stdin and checks it against the helper's
/// own trust anchor, as a command of `kind` for this server, not expired
/// and not run before. The helper acts on the returned payload only.
#[cfg(unix)]
fn signed_command(
    config: &Path,
    kind: &str,
) -> anyhow::Result<monitorflow_common::proto::v1::Command> {
    use monitorflow_agent::helper_trust::{authorize, HelperRequest, Place};

    let request = HelperRequest::read(&mut std::io::stdin().lock())?;
    let config: AgentConfig = config::load(config)?;
    let place = Place::agent(&config.tls_paths());
    let now = monitorflow_agent::self_update::unix_ms();
    Ok(authorize(&place, &request, kind, now)?)
}

/// Prints the changed paths and the backup directory, one per line.
#[cfg(unix)]
fn sources(config: &Path, command: SourcesCommand) -> anyhow::Result<()> {
    use monitorflow_agent::packages::sources::{self, SystemCheck};
    use monitorflow_common::command::{CONFIGURE_SOURCES, REVERT_SOURCES};
    use monitorflow_common::proto::v1::ConfigureSources;
    use prost::Message;

    let root = std::path::Path::new("/");
    let outcome = match command {
        SourcesCommand::Apply => {
            let command = signed_command(config, CONFIGURE_SOURCES)?;
            let spec = ConfigureSources::decode(command.payload.as_slice())
                .context("the payload is not a ConfigureSources message")?;
            sources::apply(&spec, root, &SystemCheck::default())?
        }
        SourcesCommand::Revert => {
            signed_command(config, REVERT_SOURCES)?;
            sources::revert(root)?
        }
    };
    for path in &outcome.changed {
        println!("changed {}", path.display());
    }
    if let Some(dir) = &outcome.backup {
        println!("backup {}", dir.display());
    }
    Ok(())
}

/// The snapshots helper (ADR-0094), as root. `take` and `delete` check
/// the request before anything else, then print each run's command line
/// and output, and the error last on stderr, like `packages update`.
#[cfg(unix)]
fn snapshots(config: &Path, command: SnapshotsCommand) -> anyhow::Result<()> {
    use std::io::Write;

    use monitorflow_agent::snapshots::handler::SnapshotAction;
    use monitorflow_agent::snapshots::helper::{self, Host};
    use monitorflow_common::command::{DELETE_SNAPSHOT, TAKE_SNAPSHOT};

    let host = Host::default();
    let outcome = match command {
        SnapshotsCommand::Inspect => {
            let inventory = helper::inspect(&host)?;
            println!("{}", serde_json::to_string_pretty(&inventory)?);
            return Ok(());
        }
        SnapshotsCommand::Take => {
            let command = signed_command(config, TAKE_SNAPSHOT)?;
            let name = SnapshotAction::Take
                .name(&command.payload)
                .map_err(anyhow::Error::msg)?;
            helper::take(&host, &name)
        }
        SnapshotsCommand::Delete => {
            let command = signed_command(config, DELETE_SNAPSHOT)?;
            let name = SnapshotAction::Delete
                .name(&command.payload)
                .map_err(anyhow::Error::msg)?;
            helper::delete(&host, &name)
        }
    };
    let (mut stdout, mut stderr) = (std::io::stdout().lock(), std::io::stderr().lock());
    for run in &outcome.runs {
        writeln!(stdout, "$ {}", run.line)?;
        stdout.write_all(run.stdout.as_bytes())?;
        stderr.write_all(run.stderr.as_bytes())?;
    }
    if let Some(note) = &outcome.note {
        writeln!(stdout, "{note}")?;
    }
    stdout.flush()?;
    if let Some(error) = &outcome.error {
        writeln!(stderr, "Error: {error}")?;
        stderr.flush()?;
        std::process::exit(1);
    }
    Ok(())
}

/// Checks the signed command and the spec it gives before anything else,
/// then prints each run's command line and output, and the error last on
/// stderr.
#[cfg(unix)]
async fn packages_update(config: PathBuf) -> anyhow::Result<()> {
    use std::io::Write;

    use monitorflow_agent::packages::update::{self, UpdateSpec};
    use monitorflow_agent::packages::Tools;
    use monitorflow_common::command::APPLY_UPDATES;
    use monitorflow_common::proto::v1::ApplyUpdates;
    use prost::Message;

    let command =
        tokio::task::spawn_blocking(move || signed_command(&config, APPLY_UPDATES)).await??;
    let request = ApplyUpdates::decode(command.payload.as_slice())
        .context("the payload is not an ApplyUpdates message")?;
    let spec = UpdateSpec::from_request(&request);
    spec.validate()?;
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

/// The installer's `pin-ca`, as root: writes the CA of the token in
/// MONITORFLOW_ENROLLMENT_TOKEN where the root helpers read it (ADR-0108).
#[cfg(unix)]
fn pin_ca(
    place: &monitorflow_agent::helper_trust::Place,
    token: Option<&str>,
) -> anyhow::Result<()> {
    use monitorflow_agent::helper_trust::{write_pin, Pinned};

    let token = token.context("MONITORFLOW_ENROLLMENT_TOKEN is not set")?;
    let token = monitorflow_common::tls::EnrollmentToken::parse(token)?;
    match write_pin(place, &token)? {
        Pinned::Written => println!("certificate authority recorded for privileged commands"),
        Pinned::Stale => println!(
            "this machine is connected with another certificate authority than this install \
             command's, which is an old one: nothing changed"
        ),
    }
    Ok(())
}

/// The self-update helper (ADR-0053), as root: installs the release on
/// stdin, or watches the restart. Reads the configuration for the agent's
/// data directory, where the new agent proves it connected.
#[cfg(unix)]
fn self_update(config: &Path, step: Option<SelfUpdateStep>) -> anyhow::Result<()> {
    use std::str::FromStr;

    use monitorflow_agent::self_update::helper::{self, Installed, Paths, Systemd, Watched};
    use monitorflow_agent::self_update::AGENT;
    use monitorflow_common::release::{self, Platform, Version};

    let config: AgentConfig = config::load(config)?;
    let trust = monitorflow_agent::helper_trust::Place::agent(&config.tls_paths());
    let paths = Paths::system(config.data_dir);
    match step {
        None => {
            let keys = release::trusted_keys()?;
            let current = Version::from_str(monitorflow_common::VERSION)?;
            let platform =
                Platform::current().context("releases are not built for this platform")?;
            let mut stdin = std::io::stdin().lock();
            match helper::install(
                &mut stdin,
                &paths,
                &trust,
                keys,
                &current,
                platform,
                &Systemd(AGENT),
            )? {
                Installed::Current(version) => println!("already running {version}"),
                Installed::Restarting { from, to } => {
                    println!(
                        "installed {to} over {from}; {} {to}",
                        self_update::handler::RESTARTING
                    );
                }
            }
        }
        Some(SelfUpdateStep::Watch) => {
            match helper::watch(&paths, &Systemd(AGENT), Duration::from_secs(2))? {
                Watched::Succeeded => println!("the new agent connected"),
                Watched::RolledBack => println!("rolled back to the previous agent"),
                Watched::Nothing => println!("no update to watch"),
            }
        }
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
fn probe_limits(path: &Path) -> anyhow::Result<registry::Local> {
    if !path.exists() {
        eprintln!(
            "{} not found; using the default sensor limits",
            path.display()
        );
        return Ok(registry::Local::default());
    }
    let config: AgentConfig = config::load(path)?;
    Ok(sensor_config::local(&config.sensors)?)
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

/// The Windows service (ADR-0047): reports `Running`, fires the shutdown
/// on `Stop`, `Shutdown` or `Preshutdown`, and reports `Stopped` with exit
/// code 1 when the agent ended with an error, which the service control
/// manager logs to the System event log. The error itself goes to the
/// Application event log (ADR-0050).
#[cfg(windows)]
mod service {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use monitorflow_agent::{eventlog, shutdown};
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::Logs;

    /// Name the service is installed under.
    pub const NAME: &str = "MonitorFlowAgent";
    /// How long a stop may take before the manager gives up waiting.
    const STOP_WAIT: Duration = Duration::from_secs(30);

    /// The `--config` path, for `service_main`, which only gets the
    /// service's start parameters.
    static CONFIG: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// Blocks until the service stops.
    pub fn run(config: PathBuf) -> anyhow::Result<()> {
        let _ = CONFIG.set(config);
        service_dispatcher::start(NAME, ffi_service_main).map_err(|error| {
            anyhow::anyhow!(
                "{error}: `service` only runs under the service control manager; run without it to start in this console"
            )
        })
    }

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(error) = serve() {
            // Logs may not be set up yet; the event log has the error.
            let error = format!("{error:#}");
            tracing::error!(%error, "the agent service stopped");
            eventlog::report_error(&format!("The MonitorFlow agent service stopped: {error}"));
        }
    }

    /// A service's status for the control manager; only a running
    /// service accepts stop and shutdown.
    pub fn status(
        state: ServiceState,
        exit_code: ServiceExitCode,
        wait_hint: Duration,
    ) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: match state {
                ServiceState::Running => {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                }
                _ => ServiceControlAccept::empty(),
            },
            exit_code,
            checkpoint: 0,
            wait_hint,
            process_id: None,
        }
    }

    /// Registers the control handler of the service `name`: stop,
    /// shutdown and preshutdown fire the returned trigger's shutdown.
    pub fn register(
        name: &str,
    ) -> anyhow::Result<(
        service_control_handler::ServiceStatusHandle,
        shutdown::Shutdown,
    )> {
        let (trigger, stop) = shutdown::channel();
        let mut trigger = Some(trigger);
        let handle = service_control_handler::register(name, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                if let Some(trigger) = trigger.take() {
                    tracing::info!(?control, "service stop requested");
                    trigger.fire();
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        Ok((handle, stop))
    }

    fn serve() -> anyhow::Result<()> {
        let (handle, stop) = register(NAME)?;
        let ok = ServiceExitCode::Win32(0);
        handle.set_service_status(status(ServiceState::Running, ok, Duration::ZERO))?;

        let config = CONFIG.get().cloned().unwrap_or_else(super::default_config);
        let result = tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| {
                runtime.block_on(async {
                    let mut stopping = stop.clone();
                    tokio::spawn(async move {
                        stopping.wait().await;
                        let pending = status(ServiceState::StopPending, ok, STOP_WAIT);
                        let _ = handle.set_service_status(pending);
                    });
                    let config = super::load(&config, Logs::File)?;
                    super::run(config, stop).await
                })
            });
        let exit_code = match &result {
            Ok(()) => ok,
            Err(_) => ServiceExitCode::ServiceSpecific(1),
        };
        if let Err(error) = &result {
            let error = format!("{error:#}");
            tracing::error!(%error, "the agent failed");
            eventlog::report_error(&format!("The MonitorFlow agent failed: {error}"));
        }
        handle.set_service_status(status(ServiceState::Stopped, exit_code, Duration::ZERO))?;
        Ok(())
    }
}

/// The update service (ADR-0049): `MonitorFlowUpdate`, as LocalSystem,
/// serves the agent's Windows Update requests on its pipe until stopped.
/// It does not believe the agent's account: what changes the host needs
/// the command the central server signed, which it checks against its own
/// trust anchor (ADR-0108). It reads `agent.toml`, which that account
/// cannot write, only for where the agent's certificates are and for the
/// install command's token, and logs to
/// `%ProgramData%\MonitorFlow\update\update.log`.
#[cfg(windows)]
mod update_service {
    use std::ffi::OsString;
    use std::time::Duration;

    use monitorflow_agent::helper_trust::{pin_once, Place};
    use monitorflow_agent::packages::windows_update::{
        authorized, pipe, pipe_sddl, service_sid, wua, Reply, Request, SelfUpdated, AGENT_SERVICE,
        PIPE_NAME, SERVICE_NAME,
    };
    use monitorflow_agent::self_update::swap::{self, Scm, Swapped, Watched};
    use monitorflow_common::config::{self, LogConfig};
    use monitorflow_common::logging;
    use windows_service::service::{ServiceExitCode, ServiceState};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::service::{register, status};

    /// How long a stop may take: an install in progress is not waited for.
    const STOP_WAIT: Duration = Duration::from_secs(10);

    define_windows_service!(ffi_update_main, update_main);

    /// Blocks until the service stops.
    pub fn run() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_update_main).map_err(|error| {
            anyhow::anyhow!("{error}: `update-service` only runs under the service control manager")
        })
    }

    fn update_main(_arguments: Vec<OsString>) {
        if let Err(error) = serve() {
            tracing::error!(error = format!("{error:#}"), "the update service stopped");
        }
    }

    /// Installs the staged agent release (ADR-0053), then restarts the
    /// agent and watches it in a thread of its own, so the reply goes back
    /// first. Once the new agent is kept, this service exits with an error
    /// so the service control manager restarts it on the new binary.
    fn self_update(request: &swap::Request) -> Reply {
        let installed = (|| -> anyhow::Result<(Swapped, swap::Paths)> {
            let keys = monitorflow_common::release::trusted_keys()?;
            let current: monitorflow_common::release::Version =
                monitorflow_common::VERSION.parse()?;
            let paths = swap::Paths {
                exe: std::env::current_exe()?.with_file_name("monitorflow-agent.exe"),
                state_dir: monitorflow_agent::self_update::state_dir(),
            };
            let platform = monitorflow_common::release::Platform::WindowsX86_64;
            Ok((
                swap::install(request, &paths, keys, &current, platform)?,
                paths,
            ))
        })();
        match installed {
            Err(error) => Reply::Error(format!("{error:#}")),
            Ok((Swapped::Current(version), _)) => Reply::SelfUpdate(SelfUpdated {
                restarting: false,
                message: format!("already running {version}"),
            }),
            Ok((Swapped::Restarting { from, to }, paths)) => {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(2));
                    match swap::watch(&paths, &Scm, Duration::from_secs(2)) {
                        Ok(Watched::Succeeded) => {
                            tracing::info!(
                                "the new agent connected; restarting the update service on it"
                            );
                            std::process::exit(1);
                        }
                        Ok(watched) => {
                            tracing::warn!(?watched, "the self-update did not keep the new agent")
                        }
                        Err(error) => tracing::error!(%error, "the self-update watch failed"),
                    }
                });
                Reply::SelfUpdate(SelfUpdated {
                    restarting: true,
                    message: format!("installed {to} over {from}; restarting into {to}"),
                })
            }
        }
    }

    /// The request to run for what the pipe received: reads as they are,
    /// anything else from the signed command it must carry.
    fn checked(request: &Request) -> Result<Request, String> {
        if matches!(request, Request::Scan | Request::Status) {
            return Ok(request.clone());
        }
        let config: monitorflow_common::config::AgentConfig =
            config::load(&super::default_config()).map_err(|e| e.to_string())?;
        let place = Place::agent(&config.tls_paths());
        pin_once(&place, config.enrollment_token.as_ref().map(|t| t.expose()));
        authorized(request, &place, monitorflow_agent::self_update::unix_ms())
    }

    fn serve() -> anyhow::Result<()> {
        let (handle, stop) = register(SERVICE_NAME)?;
        let ok = ServiceExitCode::Win32(0);
        handle.set_service_status(status(ServiceState::Running, ok, Duration::ZERO))?;
        let log = config::windows_base_dir().join("update").join("update.log");
        let result = logging::init_file(&LogConfig::default(), &log)
            .map_err(anyhow::Error::from)
            .and_then(|()| Ok(tokio::runtime::Runtime::new()?))
            .and_then(|runtime| {
                runtime.block_on(async {
                    let mut stopping = stop.clone();
                    tokio::spawn(async move {
                        stopping.wait().await;
                        let pending = status(ServiceState::StopPending, ok, STOP_WAIT);
                        let _ = handle.set_service_status(pending);
                    });
                    let sddl = pipe_sddl(&service_sid(AGENT_SERVICE));
                    let handle = |request: &Request| match checked(request) {
                        Err(error) => Reply::Error(error),
                        Ok(Request::SelfUpdate(request)) => self_update(&request),
                        Ok(other) => wua::handle(&other),
                    };
                    pipe::serve(PIPE_NAME, &sddl, handle, stop).await?;
                    Ok(())
                })
            });
        let exit_code = match &result {
            Ok(()) => ok,
            Err(error) => {
                tracing::error!(error = format!("{error:#}"), "the update service failed");
                ServiceExitCode::ServiceSpecific(1)
            }
        };
        handle.set_service_status(status(ServiceState::Stopped, exit_code, Duration::ZERO))?;
        Ok(())
    }
}
