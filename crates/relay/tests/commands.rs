//! Commands through a relay (ADR-0031): the server's dispatcher signs and
//! sends them, the relay passes them down and the results up unchanged.

mod support;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::command;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{CommandResult, CommandStatus};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings, NewCommand, State};
use monitorflow_server::grpc::{AgentStreamService, AllowAllRelayedAgents};
use rustls::pki_types::CertificateDer;
use sqlx::PgPool;
use support::{hello, open, server_settings, Pki, Relay};
use uuid::Uuid;

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn commands_and_results_cross_the_relay_unchanged(pool: PgPool) {
        let site: Uuid = sqlx::query_scalar("INSERT INTO sites (name) VALUES ('paris') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let server_id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-01', $1, 'web-01', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(&pool)
        .await
        .unwrap();
        let store = CommandStore::new(pool.clone());
        let new = NewCommand {
            server_id,
            kind: command::APPLY_UPDATES.into(),
            payload: b"payload".to_vec(),
            ttl: Duration::from_secs(3600),
            requested_by: None,
            campaign_id: None,
        };
        let id = store.enqueue(&new).await.unwrap().id;

        let pki = Pki::new();
        let (signer, dispatch_store) = (pki.ca.clone(), store.clone());
        let upstream = pki
            .serve(move |registry| {
                let commands = Commands::spawn(dispatch_store, registry.clone(), signer, DispatcherSettings::default());
                AgentStreamService::new(registry, Arc::new(AllowAllRelayedAgents), server_settings())
                    .with_commands(commands)
            })
            .await;
        let relay = Relay::start(pki.relay("paris-1"), &upstream.url).await;
        let mut agent = open(&relay.url, &pki.agent("web-01"), hello("web-01")).await.unwrap();

        let command = loop {
            match agent.next().await.unwrap().and_then(|e| e.payload) {
                Some(Payload::Command(command)) => break command,
                Some(_) => continue,
                None => panic!("stream ended"),
            }
        };
        assert_eq!((command.command_id.as_str(), command.agent_id.as_str()), (id.to_string().as_str(), "web-01"));
        let ca = CertificateDer::from(pem::parse(pki.ca.ca_cert_pem()).unwrap().into_contents());
        assert_eq!(command::verify(&command, &[ca]), Ok(()));
        assert_eq!(upstream.registry.get("web-01").unwrap().via_relay.as_deref(), Some("paris-1"));

        let result = CommandResult {
            command_id: command.command_id.clone(),
            status: CommandStatus::Succeeded as i32,
            stdout: b"upgraded".to_vec(),
            ..CommandResult::default()
        };
        agent.send(2, Payload::CommandResult(result)).await;
        let deadline = tokio::time::Instant::now() + support::TIMEOUT;
        loop {
            let row = store.get(id).await.unwrap().unwrap();
            if row.state == State::Succeeded {
                assert_eq!(row.stdout.as_deref(), Some("upgraded"));
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "result never recorded: {:?}", row.state);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
