//! The `configure_sources` and `revert_sources` commands (ADR-0035): the
//! signed spec goes to the root helper, `sudo -n /usr/bin/monitorflow-agent
//! sources apply|revert`, which checks it in full, changes the sources and
//! reverts on failure (ADR-0032).

use monitorflow_common::proto::v1::configure_sources::Family;
use monitorflow_common::proto::v1::{ConfigureSources, RevertSources};
use prost::Message;

use super::exec::{Run, Tools};
use super::PackageError;
use crate::commands::{Handler, Output};
use crate::os::OsFamily;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcesAction {
    Apply,
    Revert,
}

/// Runs the helper for one of the two kinds.
pub struct SourcesHandler {
    action: SourcesAction,
    family: OsFamily,
    tools: Tools,
}

impl SourcesHandler {
    /// `tools` gives `sudo` and the agent binary; `family` is the host's,
    /// a spec of the other one is refused without running anything.
    pub fn new(action: SourcesAction, family: OsFamily, tools: Tools) -> Self {
        Self {
            action,
            family,
            tools,
        }
    }

    /// The payload checked as far as the agent can without the helper,
    /// and what goes to the helper's stdin.
    fn input<'a>(&self, payload: &'a [u8]) -> Result<Option<&'a [u8]>, String> {
        match self.action {
            SourcesAction::Revert => match RevertSources::decode(payload) {
                Ok(_) => Ok(None),
                Err(_) => Err("invalid RevertSources payload".into()),
            },
            SourcesAction::Apply => {
                let spec = ConfigureSources::decode(payload)
                    .map_err(|_| "invalid ConfigureSources payload".to_owned())?;
                let host = match self.family {
                    OsFamily::Debian => Family::Apt,
                    OsFamily::Rhel => Family::Dnf,
                };
                if spec.family() != host {
                    return Err(format!(
                        "a {} spec cannot configure this {} host",
                        spec.family().as_str_name(),
                        self.family
                    ));
                }
                // The signed bytes as received; the helper decodes them again.
                Ok(Some(payload))
            }
        }
    }
}

/// The helper's run as the command's output; its first stderr line is
/// the error when it fails.
fn output(run: Run) -> Output {
    let error = (!run.success()).then(|| PackageError::failed(&run).to_string());
    let mut stdout = format!("$ {}\n", run.line).into_bytes();
    stdout.extend_from_slice(&run.stdout);
    Output {
        exit_code: run.code,
        stdout,
        stderr: run.stderr,
        error,
        rejected: false,
    }
}

#[async_trait::async_trait]
impl Handler for SourcesHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        let input = match self.input(payload) {
            Ok(input) => input,
            Err(error) => return Output::rejected(error),
        };
        let verb = match self.action {
            SourcesAction::Apply => "apply",
            SourcesAction::Revert => "revert",
        };
        let run = self
            .tools
            .privileged_with_input(&[], &self.tools.agent, &["sources", verb], input)
            .await;
        match run {
            Ok(run) => {
                tracing::info!(
                    action = verb,
                    exit_code = run.code,
                    "sources helper finished"
                );
                output(run)
            }
            Err(error) => Output {
                exit_code: -1,
                error: Some(error.to_string()),
                ..Output::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use monitorflow_common::proto::v1::configure_sources::Mode;
    use monitorflow_sensors::testing::write_executable;
    use tempfile::TempDir;

    use super::*;

    /// A fake `sudo` logging its arguments and a fake helper saving its
    /// stdin, answering `exit`.
    struct Fakes {
        dir: TempDir,
    }

    impl Fakes {
        fn new(exit: i32) -> Self {
            let dir = TempDir::new().unwrap();
            let path = |name: &str| dir.path().join(name).display().to_string();
            write_executable(
                &dir.path().join("sudo"),
                &format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$1\" = -n ] || exit 1\nshift\nexec \"$@\"\n",
                    path("calls")
                ),
                0o755,
            );
            write_executable(
                &dir.path().join("monitorflow-agent"),
                &format!(
                    "#!/bin/sh\ncat > '{}'\necho \"changed /etc/apt/sources.list.d/monitorflow.sources\"\n\
                     [ {exit} -eq 0 ] || {{ echo 'Error: post-check failed, sources reverted: E: 404' >&2; echo 'more' >&2; }}\nexit {exit}\n",
                    path("stdin")
                ),
                0o755,
            );
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn handler(&self, action: SourcesAction, family: OsFamily) -> SourcesHandler {
            let tools = Tools {
                sudo: self.path("sudo"),
                agent: self.path("monitorflow-agent"),
                ..Tools::default()
            };
            SourcesHandler::new(action, family, tools)
        }

        fn calls(&self) -> Vec<String> {
            let dir = format!("{}/", self.dir.path().display());
            std::fs::read_to_string(self.path("calls"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.replace(&dir, "@"))
                .collect()
        }
    }

    fn spec(family: Family) -> Vec<u8> {
        ConfigureSources {
            mirror_base_url: "http://mf.example:7480".into(),
            family: family as i32,
            mode: Mode::Replace as i32,
            ..ConfigureSources::default()
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn apply_sends_the_spec_to_the_helper() {
        let fakes = Fakes::new(0);
        let payload = spec(Family::Apt);
        let output = fakes
            .handler(SourcesAction::Apply, OsFamily::Debian)
            .run(&payload)
            .await;
        assert_eq!((output.exit_code, &output.error), (0, &None), "{output:?}");
        assert_eq!(fakes.calls(), ["-n @monitorflow-agent sources apply"]);
        assert_eq!(std::fs::read(fakes.path("stdin")).unwrap(), payload);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.starts_with("$ sudo -n ") && stdout.contains("changed /etc/apt"),
            "{stdout}"
        );
    }

    #[tokio::test]
    async fn revert_runs_the_helper_without_input() {
        let fakes = Fakes::new(0);
        let output = fakes
            .handler(SourcesAction::Revert, OsFamily::Rhel)
            .run(&RevertSources {}.encode_to_vec())
            .await;
        assert_eq!((output.exit_code, &output.error), (0, &None), "{output:?}");
        assert_eq!(fakes.calls(), ["-n @monitorflow-agent sources revert"]);
        assert_eq!(std::fs::read(fakes.path("stdin")).unwrap(), b"");
    }

    #[tokio::test]
    async fn a_failing_helper_fails_with_its_error_line() {
        let fakes = Fakes::new(1);
        let output = fakes
            .handler(SourcesAction::Apply, OsFamily::Rhel)
            .run(&spec(Family::Dnf))
            .await;
        assert_eq!(output.exit_code, 1);
        assert!(!output.rejected);
        let error = output.error.unwrap();
        assert!(
            error.ends_with("exited with 1: Error: post-check failed, sources reverted: E: 404"),
            "{error}"
        );
        assert_eq!(
            output.stderr,
            b"Error: post-check failed, sources reverted: E: 404\nmore\n"
        );
    }

    #[tokio::test]
    async fn bad_payloads_and_other_families_run_nothing() {
        let fakes = Fakes::new(0);
        let apply = fakes.handler(SourcesAction::Apply, OsFamily::Debian);
        for payload in [
            vec![0xff, 0xff],
            spec(Family::Dnf),
            spec(Family::Unspecified),
        ] {
            let output = apply.run(&payload).await;
            assert!(output.rejected, "{output:?}");
        }
        let output = fakes
            .handler(SourcesAction::Revert, OsFamily::Debian)
            .run(&[0xff])
            .await;
        assert!(output.rejected, "{output:?}");
        assert!(fakes.calls().is_empty());
    }

    #[tokio::test]
    async fn a_missing_sudo_fails() {
        let tools = Tools {
            sudo: "/nonexistent/sudo".into(),
            ..Tools::default()
        };
        let output = SourcesHandler::new(SourcesAction::Revert, OsFamily::Debian, tools)
            .run(&[])
            .await;
        assert_eq!(output.exit_code, -1);
        assert!(output.error.unwrap().contains("/nonexistent/sudo"));
        assert!(!output.rejected);
    }
}
