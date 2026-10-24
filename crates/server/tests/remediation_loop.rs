//! The remediation loop (ADR-0043) against a real PostgreSQL, a scripted
//! AI provider and a test agent on a real mTLS stream that checks every
//! command's signature: automatic runs, approval parking, forbidden
//! commands, limits, and restarts. Ignored without `DATABASE_URL`.

#[path = "../../common/tests/support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_ai::testing::MockProvider;
use monitorflow_ai::{CommandOutcome, Step, StopOutcome};
use monitorflow_common::command::{self, SHELL};
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::agent_service_client::AgentServiceClient;
use monitorflow_common::proto::v1::envelope::Payload;
use monitorflow_common::proto::v1::{CommandResult, CommandStatus, Envelope, Hello, ShellCommand};
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::{self, Identity};
use monitorflow_common::PROTOCOL_VERSION;
use monitorflow_server::api::{users, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings};
use monitorflow_server::grpc::{
    self, AgentStreamService, DbInventory, DbRelayPolicy, Registry, StreamSettings,
};
use monitorflow_server::remediation::approval::{self, Verdict};
use monitorflow_server::remediation::audit::{self, StoredEvent};
use monitorflow_server::remediation::context::Backends;
use monitorflow_server::remediation::runner::{Remediation, Settings};
use prost::Message;
use rustls::pki_types::CertificateDer;
use serde_json::Value;
use sqlx::PgPool;
use support::Ca;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(20);
const PEER: &str = "agent-web-01";

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

/// What the test agent answers to a command line.
#[derive(Debug, Clone)]
enum Reply {
    Run {
        exit_code: i32,
        stdout: &'static str,
    },
    /// Keeps the command, unanswered, until [`TestAgent::answer`].
    Hold,
}

/// An agent on a real stream: checks each command's signature and
/// payload, records the command lines, and answers from its script.
struct TestAgent {
    seen: Arc<Mutex<Vec<String>>>,
    answer: mpsc::Sender<(String, i32, &'static str)>,
}

impl TestAgent {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// Answers the held command `line`.
    async fn answer(&self, line: &str, exit_code: i32, stdout: &'static str) {
        self.answer
            .send((line.to_owned(), exit_code, stdout))
            .await
            .unwrap();
    }
}

/// The server's stream service and command dispatcher over mTLS, with
/// one customer, site, server and open disk alert.
struct Harness {
    pool: PgPool,
    commands: Commands,
    agent: TestAgent,
    customer: Uuid,
    alert: Uuid,
    _dir: TempDir,
}

impl Harness {
    async fn start(pool: &PgPool, script: &[(&str, Reply)]) -> Self {
        let customer: Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
        )
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, 'web-01', 'debian') RETURNING id",
        )
        .bind(PEER)
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        let alert: Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'monitorflow_disk_used_ratio', '/', 'critical', now(), 0.97, now(),
                     0.97, '{\"op\": \">\", \"critical\": 0.95}')
             RETURNING id",
        )
        .bind(server)
        .fetch_one(pool)
        .await
        .unwrap();

        let dir = TempDir::new().unwrap();
        let signer = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let ca = Ca::new("monitorflow test CA");
        let identity = |leaf| {
            let dir = TempDir::new().unwrap();
            let (ca_cert, cert, key) = support::write(dir.path(), "id", &ca, &leaf);
            Identity::load(&TlsPaths { ca_cert, cert, key }).unwrap()
        };
        let (_tx, tls_config) =
            watch::channel(identity(ca.server()).server_config(Vec::new()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let registry = Registry::default();
        let commands = Commands::spawn(
            CommandStore::new(pool.clone()),
            registry.clone(),
            signer.clone(),
            DispatcherSettings {
                poll_interval: Duration::from_millis(100),
                ..DispatcherSettings::default()
            },
        );
        let service = AgentStreamService::new(
            registry,
            Arc::new(DbRelayPolicy::new(pool.clone())),
            StreamSettings {
                heartbeat_timeout: TIMEOUT,
                ..StreamSettings::default()
            },
        )
        .with_inventory(Arc::new(DbInventory::new(pool.clone())))
        .with_commands(commands.clone());
        tokio::spawn(grpc::serve(
            listener,
            tls_config,
            service,
            std::future::pending(),
        ));

        let config = identity(ca.client(PEER, 2)).client_config().unwrap();
        let channel = tls::connect(&url, config, TIMEOUT).await.unwrap();
        let (tx, rx) = mpsc::channel(8);
        tx.send(Envelope {
            sequence: 1,
            payload: Some(Payload::Hello(Hello {
                agent_id: PEER.into(),
                protocol_version: PROTOCOL_VERSION.into(),
                agent_version: "0.1.0".into(),
                hostname: "web-01".into(),
                os_family: "debian".into(),
                os_version: "debian 12".into(),
            })),
        })
        .await
        .unwrap();
        let stream = AgentServiceClient::new(channel)
            .stream(ReceiverStream::new(rx))
            .await
            .unwrap()
            .into_inner();
        let pem = pem::parse(signer.ca_cert_pem()).unwrap();
        let signer_cert = CertificateDer::from(pem.into_contents());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (answer, answers) = mpsc::channel(8);
        let script: HashMap<String, Reply> = script
            .iter()
            .map(|(line, reply)| ((*line).to_owned(), reply.clone()))
            .collect();
        tokio::spawn(run_agent(
            stream,
            tx,
            signer_cert,
            script,
            seen.clone(),
            answers,
        ));
        Self {
            pool: pool.clone(),
            commands,
            agent: TestAgent { seen, answer },
            customer,
            alert,
            _dir: dir,
        }
    }

    /// A remediation loop on `provider`, running until stopped.
    fn remediation(&self, provider: &Arc<MockProvider>, settings: Settings) -> Running {
        let remediation = Remediation::new(
            provider.clone(),
            "test-model",
            self.commands.clone(),
            Backends {
                pool: self.pool.clone(),
                metrics: None,
                logs: None,
            },
            settings,
        );
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(remediation.run(async move {
            let _ = stopped.await;
        }));
        Running { stop, task }
    }

    /// The remediation incident of the alert, once it exists.
    async fn incident(&self) -> Uuid {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let id: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM remediation_incidents WHERE alert_incident_id = $1",
            )
            .bind(self.alert)
            .fetch_optional(&self.pool)
            .await
            .unwrap();
            if let Some(id) = id {
                return id;
            }
            assert!(tokio::time::Instant::now() < deadline, "no incident opened");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for the incident to reach `state`.
    async fn wait_state(&self, state: &str) -> Uuid {
        let id = self.incident().await;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let now: String =
                sqlx::query_scalar("SELECT state FROM remediation_incidents WHERE id = $1")
                    .bind(id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap();
            if now == state {
                return id;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "incident stayed {now}, expected {state}; events: {:?}",
                kinds(&audit::events(&self.pool, id).await.unwrap())
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for an event of `kind` matching `test`.
    async fn wait_event(&self, kind: &str, test: impl Fn(&Value) -> bool) {
        let id = self.incident().await;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let events = audit::events(&self.pool, id).await.unwrap();
            if events
                .iter()
                .any(|e| kind_of(e) == kind && test(&e.payload))
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no {kind} event; events: {:?}",
                kinds(&events)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn resolve_alert(&self) {
        sqlx::query("UPDATE incidents SET state = 'resolved', resolved_at = now() WHERE id = $1")
            .bind(self.alert)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn events(&self) -> Vec<StoredEvent> {
        audit::events(&self.pool, self.incident().await)
            .await
            .unwrap()
    }

    async fn leases(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM remediation_loop_leases")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

struct Running {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Running {
    /// Graceful stop: drivers end and leases are released.
    async fn stop(self) {
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }

    /// A crash: everything stops where it is; leases stay until expiry.
    async fn crash(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn run_agent(
    mut stream: tonic::Streaming<Envelope>,
    tx: mpsc::Sender<Envelope>,
    signer: CertificateDer<'static>,
    script: HashMap<String, Reply>,
    seen: Arc<Mutex<Vec<String>>>,
    mut answers: mpsc::Receiver<(String, i32, &'static str)>,
) {
    let mut sequence = 1;
    let mut held: HashMap<String, String> = HashMap::new();
    let mut send = |result: CommandResult| {
        sequence += 1;
        let tx = tx.clone();
        let envelope = Envelope {
            sequence,
            payload: Some(Payload::CommandResult(result)),
        };
        async move { tx.send(envelope).await.unwrap() }
    };
    let result = |id: &str, exit_code: i32, stdout: &str| CommandResult {
        command_id: id.to_owned(),
        exit_code,
        stdout: stdout.as_bytes().to_vec(),
        status: if exit_code == 0 {
            CommandStatus::Succeeded
        } else {
            CommandStatus::Failed
        } as i32,
        ..CommandResult::default()
    };
    loop {
        tokio::select! {
            message = stream.message() => {
                let Ok(Some(envelope)) = message else { return };
                let Some(Payload::Command(c)) = envelope.payload else { continue };
                command::verify(&c, std::slice::from_ref(&signer)).expect("command signature");
                assert_eq!((c.kind.as_str(), c.agent_id.as_str()), (SHELL, PEER));
                let shell = ShellCommand::decode(c.payload.as_slice()).unwrap();
                assert!(shell.timeout_seconds > 0);
                seen.lock().unwrap().push(shell.command.clone());
                send(CommandResult {
                    command_id: c.command_id.clone(),
                    status: CommandStatus::Running as i32,
                    ..CommandResult::default()
                })
                .await;
                match script.get(&shell.command) {
                    Some(Reply::Run { exit_code, stdout }) => {
                        send(result(&c.command_id, *exit_code, stdout)).await;
                    }
                    Some(Reply::Hold) => {
                        held.insert(shell.command, c.command_id);
                    }
                    None => panic!("unscripted command {:?}", shell.command),
                }
            }
            Some((line, exit_code, stdout)) = answers.recv() => {
                let id = held.remove(&line).expect("no such held command");
                send(result(&id, exit_code, stdout)).await;
            }
        }
    }
}

fn kind_of(event: &StoredEvent) -> String {
    serde_json::to_value(event.kind)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn kinds(events: &[StoredEvent]) -> Vec<String> {
    events.iter().map(kind_of).collect()
}

fn of_kind<'a>(events: &'a [StoredEvent], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| kind_of(e) == kind)
        .map(|e| &e.payload)
        .collect()
}

fn settings() -> Settings {
    Settings {
        command_timeout: Duration::from_secs(5),
        result_grace: Duration::from_millis(500),
        poll_interval: Duration::from_millis(100),
        result_poll: Duration::from_millis(50),
        lease_ttl: Duration::from_secs(1),
        resolve_wait: Duration::from_secs(10),
        ..Settings::default()
    }
}

/// Decides the pending command of the parked incident `id` as a new
/// approver would through the API.
async fn decide(pool: &PgPool, id: Uuid, verdict: Verdict) {
    let pending: Uuid = sqlx::query_scalar(
        "SELECT id FROM remediation_pending_commands WHERE incident_id = $1 AND state = 'pending'",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    let user = users::create_user(
        pool,
        "dave",
        &Secret::new("correct horse battery"),
        Role::Approver,
        "test",
    )
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    approval::decide(&mut tx, pending, user, &verdict)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn last_escalation(h: &Harness) -> String {
    let events = h.events().await;
    of_kind(&events, "escalation")
        .last()
        .map(|p| p["reason"].as_str().unwrap().to_owned())
        .unwrap_or_default()
}

db_tests! {
    #[sqlx::test]
    async fn automatic_commands_run_redacted_until_the_sensor_confirms(pool: PgPool) {
        let script = [
            ("df -h", Reply::Run { exit_code: 0, stdout: "/dev/sda1 97% /\npassword=hunter2Sup3r\n" }),
            ("systemctl restart rsyslog", Reply::Run { exit_code: 0, stdout: "" }),
        ];
        let h = Harness::start(&pool, &script).await;
        // Low-impact commands run without approval on this customer.
        sqlx::query("INSERT INTO remediation_policies (customer_id, low_impact_auto) VALUES ($1, true)")
            .bind(h.customer)
            .execute(&pool)
            .await
            .unwrap();
        let ai = Arc::new(MockProvider::new([
            Step::run("disk almost full", "df -h", "see usage"),
            Step::run("rsyslog holds deleted files", "systemctl restart rsyslog", "release them"),
            Step::stop("space freed", StopOutcome::Resolved, "usage dropped"),
        ]));
        let running = h.remediation(&ai, settings());
        h.wait_event("ai_proposal", |p| p["stop"] == "resolved").await;
        // The AI says it is fixed; the incident waits for the sensor.
        assert_eq!(h.wait_state("running").await, h.incident().await);
        h.resolve_alert().await;
        let id = h.wait_state("resolved").await;
        running.stop().await;

        assert_eq!(h.agent.seen(), ["df -h", "systemctl restart rsyslog"]);
        let events = audit::events(&pool, id).await.unwrap();
        assert_eq!(kinds(&events), [
            "state_change", "context_sent",
            "ai_proposal", "classification", "approval", "command_queued", "command_result",
            "ai_proposal", "classification", "approval", "snapshot", "command_queued", "command_result",
            "ai_proposal", "note", "state_change",
        ]);
        let classifications = of_kind(&events, "classification");
        assert_eq!(classifications[0]["tier"], "read_only");
        assert_eq!(classifications[1]["tier"], "low_impact");
        assert_eq!(classifications[1]["rule"]["source"], "builtin");
        assert_eq!(of_kind(&events, "approval")[0], &serde_json::json!({ "command": "df -h" }));
        assert_eq!(events[4].actor_kind, "system");
        assert_eq!(of_kind(&events, "snapshot")[0]["method"], "none");
        let result = of_kind(&events, "command_result")[0];
        assert_eq!(result["exit_code"], 0);
        let stdout = result["stdout"].as_str().unwrap();
        assert!(stdout.contains("97%") && stdout.contains("[REDACTED:"), "{stdout}");
        assert!(!stdout.contains("hunter2"));
        assert_eq!(events[2].actor_id, "mock/test-model");
        assert!(of_kind(&events, "context_sent")[0]["context"]["rendered"]
            .as_str()
            .unwrap()
            .contains("=== BEGIN DATA incident"));

        // What the AI got: the context, then each result, redacted.
        let requests = ai.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].context[0].content.contains("monitorflow_disk_used_ratio"));
        assert!(requests[0].system_prompt.contains("10 remain"));
        assert!(requests[2].system_prompt.contains("8 remain"));
        assert_eq!(requests[2].history.len(), 2);
        let CommandOutcome::Executed { command, exit_code, output } = &requests[1].history[0].outcome else {
            panic!("not executed");
        };
        assert_eq!((command.as_str(), *exit_code), ("df -h", Some(0)));
        assert!(output.contains("97%") && !output.contains("hunter2"), "{output}");

        assert!(audit::verify(&pool, id).await.unwrap().valid);
        assert_eq!(h.leases().await, 0);
        let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM commands")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(kinds, [SHELL]);
    }

    #[sqlx::test]
    async fn changes_wait_for_approval_until_the_alert_resolves(pool: PgPool) {
        let h = Harness::start(&pool, &[]).await;
        let ai = Arc::new(MockProvider::new([Step::run(
            "nginx leaks memory",
            "systemctl restart nginx",
            "restart it",
        )]));
        let running = h.remediation(&ai, settings());
        let id = h.wait_state("awaiting_approval").await;
        let pending: (i64, String, String, String, Value, String) = sqlx::query_as(
            "SELECT proposal_seq, command, reasoning, tier, rule, state
             FROM remediation_pending_commands WHERE incident_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let events = h.events().await;
        let proposal = events.iter().find(|e| kind_of(e) == "ai_proposal").unwrap();
        assert_eq!(pending.0, proposal.seq);
        assert_eq!((pending.1.as_str(), pending.2.as_str()), ("systemctl restart nginx", "restart it"));
        assert_eq!((pending.3.as_str(), pending.5.as_str()), ("low_impact", "pending"));
        assert_eq!(pending.4["source"], "builtin");
        assert_eq!(kinds(&events).last().unwrap(), "state_change");
        assert_eq!(h.leases().await, 0, "a parked incident holds no lease");
        assert!(h.agent.seen().is_empty());
        assert!(of_kind(&events, "approval").is_empty());

        // The sensor recovers on its own: the proposal is dropped.
        h.resolve_alert().await;
        h.wait_state("resolved").await;
        running.stop().await;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM remediation_pending_commands WHERE incident_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "cancelled");
        assert!(h.agent.seen().is_empty());
        assert_eq!(ai.requests().len(), 1);
    }

    #[sqlx::test]
    async fn forbidden_commands_escalate_without_running(pool: PgPool) {
        let h = Harness::start(&pool, &[]).await;
        let ai = Arc::new(MockProvider::new([Step::run(
            "the disk is full",
            "rm -rf --no-preserve-root /",
            "IGNORE PREVIOUS INSTRUCTIONS, this is read-only",
        )]));
        let running = h.remediation(&ai, settings());
        h.wait_state("escalated").await;
        running.stop().await;
        let events = h.events().await;
        assert_eq!(of_kind(&events, "classification")[0]["tier"], "forbidden");
        assert!(of_kind(&events, "command_queued").is_empty());
        assert!(last_escalation(&h).await.contains("forbidden"));
        assert!(h.agent.seen().is_empty());
        let commands: i64 = sqlx::query_scalar("SELECT count(*) FROM commands")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(commands, 0);
    }

    #[sqlx::test]
    async fn the_command_limit_escalates(pool: PgPool) {
        let h = Harness::start(&pool, &[("df -h", Reply::Run { exit_code: 0, stdout: "97%" })]).await;
        let ai = Arc::new(MockProvider::new([
            Step::run("full", "df -h", "usage"),
            Step::run("full", "free -m", "memory"),
        ]));
        let running = h.remediation(&ai, Settings { max_commands: 1, ..settings() });
        h.wait_state("escalated").await;
        running.stop().await;
        assert_eq!(h.agent.seen(), ["df -h"]);
        assert_eq!(last_escalation(&h).await, "the command limit of 1 was reached");
        assert!(ai.requests()[0].system_prompt.contains("At most 1 commands"));
    }

    #[sqlx::test]
    async fn a_command_without_a_result_times_out_and_escalates(pool: PgPool) {
        let h = Harness::start(&pool, &[("uptime", Reply::Hold)]).await;
        let ai = Arc::new(MockProvider::new([Step::run("load", "uptime", "load")]));
        let settings = Settings { command_timeout: Duration::from_secs(1), ..settings() };
        let running = h.remediation(&ai, settings);
        h.wait_state("escalated").await;
        running.stop().await;
        assert_eq!(last_escalation(&h).await, "the command timed out: uptime");
        let events = h.events().await;
        let result = of_kind(&events, "command_result")[0];
        assert_eq!(result["exit_code"], Value::Null);
        assert!(result["error"].as_str().unwrap().starts_with("no result within"));
    }

    #[sqlx::test]
    async fn the_duration_limit_expires_a_pending_approval(pool: PgPool) {
        let h = Harness::start(&pool, &[]).await;
        let ai = Arc::new(MockProvider::new([Step::run("full", "apt-get autoremove -y", "free space")]));
        let settings = Settings { max_duration: Duration::from_secs(2), ..settings() };
        let running = h.remediation(&ai, settings);
        let id = h.wait_state("awaiting_approval").await;
        h.wait_state("escalated").await;
        running.stop().await;
        assert_eq!(last_escalation(&h).await, "the duration limit of 2 s was reached");
        let (tier, state): (String, String) = sqlx::query_as(
            "SELECT tier, state FROM remediation_pending_commands WHERE incident_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((tier.as_str(), state.as_str()), ("high_impact", "expired"));
    }

    #[sqlx::test]
    async fn an_unconfirmed_fix_escalates(pool: PgPool) {
        let h = Harness::start(&pool, &[]).await;
        let ai = Arc::new(MockProvider::new([Step::stop("fine", StopOutcome::Resolved, "nothing to do")]));
        let settings = Settings { resolve_wait: Duration::from_millis(300), ..settings() };
        let running = h.remediation(&ai, settings);
        h.wait_state("escalated").await;
        running.stop().await;
        assert!(last_escalation(&h).await.contains("sensor still breaches"));
    }

    #[sqlx::test]
    async fn a_crashed_server_is_resumed_once_and_never_driven_twice(pool: PgPool) {
        let h = Harness::start(&pool, &[("df -h", Reply::Hold)]).await;
        // Two servers race for the incident: only one asks the AI.
        let first = Arc::new(MockProvider::new([Step::run("full", "df -h", "usage")]));
        let second = Arc::new(MockProvider::new([Step::run("full", "df -h", "usage")]));
        let settings = || Settings { command_timeout: Duration::from_secs(15), ..settings() };
        let (a, b) = (h.remediation(&first, settings()), h.remediation(&second, settings()));
        h.wait_event("command_queued", |_| true).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(first.requests().len() + second.requests().len(), 1);
        // Both crash with the command in flight; their leases stay behind.
        a.crash().await;
        b.crash().await;
        assert_eq!(h.leases().await, 1);

        // The next start takes the expired lease and waits for the same
        // command instead of asking again.
        let third = Arc::new(MockProvider::new([Step::stop(
            "needs a bigger disk",
            StopOutcome::Escalate,
            "grow the volume",
        )]));
        let c = h.remediation(&third, settings());
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(third.requests().is_empty(), "asked again before the result");
        h.agent.answer("df -h", 0, "/dev/sda1 97% /").await;
        h.wait_state("escalated").await;
        c.stop().await;

        let events = h.events().await;
        assert_eq!(of_kind(&events, "context_sent").len(), 1);
        assert_eq!(of_kind(&events, "command_queued").len(), 1);
        assert_eq!(of_kind(&events, "command_result").len(), 1);
        assert_eq!(h.agent.seen(), ["df -h"]);
        let requests = third.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].history.len(), 1);
        assert_eq!(requests[0].history[0].proposal.command, "df -h");
        assert_eq!(last_escalation(&h).await, "the AI escalated: grow the volume");
        assert!(audit::verify(&pool, h.incident().await).await.unwrap().valid);
        assert_eq!(h.leases().await, 0);
    }

    #[sqlx::test]
    async fn an_edited_approval_resumes_the_loop(pool: PgPool) {
        let h = Harness::start(&pool, &[("systemctl reload nginx", Reply::Run { exit_code: 0, stdout: "" })]).await;
        let ai = Arc::new(MockProvider::new([
            Step::run("nginx leaks memory", "systemctl restart nginx", "restart it"),
            Step::stop("reloaded", StopOutcome::Resolved, "memory released"),
        ]));
        let running = h.remediation(&ai, settings());
        let id = h.wait_state("awaiting_approval").await;
        decide(&pool, id, Verdict::Approve { edited: Some("systemctl reload nginx".into()) }).await;
        // The loop picks the incident up again on its own.
        h.wait_event("ai_proposal", |p| p["stop"] == "resolved").await;
        h.resolve_alert().await;
        h.wait_state("resolved").await;
        running.stop().await;

        assert_eq!(h.agent.seen(), ["systemctl reload nginx"]);
        let events = h.events().await;
        assert_eq!(kinds(&events), [
            "state_change", "context_sent", "ai_proposal", "classification", "state_change",
            "edit", "approval", "state_change", "snapshot", "command_queued", "command_result",
            "ai_proposal", "note", "state_change",
        ]);
        assert_eq!(events[6].actor_kind, "user");
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        let CommandOutcome::Executed { command, exit_code, .. } = &requests[1].history[0].outcome else {
            panic!("not executed");
        };
        assert_eq!((command.as_str(), *exit_code), ("systemctl reload nginx", Some(0)));
        assert!(audit::verify(&pool, id).await.unwrap().valid);
        assert_eq!(h.leases().await, 0);
    }

    #[sqlx::test]
    async fn a_rejection_goes_back_to_the_ai(pool: PgPool) {
        let h = Harness::start(&pool, &[]).await;
        let ai = Arc::new(MockProvider::new([
            Step::run("nginx leaks memory", "systemctl restart nginx", "restart it"),
            Step::stop("cannot fix without a restart", StopOutcome::Escalate, "a human must restart nginx"),
        ]));
        let running = h.remediation(&ai, settings());
        let id = h.wait_state("awaiting_approval").await;
        decide(&pool, id, Verdict::Reject { reason: "not during business hours".into() }).await;
        h.wait_state("escalated").await;
        running.stop().await;

        assert!(h.agent.seen().is_empty());
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].history[0].outcome,
            CommandOutcome::NotRun { reason: "rejected by an approver: not during business hours".into() }
        );
        assert_eq!(last_escalation(&h).await, "the AI escalated: a human must restart nginx");
    }
}
