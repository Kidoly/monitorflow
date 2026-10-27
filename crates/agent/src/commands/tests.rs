use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::command;
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use tempfile::TempDir;
use tokio::sync::Notify;

use super::*;
use crate::shutdown::{self, Trigger};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Echoes its payload; `"fail"` exits 2, `"block"` waits for `release`.
#[derive(Default)]
struct Echo {
    runs: AtomicUsize,
    release: Notify,
}

#[async_trait::async_trait]
impl Handler for Echo {
    async fn run(&self, payload: &[u8]) -> Output {
        self.runs.fetch_add(1, Ordering::SeqCst);
        match payload {
            b"block" => self.release.notified().await,
            b"bad" => return Output::rejected("invalid payload"),
            b"fail" => {
                return Output {
                    exit_code: 2,
                    stderr: b"boom".to_vec(),
                    ..Output::default()
                }
            }
            _ => {}
        }
        Output {
            stdout: payload.to_vec(),
            ..Output::default()
        }
    }
}

struct Setup {
    data: TempDir,
    _ca_dir: TempDir,
    ca: CertificateAuthority,
    echo: Arc<Echo>,
    stream: StreamEnd,
    trigger: Trigger,
    task: JoinHandle<()>,
}

impl Setup {
    fn new() -> Self {
        Self::with(TempDir::new().unwrap(), DispatcherSettings::default())
    }

    fn with(data: TempDir, settings: DispatcherSettings) -> Self {
        Self::build(data, settings, None)
    }

    fn build(
        data: TempDir,
        settings: DispatcherSettings,
        resumer: Option<Arc<dyn Resumer>>,
    ) -> Self {
        let ca_dir = TempDir::new().unwrap();
        let ca = CertificateAuthority::open_or_create(ca_dir.path(), CaOptions::default()).unwrap();
        let ca_der = pem::parse(ca.ca_cert_pem()).unwrap().into_contents();
        let echo = Arc::new(Echo::default());
        let mut handlers = Handlers::default();
        handlers.register("echo", echo.clone());
        let mut dispatcher = Dispatcher::new(
            "web-01",
            vec![CertificateDer::from(ca_der)],
            handlers,
            Journal::in_dir(data.path()),
            settings,
        );
        if let Some(resumer) = resumer {
            dispatcher = dispatcher.with_resumer(resumer);
        }
        let (stream, end) = link();
        let (trigger, stop) = shutdown::channel();
        let task = tokio::spawn(dispatcher.run(end, stop));
        Self {
            data,
            _ca_dir: ca_dir,
            ca,
            echo,
            stream,
            trigger,
            task,
        }
    }

    fn command(&self, id: &str, kind: &str, payload: &[u8]) -> Command {
        let mut command = Command {
            command_id: id.into(),
            kind: kind.into(),
            payload: payload.to_vec(),
            issued_at_unix_ms: unix_ms(),
            expires_at_unix_ms: unix_ms() + 60_000,
            agent_id: "web-01".into(),
            signature: Vec::new(),
        };
        self.sign(&mut command);
        command
    }

    fn sign(&self, command: &mut Command) {
        command.signature = self
            .ca
            .sign_command(&command::signed_bytes(command))
            .unwrap();
    }

    async fn send(&self, command: Command) {
        self.stream.commands.send(command).await.unwrap();
    }

    async fn result(&mut self) -> CommandResult {
        tokio::time::timeout(TIMEOUT, self.stream.results.recv())
            .await
            .expect("no result in time")
            .unwrap()
    }

    /// The final result of `id`, after its `RUNNING` report.
    async fn ran(&mut self, id: &str) -> CommandResult {
        let running = self.result().await;
        assert_eq!(
            (running.command_id.as_str(), running.status()),
            (id, CommandStatus::Running)
        );
        self.result().await
    }

    fn runs(&self) -> usize {
        self.echo.runs.load(Ordering::SeqCst)
    }

    async fn stop(self) -> TempDir {
        self.trigger.fire();
        tokio::time::timeout(TIMEOUT, self.task)
            .await
            .unwrap()
            .unwrap();
        self.data
    }
}

#[tokio::test]
async fn a_command_runs_once_and_a_redelivery_gets_the_stored_result() {
    let mut s = Setup::new();
    let command = s.command("c1", "echo", b"hello");
    s.send(command.clone()).await;
    let done = s.ran("c1").await;
    assert_eq!(done.status(), CommandStatus::Succeeded);
    assert_eq!((done.exit_code, done.stdout.as_slice()), (0, &b"hello"[..]));
    assert!(done.finished_at_unix_ms >= done.started_at_unix_ms && done.started_at_unix_ms > 0);

    s.send(command.clone()).await;
    assert_eq!(s.result().await, done);
    assert_eq!(s.runs(), 1);

    // Across a restart too.
    let data = s.stop().await;
    let mut s = Setup::with(data, DispatcherSettings::default());
    let mut again = command;
    s.sign(&mut again);
    s.send(again).await;
    let stored = s.result().await;
    assert_eq!(
        (stored.command_id.as_str(), stored.stdout.as_slice()),
        ("c1", &b"hello"[..])
    );
    assert_eq!(s.runs(), 0);
}

#[tokio::test]
async fn failures_and_refusals_are_reported_without_running() {
    let mut s = Setup::new();
    s.send(s.command("f", "echo", b"fail")).await;
    let failed = s.ran("f").await;
    assert_eq!(
        (failed.status(), failed.exit_code, failed.stderr.as_slice()),
        (CommandStatus::Failed, 2, &b"boom"[..])
    );
    s.send(s.command("bad", "echo", b"bad")).await;
    let bad = s.ran("bad").await;
    assert_eq!(
        (bad.status(), bad.error.as_str()),
        (CommandStatus::Rejected, "invalid payload")
    );
    assert_eq!(s.runs(), 2);

    let mut forged = s.command("x1", "echo", b"");
    forged.payload = b"other".to_vec();
    let mut other_agent = s.command("x2", "echo", b"");
    other_agent.agent_id = "web-02".into();
    s.sign(&mut other_agent);
    let mut expired = s.command("x3", "echo", b"");
    expired.expires_at_unix_ms = unix_ms() - 1;
    s.sign(&mut expired);
    let unknown = s.command("x4", "reboot", b"");
    let bad_id = s.command("../x5", "echo", b"");
    let cases = [
        (forged, CommandStatus::Rejected, "signature"),
        (other_agent, CommandStatus::Rejected, "another agent"),
        (expired, CommandStatus::Expired, "expired"),
        (
            unknown,
            CommandStatus::Rejected,
            "unknown command kind `reboot`",
        ),
        (bad_id, CommandStatus::Rejected, "invalid command ID"),
    ];
    for (command, status, error) in cases {
        let id = command.command_id.clone();
        s.send(command).await;
        let result = s.result().await;
        assert_eq!((result.command_id.clone(), result.status()), (id, status));
        assert!(result.error.contains(error), "{}", result.error);
    }
    assert_eq!(s.runs(), 2);
    // Refused commands leave no journal entry.
    let journal = Journal::in_dir(s.data.path());
    assert_eq!(journal.get("x1").await.unwrap(), None);
}

#[tokio::test]
async fn commands_run_one_at_a_time_and_duplicates_of_a_running_one_get_running() {
    let mut s = Setup::new();
    let blocking = s.command("a", "echo", b"block");
    s.send(blocking.clone()).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    s.send(s.command("b", "echo", b"second")).await;
    s.send(s.command("b", "echo", b"second")).await;
    s.send(blocking).await;
    let duplicate = s.result().await;
    assert_eq!(
        (duplicate.command_id.as_str(), duplicate.status()),
        ("a", CommandStatus::Running)
    );
    assert_eq!(s.runs(), 1);

    s.echo.release.notify_one();
    assert_eq!(s.result().await.status(), CommandStatus::Succeeded);
    let second = s.ran("b").await;
    assert_eq!(second.stdout, b"second");
    // The copy of "b" that waited was dropped, not run again.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), s.stream.results.recv())
            .await
            .is_err()
    );
    assert_eq!(s.runs(), 2);
}

#[tokio::test]
async fn a_command_interrupted_by_a_restart_is_failed_not_run_again() {
    let mut s = Setup::new();
    let command = s.command("c1", "echo", b"block");
    s.send(command.clone()).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    let data = s.stop().await;

    let mut s = Setup::with(data, DispatcherSettings::default());
    let mut again = command;
    s.sign(&mut again);
    s.send(again.clone()).await;
    let failed = s.result().await;
    assert_eq!(
        (failed.status(), failed.error.as_str()),
        (CommandStatus::Failed, "interrupted by an agent restart")
    );
    // That answer is now the stored result.
    s.send(again).await;
    assert_eq!(s.result().await, failed);
    assert_eq!(s.runs(), 0);
}

/// Claims `c1`, answering once `release` is notified.
#[derive(Default)]
struct ResumeC1 {
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Resumer for ResumeC1 {
    async fn resume(
        &self,
        command_id: &str,
    ) -> Option<Pin<Box<dyn Future<Output = Output> + Send>>> {
        let release = self.release.clone();
        (command_id == "c1").then(|| {
            Box::pin(async move {
                release.notified().await;
                Output {
                    stdout: b"resumed".to_vec(),
                    ..Output::default()
                }
            }) as Pin<Box<dyn Future<Output = Output> + Send>>
        })
    }
}

#[tokio::test]
async fn a_resumer_finishes_the_commands_it_claims_after_a_restart() {
    let mut s = Setup::new();
    s.send(s.command("c1", "echo", b"block")).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    let data = s.stop().await;
    let resumer = Arc::new(ResumeC1::default());
    let mut s = Setup::build(data, DispatcherSettings::default(), Some(resumer.clone()));
    let mut again = s.command("c1", "echo", b"block");
    s.sign(&mut again);
    s.send(again.clone()).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    // A copy while it waits is answered running.
    s.send(again.clone()).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    resumer.release.notify_one();
    let done = s.result().await;
    assert_eq!(
        (done.status(), done.stdout.as_slice()),
        (CommandStatus::Succeeded, &b"resumed"[..])
    );
    // Stored like any result.
    s.send(again).await;
    assert_eq!(s.result().await, done);
    assert_eq!(s.runs(), 0);
}

#[tokio::test]
async fn unclaimed_interrupted_commands_still_fail() {
    let mut s = Setup::new();
    s.send(s.command("c9", "echo", b"block")).await;
    assert_eq!(s.result().await.status(), CommandStatus::Running);
    let data = s.stop().await;
    let resumer: Arc<dyn Resumer> = Arc::new(ResumeC1::default());
    let mut s = Setup::build(data, DispatcherSettings::default(), Some(resumer));
    s.send(s.command("c9", "echo", b"block")).await;
    let failed = s.result().await;
    assert_eq!(failed.error, "interrupted by an agent restart");
}

#[tokio::test]
async fn a_handler_past_the_timeout_fails() {
    let settings = DispatcherSettings {
        run_timeout: Duration::from_millis(50),
    };
    let mut s = Setup::with(TempDir::new().unwrap(), settings);
    s.send(s.command("slow", "echo", b"block")).await;
    let result = s.ran("slow").await;
    assert_eq!(result.status(), CommandStatus::Failed);
    assert!(result.error.starts_with("timed out"), "{}", result.error);
}

#[test]
fn output_status_and_truncation() {
    assert_eq!(Output::default().status(), CommandStatus::Succeeded);
    let error = Output {
        error: Some("x".into()),
        ..Output::default()
    };
    assert_eq!(error.status(), CommandStatus::Failed);
    assert_eq!(Output::rejected("no").status(), CommandStatus::Rejected);
    let mut long = vec![b'a'; MAX_OUTPUT];
    long.push(b'z');
    let cut = tail(long);
    assert_eq!((cut.len(), cut.last()), (MAX_OUTPUT, Some(&b'z')));
}
