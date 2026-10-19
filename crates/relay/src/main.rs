//! MonitorFlow relay entry point.

use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use monitorflow_common::client::backoff::Backoff;
use monitorflow_common::client::enroll::{self, EnrollError, EnrollSettings};
use monitorflow_common::client::renew::{Renewal, RenewalSettings};
use monitorflow_common::client::shutdown;
use monitorflow_common::config::{self, RelayConfig};
use monitorflow_common::logging;
use monitorflow_relay::control::{Control, ControlSettings};
use monitorflow_relay::forward::{ForwardSettings, Forwarder};
use monitorflow_relay::listener::{self, CrlCache, CrlSettings, Services, TlsPublisher};
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
    );
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
