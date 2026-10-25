//! The agent's side of Windows Update (ADR-0049): package reports and
//! `apply_updates` through the update service. Written against
//! [`UpdateService`] so it is tested with a fake; the pipe client is in
//! `pipe`.

use std::sync::Arc;

use monitorflow_common::proto::v1::{ApplyUpdates, Package};
use prost::Message;

use super::{describe, to_packages, Installed, Reply, Request, Scan, Status};
use crate::commands::{Handler, Output};
use crate::packages::{PackageError, PackageManager, Reports};

/// Sends one request to the update service and returns its reply.
#[async_trait::async_trait]
pub trait UpdateService: Send + Sync + 'static {
    async fn call(&self, request: &Request) -> Result<Reply, PackageError>;
}

/// The reply's error, or one for a reply of the wrong kind.
fn unexpected(reply: Reply) -> PackageError {
    match reply {
        Reply::Error(error) => PackageError::Helper(format!("update service: {error}")),
        other => PackageError::Helper(format!("update service: unexpected reply {other:?}")),
    }
}

async fn scan(service: &dyn UpdateService) -> Result<Scan, PackageError> {
    match service.call(&Request::Scan).await? {
        Reply::Updates(scan) => Ok(scan),
        other => Err(unexpected(other)),
    }
}

async fn status(service: &dyn UpdateService) -> Result<Status, PackageError> {
    match service.call(&Request::Status).await? {
        Reply::Status(status) => Ok(status),
        other => Err(unexpected(other)),
    }
}

/// Windows Update as the agent's package manager: pending and installed
/// updates by KB number.
pub struct WindowsUpdates {
    service: Arc<dyn UpdateService>,
}

impl WindowsUpdates {
    pub fn new(service: Arc<dyn UpdateService>) -> Self {
        Self { service }
    }
}

#[async_trait::async_trait]
impl PackageManager for WindowsUpdates {
    async fn inventory(&self) -> Result<Vec<Package>, PackageError> {
        Ok(to_packages(&scan(self.service.as_ref()).await?.updates))
    }

    async fn reboot_required(&self) -> Result<bool, PackageError> {
        Ok(status(self.service.as_ref()).await?.reboot_required)
    }
}

/// `apply_updates` on Windows: the update service installs, the agent
/// reports, then asks for a restart when the command allows it and
/// Windows needs one.
pub struct WindowsApplyHandler {
    service: Arc<dyn UpdateService>,
    reports: Reports,
}

impl WindowsApplyHandler {
    pub fn new(service: Arc<dyn UpdateService>, reports: Reports) -> Self {
        Self { service, reports }
    }

    async fn install(&self, request: &Request) -> Result<Installed, PackageError> {
        match self.service.call(request).await? {
            Reply::Installed(installed) => Ok(installed),
            other => Err(unexpected(other)),
        }
    }
}

/// `install security KB1 KB2`, for the output.
fn line(request: &Request) -> String {
    match request {
        Request::Install { kind, updates } if updates.is_empty() => {
            format!("install {}", kind.as_str())
        }
        Request::Install { kind, updates } => {
            format!("install {} {}", kind.as_str(), updates.join(" "))
        }
        other => format!("{other:?}"),
    }
}

#[async_trait::async_trait]
impl Handler for WindowsApplyHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        let Ok(apply) = ApplyUpdates::decode(payload) else {
            return Output::rejected("invalid ApplyUpdates payload");
        };
        let request = Request::install(&apply);
        if let Err(error) = request.validate() {
            return Output::rejected(error.to_string());
        }
        let mut output = Output::default();
        output
            .stdout
            .extend_from_slice(format!("$ windows update {}\n", line(&request)).as_bytes());
        let result = self.install(&request).await;
        // Whatever happened, the server gets the state of the host.
        self.reports.collect().await;
        let installed = match result {
            Ok(installed) => installed,
            Err(error) => {
                output.exit_code = -1;
                output.error = Some(error.to_string());
                return output;
            }
        };
        let (text, error) = describe(&installed);
        output.stdout.extend_from_slice(text.as_bytes());
        tracing::info!(
            updates = installed.results.len(),
            failed = error.is_some(),
            reboot_required = installed.reboot_required,
            "windows updates installed"
        );
        if let Some(error) = error {
            output.exit_code = 1;
            output.error = Some(error);
            return output;
        }
        if apply.allow_reboot && installed.reboot_required {
            match self.service.call(&Request::Reboot).await {
                Ok(Reply::Rebooting(())) => {
                    output
                        .stdout
                        .extend_from_slice(b"restarting in one minute\n");
                }
                Ok(other) => {
                    output.exit_code = -1;
                    output.error = Some(format!(
                        "updates installed, reboot failed: {}",
                        unexpected(other)
                    ));
                }
                Err(error) => {
                    output.exit_code = -1;
                    output.error = Some(format!("updates installed, reboot failed: {error}"));
                }
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::{InstallResult, Kind, Outcome, Update};
    use super::*;

    /// Answers each request kind with a fixed reply and records requests.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Request>>,
        install: Option<Reply>,
        reboot: Option<Reply>,
    }

    #[async_trait::async_trait]
    impl UpdateService for Fake {
        async fn call(&self, request: &Request) -> Result<Reply, PackageError> {
            self.calls.lock().unwrap().push(request.clone());
            let reply = match request {
                Request::Scan => Some(Reply::Updates(Scan {
                    updates: vec![Update {
                        name: "KB5034441".into(),
                        revision: 200,
                        ..Update::default()
                    }],
                    reboot_required: false,
                })),
                Request::Status => Some(Reply::Status(Status {
                    reboot_required: true,
                })),
                Request::Install { .. } => self.install.clone(),
                Request::Reboot => self.reboot.clone(),
            };
            reply.ok_or_else(|| PackageError::Helper("the update service is not running".into()))
        }
    }

    fn installed(results: &[(&str, Outcome)], reboot_required: bool) -> Reply {
        Reply::Installed(Installed {
            results: results
                .iter()
                .map(|(name, result)| InstallResult {
                    name: name.to_string(),
                    title: format!("{name} title"),
                    result: *result,
                    hresult: if result.succeeded() { 0 } else { -2145124318 },
                })
                .collect(),
            not_pending: Vec::new(),
            reboot_required,
        })
    }

    async fn run(fake: Fake, apply: ApplyUpdates) -> (Output, Vec<Request>, bool) {
        let fake = Arc::new(fake);
        let manager = Arc::new(WindowsUpdates::new(fake.clone()));
        let (reports, latest) = Reports::new(manager);
        let handler = WindowsApplyHandler::new(fake.clone(), reports);
        let output = handler.run(&apply.encode_to_vec()).await;
        let reported = latest.borrow().is_some();
        let calls = fake.calls.lock().unwrap().clone();
        (output, calls, reported)
    }

    #[tokio::test]
    async fn reports_list_updates_and_the_pending_restart() {
        let fake = Arc::new(Fake::default());
        let manager = WindowsUpdates::new(fake);
        let packages = manager.inventory().await.unwrap();
        assert_eq!(packages[0].name, "KB5034441");
        assert_eq!(packages[0].upgradable_version, "200");
        assert!(manager.reboot_required().await.unwrap());
    }

    /// Unreachable for its first `down` calls.
    struct Flaky {
        down: Mutex<usize>,
        fake: Fake,
    }

    #[async_trait::async_trait]
    impl UpdateService for Flaky {
        async fn call(&self, request: &Request) -> Result<Reply, PackageError> {
            let was_down = {
                let mut down = self.down.lock().unwrap();
                let was = *down > 0;
                *down = down.saturating_sub(1);
                was
            };
            if was_down {
                return Err(PackageError::Helper("not running yet".into()));
            }
            self.fake.call(request).await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_report_is_retried_after_five_minutes() {
        let service = Arc::new(Flaky {
            down: Mutex::new(1),
            fake: Fake::default(),
        });
        let (reports, latest) = Reports::new(Arc::new(WindowsUpdates::new(service)));
        let (trigger, stop) = crate::shutdown::channel();
        let six_hours = std::time::Duration::from_secs(6 * 3600);
        let task = tokio::spawn(reports.run(six_hours, stop));
        tokio::time::sleep(crate::packages::REPORT_RETRY / 2).await;
        assert!(latest.borrow().is_none());
        tokio::time::sleep(crate::packages::REPORT_RETRY).await;
        assert!(latest.borrow().is_some());
        trigger.fire();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn installs_report_then_restart_only_when_allowed() {
        let apply = |allow_reboot| ApplyUpdates {
            packages: vec!["KB5034441".into()],
            security_only: true,
            allow_reboot,
        };
        let fake = || Fake {
            install: Some(installed(&[("KB5034441", Outcome::Succeeded)], true)),
            reboot: Some(Reply::Rebooting(())),
            ..Fake::default()
        };
        let (output, calls, reported) = run(fake(), apply(false)).await;
        assert_eq!((output.exit_code, output.error.as_deref()), (0, None));
        assert!(reported);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "$ windows update install security KB5034441\n\
             KB5034441 succeeded: KB5034441 title\n\
             a restart is required\n"
        );
        let install = Request::Install {
            kind: Kind::Security,
            updates: vec!["KB5034441".into()],
        };
        assert_eq!(calls, [install.clone(), Request::Scan, Request::Status]);

        let (output, calls, _) = run(fake(), apply(true)).await;
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("restarting in one minute\n"));
        assert_eq!(calls.last(), Some(&Request::Reboot));
    }

    #[tokio::test]
    async fn failures_name_the_updates_and_never_restart() {
        let fake = Fake {
            install: Some(installed(
                &[("KB1", Outcome::Succeeded), ("KB2", Outcome::Failed)],
                true,
            )),
            reboot: Some(Reply::Rebooting(())),
            ..Fake::default()
        };
        let apply = ApplyUpdates {
            allow_reboot: true,
            ..ApplyUpdates::default()
        };
        let (output, calls, reported) = run(fake, apply).await;
        assert_eq!(output.exit_code, 1);
        assert_eq!(
            output.error.as_deref(),
            Some("1 of 2 updates did not install: KB2 (0x80240022)")
        );
        assert!(reported);
        assert!(!calls.contains(&Request::Reboot));
    }

    #[tokio::test]
    async fn an_unreachable_service_fails_and_bad_names_are_refused() {
        let (output, _, _) = run(Fake::default(), ApplyUpdates::default()).await;
        assert_eq!(output.exit_code, -1);
        assert!(!output.rejected);
        assert_eq!(
            output.error.as_deref(),
            Some("the update service is not running")
        );

        let fake = Fake {
            install: Some(Reply::Error("search failed: 0x8024402c".into())),
            ..Fake::default()
        };
        let (output, _, _) = run(fake, ApplyUpdates::default()).await;
        assert_eq!(
            output.error.as_deref(),
            Some("update service: search failed: 0x8024402c")
        );

        let apply = ApplyUpdates {
            packages: vec!["openssl".into()],
            ..ApplyUpdates::default()
        };
        let (output, calls, _) = run(Fake::default(), apply).await;
        assert!(output.rejected);
        assert_eq!(
            output.error.as_deref(),
            Some("`openssl` is not a KB number or update ID")
        );
        assert!(calls.is_empty());
    }
}
