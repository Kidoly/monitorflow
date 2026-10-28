//! Relay self-update over its own stream (ADR-0055), against the real
//! server pieces: the server records the relay's version from its Hello,
//! signs and sends a queued relay update, and records the result the
//! relay's dispatcher sends back. The relay writes its connected file for
//! the watchdog. No release is served, so the update fails before
//! anything is installed.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_agent::commands::Output;
use monitorflow_agent::self_update::handler::{Installed, Installer, SelfUpdateHandler};
use monitorflow_agent::self_update::{Connected, Header, RELAY};
use monitorflow_common::client::shutdown;
use monitorflow_common::config::RelayConfig;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::release::{Platform, Version};
use monitorflow_relay::control::{Control, ControlSettings};
use monitorflow_relay::self_update;
use monitorflow_server::grpc::{AgentStreamService, AllowAllRelayedAgents, DbInventory};
use monitorflow_server::relay_commands::{self, RelayCommands};
use prost::Message;
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use support::{server_settings, wait_until, Pki};
use tempfile::TempDir;
use tokio::sync::watch;
use uuid::Uuid;

struct NoInstall;

#[async_trait::async_trait]
impl Installer for NoInstall {
    async fn install(&self, _header: &Header, _binary: &[u8]) -> Result<Installed, Output> {
        panic!("nothing valid to install")
    }

    fn log_hint(&self) -> &'static str {
        RELAY.watch_log
    }
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn the_server_updates_a_relay_on_its_own_stream(pool: PgPool) {
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let relay_id: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name) VALUES ('paris-1', $1, 'paris-1') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();

        let pki = Pki::new();
        let (ca, inventory_pool, command_pool) = (pki.ca.clone(), pool.clone(), pool.clone());
        let (relay_commands_tx, relay_commands_rx) = std::sync::mpsc::channel();
        let upstream = pki
            .serve(move |registry| {
                let commands = RelayCommands::spawn(command_pool, registry.clone(), ca);
                relay_commands_tx.send(commands.clone()).unwrap();
                AgentStreamService::new(registry, Arc::new(AllowAllRelayedAgents), server_settings())
                    .with_inventory(Arc::new(DbInventory::new(inventory_pool)))
                    .with_relay_commands(commands)
            })
            .await;
        let relay_commands = relay_commands_rx.recv().unwrap();

        // The relay's own stream with its command dispatcher.
        let data = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let ca_der = CertificateDer::from(pem::parse(pki.ca.ca_cert_pem()).unwrap().into_contents());
        let handler = SelfUpdateHandler::with(
            NoInstall,
            Vec::new(),
            "0.2.0".parse::<Version>().unwrap(),
            Some(Platform::LinuxX86_64),
        )
        .for_product(RELAY);
        let (dispatcher, stream, end) =
            self_update::dispatcher("paris-1", vec![ca_der], data.path(), handler, state.path());
        let (_trigger, stop) = shutdown::channel();
        tokio::spawn(dispatcher.run(end, stop.clone()));
        let config = RelayConfig {
            upstream_url: upstream.url.clone(),
            heartbeat_interval: 1,
            ..RelayConfig::default()
        };
        let (_identity, identities) = watch::channel(pki.relay("paris-1"));
        let (control, up) = Control::new(ControlSettings::from_config(&config), identities);
        tokio::spawn(
            control
                .with_commands(stream)
                .with_connected_record(data.path().to_owned())
                .run(stop),
        );
        wait_until("relay stream open", || *up.borrow()).await;
        wait_until("connected file", || Connected::read(data.path()).ok().flatten().is_some()).await;
        let reported: Option<String> = sqlx::query_scalar("SELECT relay_version FROM relays WHERE id = $1")
            .bind(relay_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(reported.as_deref(), Some(monitorflow_common::VERSION));

        let payload = SelfUpdate {
            version: "0.3.0".into(),
            base_urls: vec!["http://127.0.0.1:1/".into()],
            ..SelfUpdate::default()
        }
        .encode_to_vec();
        let mut conn = pool.acquire().await.unwrap();
        let command = relay_commands::enqueue_in(&mut conn, relay_id, "0.3.0", &payload, Duration::from_secs(600), None)
            .await
            .unwrap()
            .unwrap();
        drop(conn);
        relay_commands.dispatch("paris-1");

        let deadline = tokio::time::Instant::now() + support::TIMEOUT;
        loop {
            let (state, error): (String, Option<String>) =
                sqlx::query_as("SELECT state, error FROM relay_commands WHERE id = $1")
                    .bind(command.id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if state == "failed" {
                let error = error.unwrap_or_default();
                assert!(error.contains("no source served a valid 0.3.0 release"), "{error}");
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "result never recorded: {state}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
