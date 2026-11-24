//! The `run_script` command (ADR-0189): a script of the central server's
//! library, run as the agent's user with the same isolation as `shell`
//! commands: environment reset, no stdin for the script, from the root
//! directory, and a timeout that kills everything it started.
//!
//! Only the host's administrator lets this agent run scripts, with
//! `commands.scripts`; no command can turn it on. Parameters reach the
//! script as environment variables `MF_PARAM_<NAME>`, never inside its
//! text, so a value cannot change what the server classified.

use std::time::Duration;

use monitorflow_common::proto::v1::{CommandAcceptance, RunScript};
use prost::Message;

use super::{Handler, Output};

/// Timeout when the command does not set one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Longest timeout accepted; longer ones are cut to it.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Longest script, in bytes, as the server allows.
pub const MAX_BODY: usize = 64 * 1024;
/// Most parameters of one script.
pub const MAX_PARAMETERS: usize = 20;
/// Longest parameter value, in bytes.
pub const MAX_VALUE: usize = 1024;
/// Prefix of the environment variables holding the parameters.
pub const ENV_PREFIX: &str = "MF_PARAM_";

/// The interpreter this system runs.
#[cfg(unix)]
const INTERPRETER: &str = "sh";
#[cfg(windows)]
const INTERPRETER: &str = "powershell";

/// Why a script is refused while `commands.scripts` is off, with the way
/// to turn it on on this platform.
#[cfg(unix)]
const DISABLED: &str = "library scripts are disabled on this agent (commands.scripts); \
                        run the installer with MF_ALLOW_SCRIPTS=1 to allow them";
#[cfg(windows)]
const DISABLED: &str = "library scripts are disabled on this agent (commands.scripts); \
                        run `monitorflow-agent.exe scripts on` as an administrator \
                        and restart the MonitorFlowAgent service to allow them";

pub struct ScriptHandler {
    enabled: bool,
}

impl ScriptHandler {
    /// `enabled` is `commands.scripts`, read once at start.
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// Whether `name` is a parameter name: `[a-z][a-z0-9_]{0,31}`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The checked request: the environment to add, or why it is refused.
fn check(request: &RunScript) -> Result<Vec<(String, String)>, String> {
    if request.interpreter != INTERPRETER {
        return Err(format!(
            "this agent runs `{INTERPRETER}` scripts, not `{}`",
            request.interpreter.escape_debug()
        ));
    }
    if request.body.trim().is_empty() {
        return Err("the script is empty".into());
    }
    if request.body.len() > MAX_BODY || request.body.contains('\0') {
        return Err(format!(
            "the script must be at most {MAX_BODY} bytes, without NUL"
        ));
    }
    if request.parameters.len() > MAX_PARAMETERS {
        return Err(format!("at most {MAX_PARAMETERS} parameters"));
    }
    let mut env: Vec<(String, String)> = Vec::new();
    for parameter in &request.parameters {
        if !valid_name(&parameter.name) {
            return Err("invalid parameter name".into());
        }
        if parameter.value.len() > MAX_VALUE || parameter.value.contains(['\0', '\n', '\r']) {
            return Err(format!(
                "parameter `{}` must be one line of at most {MAX_VALUE} bytes",
                parameter.name
            ));
        }
        let key = format!("{ENV_PREFIX}{}", parameter.name.to_ascii_uppercase());
        if env.iter().any(|(k, _)| *k == key) {
            return Err(format!("parameter `{}` is given twice", parameter.name));
        }
        env.push((key, parameter.value.clone()));
    }
    Ok(env)
}

#[async_trait::async_trait]
impl Handler for ScriptHandler {
    fn acceptance(&self) -> CommandAcceptance {
        if self.enabled {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    async fn run(&self, payload: &[u8]) -> Output {
        if !self.enabled {
            return Output::rejected(DISABLED);
        }
        let Ok(request) = RunScript::decode(payload) else {
            return Output::rejected("invalid RunScript payload");
        };
        let env = match check(&request) {
            Ok(env) => env,
            Err(error) => return Output::rejected(error),
        };
        let timeout = match request.timeout_seconds {
            0 => DEFAULT_TIMEOUT,
            s => Duration::from_secs(s.into()).min(MAX_TIMEOUT),
        };
        tracing::info!(
            script_id = %request.script_id.escape_debug(),
            version = request.version,
            timeout_s = timeout.as_secs(),
            "running a library script"
        );
        #[cfg(unix)]
        return super::shell::unix::run_with(&request.body, &env, timeout).await;
        #[cfg(windows)]
        return super::shell::windows::run_script(&request.body, &env, timeout).await;
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::proto::v1::ScriptParameter;

    use super::*;

    fn payload(body: &str, parameters: &[(&str, &str)], timeout_seconds: u32) -> Vec<u8> {
        RunScript {
            interpreter: INTERPRETER.into(),
            body: body.into(),
            parameters: parameters
                .iter()
                .map(|(name, value)| ScriptParameter {
                    name: (*name).into(),
                    value: (*value).into(),
                })
                .collect(),
            timeout_seconds,
            script_id: "s".into(),
            version: 1,
        }
        .encode_to_vec()
    }

    #[test]
    fn parameter_names() {
        assert!(valid_name("path") && valid_name("max_age2"));
        for bad in ["", "Path", "2x", "a-b", "a b", &"a".repeat(33)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    /// The report is true of what the handler then does (ADR-0076), and
    /// off refuses without running anything.
    #[tokio::test]
    async fn off_unless_the_host_allows_it() {
        let off = ScriptHandler::new(false);
        assert_eq!(off.acceptance(), CommandAcceptance::Disabled);
        let output = off.run(&payload("whoami", &[], 0)).await;
        assert!(output.rejected);
        assert!(output.error.unwrap().contains("commands.scripts"));
        assert_eq!(
            ScriptHandler::new(true).acceptance(),
            CommandAcceptance::Accepted
        );
    }

    #[tokio::test]
    async fn refuses_bad_requests() {
        let handler = ScriptHandler::new(true);
        assert!(handler.run(b"\xff\xff").await.rejected);
        assert!(handler.run(&payload("  \n", &[], 0)).await.rejected);
        assert!(handler.run(&payload("a\0b", &[], 0)).await.rejected);
        let long = "#".repeat(MAX_BODY + 1);
        assert!(handler.run(&payload(&long, &[], 0)).await.rejected);
        let twice = payload("whoami", &[("a", "1"), ("a", "2")], 0);
        assert!(handler.run(&twice).await.rejected);
        let multiline = payload("whoami", &[("a", "1\n2")], 0);
        assert!(handler.run(&multiline).await.rejected);
        let bad_name = payload("whoami", &[("A", "1")], 0);
        assert!(handler.run(&bad_name).await.rejected);
        let mut other = RunScript::decode(&payload("whoami", &[], 0)[..]).unwrap();
        other.interpreter = if cfg!(unix) { "powershell" } else { "sh" }.into();
        let output = handler.run(&other.encode_to_vec()).await;
        assert!(output.rejected && output.error.unwrap().contains("runs"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_multi_line_scripts_with_parameters_in_the_environment() {
        let handler = ScriptHandler::new(true);
        let body = "set -e\nfor word in one two; do\n  echo \"$word\"\ndone\n\
                    echo \"[$MF_PARAM_TARGET] [$MF_PARAM_COUNT] [$HOME]\"\nexit 4\n";
        let parameters = [("target", "a b; rm -rf /"), ("count", "3")];
        let output = handler.run(&payload(body, &parameters, 0)).await;
        assert!(!output.rejected, "{output:?}");
        assert_eq!(output.exit_code, 4);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "one\ntwo\n[a b; rm -rf /] [3] []\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeouts_kill_the_script() {
        let started = std::time::Instant::now();
        let output = ScriptHandler::new(true)
            .run(&payload("sleep 5\necho late", &[], 1))
            .await;
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(output.error.as_deref(), Some("timed out after 1 s"));
    }
}
