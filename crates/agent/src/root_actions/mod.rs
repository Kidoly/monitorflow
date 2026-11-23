//! AI root actions (ADR-0181): the `root_action` command, one action of a
//! fixed catalog ([`RootAction`]) run as root by the agent's own helper.
//!
//! The agent's side, as its own user ([`RootActionHandler`]), refuses the
//! command unless AI commands are on (ADR-0098) and the host allowed root
//! actions (`commands.root_actions`), checks the payload, then passes the
//! signed command to `sudo -n /usr/bin/monitorflow-agent root-action`, the
//! one sudo line of the feature. The helper ([`helper`]) trusts none of
//! that: it reads the consent from `agent.toml` itself, checks the
//! command's signature with its own anchor (ADR-0108) and the parameters
//! again, and runs fixed programs with fixed options. Linux only.

pub mod helper;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use monitorflow_common::proto::v1::{self, Command, CommandAcceptance};
use monitorflow_common::root_action::RootAction;
use prost::Message;

use crate::commands::ai_switch::AiSwitch;
use crate::commands::{Handler, Output};
use crate::helper_trust::HelperRequest;
use crate::packages::{PackageError, Tools};

/// The helper's arguments after the agent binary, as sudoers names them.
pub const HELPER_ARGS: [&str; 1] = ["root-action"];

const COMMANDS_OFF: &str = "AI commands are off on this agent, root actions too";

/// Why a root action is refused without the host's consent, with the way
/// to give it.
pub const NOT_ALLOWED: &str = "AI root actions are not allowed on this server \
     (commands.root_actions); run `sudo monitorflow-agent ai-commands root-actions on` \
     there, or the installer with MF_AI_ROOT_ACTIONS=1, to allow them";

/// What sudo's refusal means here: rules installed before root actions.
pub const OLD_SUDO_RULES: &str = "the sudo rules on this server predate AI root actions; \
     run the install command again";

/// Runs `root_action` commands through the root helper.
pub struct RootActionHandler {
    switch: Arc<AiSwitch>,
    /// `commands.root_actions`.
    allowed: bool,
    /// Whether sudo lets this user run the helper, as found at start.
    sudo_rule: bool,
    tools: Tools,
}

impl RootActionHandler {
    pub fn new(switch: Arc<AiSwitch>, allowed: bool, sudo_rule: bool, tools: Tools) -> Self {
        Self {
            switch,
            allowed,
            sudo_rule,
            tools,
        }
    }
}

/// Whether sudo lets this user run the helper without a password:
/// `sudo -n -l <agent> root-action` answers without running it. Hosts
/// installed before ADR-0181 keep their old rules after a self-update.
pub async fn sudo_rule(tools: &Tools) -> bool {
    let agent = tools.agent.display().to_string();
    let mut args = vec!["-n", "-l", agent.as_str()];
    args.extend(HELPER_ARGS);
    match tools.read(&tools.sudo, &args).await {
        Ok(run) => run.success(),
        Err(error) => {
            tracing::debug!(%error, "cannot ask sudo about the root actions helper");
            false
        }
    }
}

/// The action a payload names, checked as the helper will.
pub fn action(payload: &[u8]) -> Result<RootAction, String> {
    let message =
        v1::RootAction::decode(payload).map_err(|e| format!("invalid RootAction payload: {e}"))?;
    RootAction::from_proto(&message)
}

#[async_trait::async_trait]
impl Handler for RootActionHandler {
    /// Off while AI commands are off or the host did not allow root
    /// actions; unsupported while the sudo rules predate them.
    fn acceptance(&self) -> CommandAcceptance {
        if !self.allowed || !self.switch.is_on() {
            CommandAcceptance::Disabled
        } else if !self.sudo_rule {
            CommandAcceptance::Unsupported
        } else {
            CommandAcceptance::Accepted
        }
    }

    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("root actions need their signed command")
    }

    async fn run_command(&self, command: &Command) -> Output {
        if !self.switch.is_on() {
            return Output::rejected(COMMANDS_OFF);
        }
        if !self.allowed {
            return Output::rejected(NOT_ALLOWED);
        }
        let action = match action(&command.payload) {
            Ok(action) => action,
            Err(error) => return Output::rejected(error),
        };
        tracing::info!(action = action.name(), "running an AI root action");
        let request = HelperRequest::of(command).framed();
        let run = self
            .tools
            .privileged_with_input(&[], &self.tools.agent, &HELPER_ARGS, Some(&request))
            .await;
        let mut output = Output::default();
        match run {
            Ok(run) => {
                tracing::info!(exit_code = run.code, "root actions helper finished");
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
