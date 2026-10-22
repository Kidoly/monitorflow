//! MonitorFlow relay entry point.

use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::enroll::{self, EnrollError, EnrollSettings};
use monitorflow_common::client::renew::{Renewal, RenewalSettings};
use monitorflow_common::client::shutdown::{self, Shutdown};
use monitorflow_common::config::{self, RelayConfig};
use monitorflow_common::logging;
use monitorflow_common::mirror::serve;
use monitorflow_relay::buffer::{Buffer, BufferSettings};
use monitorflow_relay::control::{Control, ControlSettings};
use monitorflow_relay::forward::{ForwardSettings, Forwarder};
use monitorflow_relay::listener::{self, CrlCache, CrlSettings, Services, TlsPublisher};
use monitorflow_relay::mirror;
use monitorflow_relay::mirror::task::{MirrorSettings, MirrorTask};
use monitorflow_relay::renew::{RenewProxy, RenewSettings};
use tokio::net::TcpListener;

/// Time given to forwarded streams to end after a shutdown signal.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(name = "monitorflow-relay", version, about = "MonitorFlow relay")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        long,
        env = "MONITORFLOW_CONFIG",
        default_value = "/etc/monitorflow/relay.toml"
    )]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config: RelayConfig = config::load(&cli.config)?;
    logging::init(&config.log);
    tracing::info!(config = %cli.config.display(), protocol = monitorflow_common::PROTOCOL_VERSION, "starting relay");
    // Secrets are redacted by their Debug impls.
    tracing::info!(effective = ?config, "configuration loaded");

    let (trigger, stop) = shutdown::channel();
    shutdown::on_signals(trigger).context("cannot install signal handlers")?;
    let enroll = EnrollSettings::relay(&config);
    let identity = match enroll::ensure_identity(&enroll, Backoff::default(), stop.clone()).await {
        Ok(identity) => identity,
        Err(EnrollError::Shutdown) => return Ok(()),
        Err(error) => return Err(error).context("no usable relay identity"),
    };
    let names = listener::server_names(&identity).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        !names.is_empty(),
        "the relay certificate names no DNS name or IP address, so agents cannot verify this relay; create a relay token with --server-name, remove the identity files and enroll again"
    );
    tracing::info!(peer = %identity.id, ?names, not_after = ?identity.not_after, "identity loaded");

    let (renewal, identities) = Renewal::new(
        RenewalSettings::for_upstream(&config.upstream_url),
        config.tls_paths(),
        identity,
    );
    tokio::spawn(renewal.run(stop.clone()));
    let (control, upstream_up) =
        Control::new(ControlSettings::from_config(&config), identities.clone());
    tokio::spawn(control.run(stop.clone()));

    let crl = CrlSettings::for_upstream(&config.upstream_url);
    let started = TlsPublisher::start(crl, identities.clone(), Backoff::default(), stop.clone())
        .await
        .context("cannot accept agents")?;
    let Some((publisher, tls)) = started else {
        return Ok(());
    };
    let crl = CrlCache::new(publisher.crl());
    tokio::spawn(publisher.run(stop.clone()));

    let buffer_settings = BufferSettings::from_config(&config);
    let buffer = Buffer::open(&buffer_settings).await.with_context(|| {
        format!(
            "cannot open the buffer in {}",
            buffer_settings.dir.display()
        )
    })?;
    tokio::spawn(buffer.clone().run(stop.clone()));

    let listener = TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("binding the agent listener on {}", config.listen_addr))?;
    tracing::info!(listen = %config.listen_addr, upstream = %config.upstream_url, "accepting agents");
    let renew = RenewProxy::new(
        RenewSettings::for_upstream(&config.upstream_url),
        identities.clone(),
    );
    let forwarder = Forwarder::new(
        ForwardSettings::from_config(&config),
        identities,
        upstream_up,
        buffer,
    );
    tokio::spawn(forwarder.clone().run_replays(stop.clone()));
    if config.mirror.enabled {
        // Mirroring is secondary to monitoring: a busy port or a bad
        // mirror directory must not stop agent forwarding (ADR-0034).
        if let Err(error) = start_mirrors(&config, stop.clone()).await {
            tracing::error!(
                error = format!("{error:#}"),
                "mirrors disabled; agent forwarding continues"
            );
        }
    }
    let mut until_stopped = stop.clone();
    let services = Services {
        forwarder,
        renew,
        crl,
    };
    let served = listener::serve(listener, tls, services, async move {
        until_stopped.wait().await
    });
    tokio::pin!(served);
    let mut stopped = stop;
    tokio::select! {
        result = &mut served => result.context("agent listener")?,
        () = stopped.wait() => {
            // Forwarded streams end once the upstream sees them closed;
            // do not wait forever for an upstream that does not answer.
            if tokio::time::timeout(SHUTDOWN_GRACE, served).await.is_err() {
                tracing::warn!("forwarded streams still open at shutdown; closing them");
            }
        }
    }
    tracing::info!("relay stopped");
    Ok(())
}

/// Starts the mirror listener and the sync loop (ADR-0034). Fails without
/// spawning anything if the directory, listener or sync client cannot be
/// set up.
async fn start_mirrors(config: &RelayConfig, stop: Shutdown) -> anyhow::Result<()> {
    let root = config.mirror_dir();
    let served = tokio::task::spawn_blocking(move || mirror::prepare(&root))
        .await?
        .with_context(|| format!("cannot prepare {}", config.mirror_dir().display()))?;
    let task = MirrorTask::new(MirrorSettings::from_config(config))
        .context("cannot build the mirror sync client")?;
    let mirror_listener = TcpListener::bind(config.mirror_listen_addr)
        .await
        .with_context(|| {
            format!(
                "binding the mirror listener on {}",
                config.mirror_listen_addr
            )
        })?;
    tracing::info!(listen = %config.mirror_listen_addr, dir = %config.mirror_dir().display(), "serving mirrors");
    let mut until_stopped = stop.clone();
    tokio::spawn(async move {
        let shutdown = async move { until_stopped.wait().await };
        if let Err(error) = serve::serve(mirror_listener, served, shutdown).await {
            tracing::error!(%error, "mirror listener failed");
        }
    });
    tokio::spawn(task.run(stop));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_config(
        dir: &std::path::Path,
        mirror_listen_addr: std::net::SocketAddr,
    ) -> RelayConfig {
        let mut config: RelayConfig = config::parse_with_env(
            r#"upstream_url = "https://127.0.0.1:1""#,
            std::path::Path::new("relay.toml"),
            &Default::default(),
        )
        .unwrap();
        config.data_dir = dir.to_owned();
        config.mirror_listen_addr = mirror_listen_addr;
        config
    }

    #[tokio::test]
    async fn a_busy_mirror_port_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = relay_config(dir.path(), taken.local_addr().unwrap());
        let (_trigger, stop) = shutdown::channel();
        let error = start_mirrors(&config, stop).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("binding the mirror listener"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn an_unusable_mirror_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"").unwrap();
        let mut config = relay_config(dir.path(), "127.0.0.1:0".parse().unwrap());
        config.mirror_dir = Some(file.join("mirrors"));
        let (_trigger, stop) = shutdown::channel();
        let error = start_mirrors(&config, stop).await.unwrap_err();
        assert!(format!("{error:#}").contains("cannot prepare"), "{error:#}");
    }

    #[tokio::test]
    async fn mirrors_start_on_a_free_port() {
        let dir = tempfile::tempdir().unwrap();
        let config = relay_config(dir.path(), "127.0.0.1:0".parse().unwrap());
        let (trigger, stop) = shutdown::channel();
        start_mirrors(&config, stop).await.unwrap();
        assert!(dir.path().join("mirrors/apt").is_dir());
        trigger.fire();
    }
}
