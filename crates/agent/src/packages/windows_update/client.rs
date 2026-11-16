//! The agent's side of Windows Update (ADR-0049) and winget (ADR-0139):
//! package reports and `apply_updates` through the update service.
//! Written against [`UpdateService`] so it is tested with a fake; the pipe
//! client is in `pipe`.

use std::sync::Arc;

use monitorflow_common::proto::v1::{
    ApplyUpdates, Command, Package, ThirdPartySelection, WindowsUpdateSource,
};
use monitorflow_common::third_party;
use prost::Message;

use super::{describe, to_packages, Installed, Reply, Request, Scan, Status, Step};
use crate::commands::{Handler, Output};
use crate::packages::winget::{self, Apps};
use crate::packages::{PackageError, PackageManager, Reports, ThirdParty};

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

    /// Read by the agent itself: anyone may read the policy keys, so the
    /// update service needs no new request.
    async fn windows_update_source(&self) -> Option<WindowsUpdateSource> {
        #[cfg(windows)]
        {
            let read = tokio::task::spawn_blocking(super::source::read).await;
            match read {
                Ok(Ok(source)) => Some(source),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "cannot read the Windows Update policy; no update source in the report");
                    None
                }
                Err(error) => {
                    tracing::warn!(%error, "reading the Windows Update policy panicked");
                    None
                }
            }
        }
        #[cfg(not(windows))]
        None
    }

    /// winget's apps, listed by the update service as LocalSystem
    /// (ADR-0139). Its status is always sent, whatever happened.
    async fn third_party(&self) -> ThirdParty {
        let apps = winget_scan(self.service.as_ref()).await;
        let (status, packages) = winget::to_report(&apps);
        ThirdParty {
            packages,
            managers: vec![status],
        }
    }
}

/// An update service from before ADR-0139 refuses the request.
pub const SERVICE_TOO_OLD: &str =
    "the update service is too old for winget; it updates with the agent";

async fn winget_scan(service: &dyn UpdateService) -> Apps {
    match service.call(&Request::WingetScan).await {
        Ok(Reply::Apps(apps)) => apps,
        Ok(Reply::Error(error)) if error.contains("unknown variant `winget_scan`") => {
            Apps::unusable(SERVICE_TOO_OLD)
        }
        Ok(other) => Apps::unusable(&unexpected(other).to_string()),
        Err(error) => Apps::unusable(&error.to_string()),
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

/// `winget upgrade A B`, or `all`, for the output.
fn winget_line(selection: &ThirdPartySelection) -> String {
    if selection.packages.is_empty() {
        "upgrade all".into()
    } else {
        format!("upgrade {}", selection.packages.join(" "))
    }
}

#[async_trait::async_trait]
impl Handler for WindowsApplyHandler {
    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("apply_updates needs its signed command")
    }

    /// Windows updates unless the command skips the OS, then winget's apps
    /// when it selects them (ADR-0138, ADR-0139), stopping at the first
    /// failure; a report; then a restart when allowed and needed.
    async fn run_command(&self, command: &Command) -> Output {
        let Ok(apply) = ApplyUpdates::decode(command.payload.as_slice()) else {
            return Output::rejected("invalid ApplyUpdates payload");
        };
        if let Err(error) = third_party::check(&apply, Some("windows")) {
            return Output::rejected(error.to_string());
        }
        let os = (!apply.skip_os).then(|| Request::install(&apply));
        if let Some(Err(error)) = os.as_ref().map(Request::validate) {
            return Output::rejected(error.to_string());
        }
        // `check` leaves only winget on Windows.
        let apps = apply.third_party.first();
        let mut steps: Vec<(String, Step)> = Vec::new();
        if let Some(request) = &os {
            steps.push((format!("windows update {}", line(request)), Step::Install));
        }
        if let Some(selection) = apps {
            steps.push((format!("winget {}", winget_line(selection)), Step::Winget));
        }
        let mut output = Output::default();
        let mut reboot_required = false;
        let mut failure = None;
        for (shown, step) in steps {
            output
                .stdout
                .extend_from_slice(format!("$ {shown}\n").as_bytes());
            // The service checks the signed command and builds the same
            // request from it (ADR-0108).
            let installed = match self.install(&Request::signed(command, step, None)).await {
                Ok(installed) => installed,
                Err(error) => {
                    failure = Some((-1, error.to_string()));
                    break;
                }
            };
            let (text, error) = describe(&installed);
            output.stdout.extend_from_slice(text.as_bytes());
            reboot_required |= installed.reboot_required;
            tracing::info!(
                step = ?step,
                updates = installed.results.len(),
                failed = error.is_some(),
                reboot_required = installed.reboot_required,
                "updates installed"
            );
            if let Some(error) = error {
                failure = Some((1, error));
                break;
            }
        }
        // Whatever happened, the server gets the state of the host.
        self.reports.collect().await;
        if let Some((exit_code, error)) = failure {
            output.exit_code = exit_code;
            output.error = Some(error);
            return output;
        }
        if apply.allow_reboot && reboot_required {
            let reboot = Request::signed(command, Step::Reboot, None);
            match self.service.call(&reboot).await {
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

    use super::super::{InstallResult, Outcome, Update};
    use super::*;

    /// Answers each request kind with a fixed reply and records requests.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Request>>,
        install: Option<Reply>,
        reboot: Option<Reply>,
        winget: Option<Reply>,
        apps: Option<Reply>,
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
                Request::Signed { step, .. } => match step {
                    Step::Install => self.install.clone(),
                    Step::Reboot => self.reboot.clone(),
                    Step::Winget => self.winget.clone(),
                    Step::SelfUpdate => None,
                },
                Request::WingetScan => self.apps.clone(),
                Request::Install { .. }
                | Request::Reboot
                | Request::SelfUpdate(_)
                | Request::WingetUpgrade { .. } => None,
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
        let command = Command {
            command_id: "c-1".into(),
            kind: monitorflow_common::command::APPLY_UPDATES.into(),
            payload: apply.encode_to_vec(),
            ..Command::default()
        };
        let output = handler.run_command(&command).await;
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
        // Read from this host's registry on Windows only.
        assert_eq!(
            manager.windows_update_source().await.is_some(),
            cfg!(windows)
        );
    }

    /// No updates, and WSUS as the source.
    struct OnWsus;

    #[async_trait::async_trait]
    impl PackageManager for OnWsus {
        async fn inventory(&self) -> Result<Vec<Package>, PackageError> {
            Ok(Vec::new())
        }

        async fn reboot_required(&self) -> Result<bool, PackageError> {
            Ok(false)
        }

        async fn windows_update_source(&self) -> Option<WindowsUpdateSource> {
            Some(WindowsUpdateSource {
                wsus_url: "http://wsus.example:8530".into(),
                ..WindowsUpdateSource::default()
            })
        }
    }

    #[tokio::test]
    async fn reports_carry_the_update_source() {
        let (reports, latest) = Reports::new(Arc::new(OnWsus));
        assert!(reports.collect().await);
        let report = latest.borrow().clone().unwrap();
        assert_eq!(
            report.windows_update_source.unwrap().wsus_url,
            "http://wsus.example:8530"
        );
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
            ..ApplyUpdates::default()
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
        // The service gets the command itself, one step at a time.
        let steps = |calls: &[Request]| -> Vec<Step> {
            calls
                .iter()
                .filter_map(|call| match call {
                    Request::Signed { step, .. } => Some(*step),
                    _ => None,
                })
                .collect()
        };
        assert!(matches!(calls[0], Request::Signed { .. }), "{calls:?}");
        assert_eq!(
            calls[1..],
            [Request::Scan, Request::Status, Request::WingetScan]
        );
        assert_eq!(steps(&calls), [Step::Install]);

        let (output, calls, _) = run(fake(), apply(true)).await;
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("restarting in one minute\n"));
        assert_eq!(steps(&calls), [Step::Install, Step::Reboot]);
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
        assert!(!calls.iter().any(|call| matches!(
            call,
            Request::Signed {
                step: Step::Reboot,
                ..
            }
        )));
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

    fn signed_steps(calls: &[Request]) -> Vec<Step> {
        calls
            .iter()
            .filter_map(|call| match call {
                Request::Signed { step, .. } => Some(*step),
                _ => None,
            })
            .collect()
    }

    fn winget_apply(ids: &[&str], skip_os: bool) -> ApplyUpdates {
        ApplyUpdates {
            skip_os,
            allow_reboot: true,
            third_party: vec![ThirdPartySelection {
                manager: monitorflow_common::proto::v1::PackageManager::Winget.into(),
                packages: ids.iter().map(|s| s.to_string()).collect(),
            }],
            ..ApplyUpdates::default()
        }
    }

    #[tokio::test]
    async fn reports_carry_winget_apps_or_why_there_are_none() {
        use monitorflow_common::proto::v1::package_manager_status::State;
        let apps = winget::Apps {
            state: winget::Availability::Available,
            version: "1.29.380".into(),
            error: String::new(),
            apps: vec![winget::App {
                id: "Example.App".into(),
                name: "Example".into(),
                source: "winget".into(),
                version: "1.0".into(),
                available: "2.0".into(),
            }],
        };
        let collect = |apps: Option<Reply>| async move {
            let fake = Fake {
                apps,
                ..Fake::default()
            };
            WindowsUpdates::new(Arc::new(fake)).third_party().await
        };
        let found = collect(Some(Reply::Apps(apps))).await;
        assert_eq!(found.managers.len(), 1);
        assert_eq!(found.managers[0].state, i32::from(State::Available));
        assert_eq!(found.packages[0].id, "Example.App");
        assert_eq!(found.packages[0].available_version, "2.0");

        let absent = collect(Some(Reply::Apps(winget::Apps::absent(
            winget::NO_POWERSHELL,
        ))))
        .await;
        assert_eq!(absent.managers[0].state, i32::from(State::Absent));
        assert_eq!(absent.managers[0].error, winget::NO_POWERSHELL);
        assert!(absent.packages.is_empty());

        // What a service from before ADR-0139 answers.
        let old = Reply::Error(
            "invalid request: a message is not valid: unknown variant `winget_scan`, expected one of `scan`".into(),
        );
        let old = collect(Some(old)).await;
        assert_eq!(old.managers[0].state, i32::from(State::Unusable));
        assert_eq!(old.managers[0].error, SERVICE_TOO_OLD);

        let down = collect(None).await;
        assert_eq!(down.managers[0].state, i32::from(State::Unusable));
        assert_eq!(down.managers[0].error, "the update service is not running");
    }

    #[tokio::test]
    async fn winget_runs_after_windows_updates_and_its_restart_counts() {
        let fake = || Fake {
            install: Some(installed(&[("KB1", Outcome::Succeeded)], false)),
            winget: Some(installed(&[("Example.App", Outcome::Succeeded)], true)),
            reboot: Some(Reply::Rebooting(())),
            ..Fake::default()
        };
        let (output, calls, reported) = run(fake(), winget_apply(&["Example.App"], false)).await;
        assert_eq!((output.exit_code, output.error.as_deref()), (0, None));
        assert!(reported);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "$ windows update install all\n\
             KB1 succeeded: KB1 title\n\
             $ winget upgrade Example.App\n\
             Example.App succeeded: Example.App title\n\
             a restart is required\n\
             restarting in one minute\n"
        );
        assert_eq!(
            signed_steps(&calls),
            [Step::Install, Step::Winget, Step::Reboot]
        );

        // Skipping the OS sends no Windows Update step.
        let (output, calls, _) = run(fake(), winget_apply(&[], true)).await;
        assert_eq!(output.exit_code, 0);
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("$ winget upgrade all\n"));
        assert_eq!(signed_steps(&calls), [Step::Winget, Step::Reboot]);
    }

    #[tokio::test]
    async fn the_first_failure_stops_the_command() {
        let os_fails = Fake {
            install: Some(installed(&[("KB2", Outcome::Failed)], false)),
            winget: Some(installed(&[("Example.App", Outcome::Succeeded)], false)),
            ..Fake::default()
        };
        let (output, calls, reported) = run(os_fails, winget_apply(&[], false)).await;
        assert_eq!(output.exit_code, 1);
        assert!(reported);
        assert_eq!(signed_steps(&calls), [Step::Install]);

        let winget_fails = Fake {
            winget: Some(installed(&[("Example.App", Outcome::Failed)], true)),
            reboot: Some(Reply::Rebooting(())),
            ..Fake::default()
        };
        let (output, calls, _) = run(winget_fails, winget_apply(&[], true)).await;
        assert_eq!(output.exit_code, 1);
        assert_eq!(
            output.error.as_deref(),
            Some("1 of 1 updates did not install: Example.App (0x80240022)")
        );
        assert_eq!(signed_steps(&calls), [Step::Winget]);
    }

    #[tokio::test]
    async fn bad_selections_are_refused_before_anything_runs() {
        let bad_id = winget_apply(&["Example App"], true);
        let nothing = ApplyUpdates {
            skip_os: true,
            ..ApplyUpdates::default()
        };
        let mut linux = winget_apply(&[], true);
        linux.third_party[0].manager = monitorflow_common::proto::v1::PackageManager::Snap.into();
        for apply in [bad_id, nothing, linux] {
            let (output, calls, _) = run(Fake::default(), apply.clone()).await;
            assert!(output.rejected, "{apply:?}");
            assert!(calls.is_empty(), "{apply:?}");
        }
    }
}
