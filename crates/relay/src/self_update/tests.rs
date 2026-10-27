use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_agent::commands::Output;
use monitorflow_agent::self_update::handler::{Installed, Installer};
use monitorflow_agent::self_update::Header;
use monitorflow_common::client::shutdown;
use monitorflow_common::command;
use monitorflow_common::proto::v1::{Command, CommandResult, CommandStatus, SelfUpdate};
use monitorflow_common::release::{Platform, Version};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use prost::Message;
use tempfile::TempDir;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Never reached in these tests: the release is refused before.
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

struct Setup {
    _dirs: (TempDir, TempDir, TempDir),
    ca: CertificateAuthority,
    stream: StreamEnd,
    _stop: shutdown::Trigger,
}

impl Setup {
    fn new() -> Self {
        let (data, state, ca_dir) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let ca = CertificateAuthority::open_or_create(ca_dir.path(), CaOptions::default()).unwrap();
        let ca_der = pem::parse(ca.ca_cert_pem()).unwrap().into_contents();
        let handler = SelfUpdateHandler::with(
            NoInstall,
            Vec::new(),
            "0.2.0".parse::<Version>().unwrap(),
            Some(Platform::LinuxX86_64),
        )
        .for_product(RELAY);
        let (dispatcher, stream, end) = dispatcher(
            "relay-1",
            vec![CertificateDer::from(ca_der)],
            data.path(),
            handler,
            state.path(),
        );
        let (trigger, stop) = shutdown::channel();
        tokio::spawn(dispatcher.run(end, stop));
        Self {
            _dirs: (data, state, ca_dir),
            ca,
            stream,
            _stop: trigger,
        }
    }

    fn command(&self, id: &str, for_peer: &str, kind: &str, payload: Vec<u8>) -> Command {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut command = Command {
            command_id: id.into(),
            kind: kind.into(),
            payload,
            issued_at_unix_ms: now,
            expires_at_unix_ms: now + 60_000,
            agent_id: for_peer.into(),
            signature: Vec::new(),
        };
        command.signature = self
            .ca
            .sign_command(&command::signed_bytes(&command))
            .unwrap();
        command
    }

    async fn result(&mut self) -> CommandResult {
        tokio::time::timeout(TIMEOUT, self.stream.results.recv())
            .await
            .expect("a result in time")
            .unwrap()
    }
}

fn update_to(version: &str, url: &str) -> Vec<u8> {
    SelfUpdate {
        version: version.into(),
        base_urls: vec![url.into()],
        ..SelfUpdate::default()
    }
    .encode_to_vec()
}

#[tokio::test]
async fn a_self_update_runs_and_says_why_it_failed() {
    let mut setup = Setup::new();
    // Nothing listens there: no source serves the release.
    let payload = update_to("0.3.0", "http://127.0.0.1:1/release/");
    let command = setup.command("c1", "relay-1", command::SELF_UPDATE, payload);
    setup.stream.commands.send(command).await.unwrap();
    let running = setup.result().await;
    assert_eq!(running.status(), CommandStatus::Running);
    let done = setup.result().await;
    assert_eq!(
        (done.command_id.as_str(), done.status()),
        ("c1", CommandStatus::Failed)
    );
    assert!(
        done.error
            .contains("no source served a valid 0.3.0 release"),
        "{}",
        done.error
    );
}

#[tokio::test]
async fn commands_for_others_or_of_other_kinds_or_forged_are_refused() {
    let mut setup = Setup::new();
    let payload = update_to("0.3.0", "http://127.0.0.1:1/");
    let other = setup.command("c1", "web-01", command::SELF_UPDATE, payload.clone());
    let shell = setup.command("c2", "relay-1", command::SHELL, Vec::new());
    let mut forged = setup.command("c3", "relay-1", command::SELF_UPDATE, payload);
    forged.payload = update_to("9.9.9", "http://attacker.example/");
    for command in [other, shell, forged] {
        let id = command.command_id.clone();
        setup.stream.commands.send(command).await.unwrap();
        let result = setup.result().await;
        assert_eq!(
            (result.command_id.as_str(), result.status()),
            (id.as_str(), CommandStatus::Rejected),
            "{}",
            result.error
        );
    }
}

#[tokio::test]
async fn an_older_relay_release_is_not_installed() {
    let mut setup = Setup::new();
    let payload = update_to("0.1.0", "http://127.0.0.1:1/");
    let command = setup.command("c1", "relay-1", command::SELF_UPDATE, payload);
    setup.stream.commands.send(command).await.unwrap();
    let result = loop {
        let result = setup.result().await;
        if result.status() != CommandStatus::Running {
            break result;
        }
    };
    assert_eq!(result.status(), CommandStatus::Rejected, "{}", result.error);
}
