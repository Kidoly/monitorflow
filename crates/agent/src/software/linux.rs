//! The `software` command on Linux (ADR-0188), as the agent's own user:
//! refuses it unless the host allowed software installs and the web
//! interface did not turn them off, checks the payload, then passes the
//! signed command to `sudo -n /usr/bin/monitorflow-agent packages
//! software`, one exact sudo line. The helper ([`super::helper`]) checks
//! everything again as root.

use std::sync::Arc;

use monitorflow_common::proto::v1::SoftwareChange;
use monitorflow_common::proto::v1::{software_change::Action, Command, CommandAcceptance};
use monitorflow_common::software;
use prost::Message;

use super::SoftwareSwitch;
use crate::commands::{Handler, Output};
use crate::helper_trust::HelperRequest;
use crate::os::OsFamily;
use crate::packages::{PackageError, PackageManager, Reports, Tools};

/// The helper's arguments after the agent binary, as sudoers names them.
pub const HELPER_ARGS: [&str; 2] = ["packages", "software"];

/// What sudo's refusal means here: rules installed before this helper.
pub const OLD_SUDO_RULES: &str = "the sudo rules on this server predate software installs; \
     run the install command again";

/// Runs `software` commands through the root helper.
pub struct LinuxSoftwareHandler {
    switch: Arc<SoftwareSwitch>,
    family: OsFamily,
    /// Whether sudo lets this user run the helper, as found at start.
    sudo_rule: bool,
    manager: Arc<dyn PackageManager>,
    reports: Reports,
    tools: Tools,
}

impl LinuxSoftwareHandler {
    pub fn new(
        switch: Arc<SoftwareSwitch>,
        family: OsFamily,
        sudo_rule: bool,
        manager: Arc<dyn PackageManager>,
        reports: Reports,
        tools: Tools,
    ) -> Self {
        Self {
            switch,
            family,
            sudo_rule,
            manager,
            reports,
            tools,
        }
    }
}

/// Whether sudo lets this user run the helper without a password: `sudo
/// -n -l <agent> packages software` answers without running it.
pub async fn sudo_rule(tools: &Tools) -> bool {
    let agent = tools.agent.display().to_string();
    let mut args = vec!["-n", "-l", agent.as_str()];
    args.extend(HELPER_ARGS);
    match tools.read(&tools.sudo, &args).await {
        Ok(run) => run.success(),
        Err(error) => {
            tracing::debug!(%error, "cannot ask sudo about the software helper");
            false
        }
    }
}

#[async_trait::async_trait]
impl Handler for LinuxSoftwareHandler {
    /// Off without the consent or once turned off from the web interface;
    /// unsupported while the sudo rules predate the helper.
    fn acceptance(&self) -> CommandAcceptance {
        match self.switch.acceptance() {
            CommandAcceptance::Accepted if !self.sudo_rule => CommandAcceptance::Unsupported,
            acceptance => acceptance,
        }
    }

    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("software needs its signed command")
    }

    async fn run_command(&self, command: &Command) -> Output {
        if let Some(refusal) = self.switch.refusal() {
            return Output::rejected(refusal);
        }
        let Ok(change) = SoftwareChange::decode(command.payload.as_slice()) else {
            return Output::rejected("invalid SoftwareChange payload");
        };
        if let Err(error) = software::check_change(&change, Some(self.family.as_str())) {
            return Output::rejected(error.to_string());
        }
        if !self.sudo_rule {
            return Output::rejected(OLD_SUDO_RULES);
        }
        let verb = if change.action == i32::from(Action::Install) {
            "install"
        } else {
            "remove"
        };
        tracing::info!(
            action = verb,
            packages = change.packages.len(),
            "changing software"
        );
        let run = self
            .tools
            .privileged_with_input(
                &[],
                &self.tools.agent,
                &HELPER_ARGS,
                Some(&HelperRequest::of(command).framed()),
            )
            .await;
        // Whatever happened, the server gets the state of the host.
        self.reports.collect().await;
        let mut output = Output::default();
        match run {
            Ok(run) => {
                tracing::info!(exit_code = run.code, "software helper finished");
                if !run.success() {
                    output.exit_code = if run.code == 0 { 1 } else { run.code };
                    output.error = Some(error_of(&run));
                }
                output.stdout = run.stdout;
                output.stderr = run.stderr;
            }
            Err(error) => {
                output.exit_code = 1;
                output.error = Some(error.to_string());
            }
        }
        if output.error.is_none()
            && change.allow_reboot
            && self.manager.reboot_required().await.unwrap_or(false)
        {
            // In a minute, so the result and the report get out first.
            let reboot = self
                .tools
                .privileged(&[], &self.tools.shutdown, &["-r", "+1"])
                .await;
            match reboot {
                Ok(run) if run.success() => {
                    output
                        .stdout
                        .extend_from_slice(b"restarting in one minute\n");
                }
                Ok(run) => {
                    output.exit_code = run.code.max(1);
                    output.error = Some(format!(
                        "software changed, reboot failed: {}",
                        PackageError::failed(&run)
                    ));
                }
                Err(error) => {
                    output.exit_code = 1;
                    output.error = Some(format!("software changed, reboot failed: {error}"));
                }
            }
        }
        output
    }
}

/// The helper's last error line, or what sudo's own refusal means.
fn error_of(run: &crate::packages::Run) -> String {
    let stderr = String::from_utf8_lossy(&run.stderr);
    if stderr.contains("a password is required") || stderr.contains("a terminal is required") {
        return OLD_SUDO_RULES.into();
    }
    PackageError::helper(run).to_string()
}
