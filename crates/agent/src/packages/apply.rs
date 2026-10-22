//! The `apply_updates` command: the root helper refreshes and upgrades
//! (ADR-0036), then the agent reports, and reboots when allowed and needed.

use std::sync::Arc;

use monitorflow_common::proto::v1::ApplyUpdates;
use prost::Message;

use super::exec::{Run, Tools};
use super::update::UpdateSpec;
use super::{PackageError, PackageManager, Reports};
use crate::commands::{Handler, Output};

pub struct ApplyUpdatesHandler {
    manager: Arc<dyn PackageManager>,
    reports: Reports,
    tools: Tools,
}

impl ApplyUpdatesHandler {
    /// `tools` gives `sudo`, the agent binary for the helper and
    /// `shutdown` for the reboot.
    pub fn new(manager: Arc<dyn PackageManager>, reports: Reports, tools: Tools) -> Self {
        Self {
            manager,
            reports,
            tools,
        }
    }

    /// The steps up to the reboot: `sudo -n /usr/bin/monitorflow-agent
    /// packages update` with `spec` on stdin. The helper prints what it
    /// ran; when it fails, its last stderr line is the error.
    async fn apply(&self, spec: &UpdateSpec, output: &mut Output) -> Result<(), PackageError> {
        let run = self
            .tools
            .privileged_with_input(
                &[],
                &self.tools.agent,
                &["packages", "update"],
                Some(&spec.to_json()),
            )
            .await?;
        tracing::info!(exit_code = run.code, "packages helper finished");
        let error = (!run.success()).then(|| PackageError::helper(&run));
        append(output, run);
        error.map_or(Ok(()), Err)
    }
}

/// Appends `run` to the command's output; a failure becomes the error.
fn step(output: &mut Output, run: Run) -> Result<(), PackageError> {
    let error = (!run.success()).then(|| PackageError::failed(&run));
    append(output, run);
    error.map_or(Ok(()), Err)
}

fn append(output: &mut Output, run: Run) {
    output
        .stdout
        .extend_from_slice(format!("$ {}\n", run.line).as_bytes());
    output.stdout.extend_from_slice(&run.stdout);
    output.stderr.extend_from_slice(&run.stderr);
    output.exit_code = run.code;
}

#[async_trait::async_trait]
impl Handler for ApplyUpdatesHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        let Ok(request) = ApplyUpdates::decode(payload) else {
            return Output::rejected("invalid ApplyUpdates payload");
        };
        let spec = UpdateSpec::from_request(&request);
        if let Err(error) = spec.validate() {
            return Output::rejected(error.to_string());
        }
        let mut output = Output::default();
        let result = self.apply(&spec, &mut output).await;
        // Whatever happened, the server gets the state of the host.
        self.reports.collect().await;
        if let Err(error) = result {
            if output.exit_code == 0 {
                output.exit_code = -1;
            }
            output.error = Some(error.to_string());
            return output;
        }
        if request.allow_reboot && self.manager.reboot_required().await.unwrap_or(false) {
            // In a minute, so the result and the report get out first.
            let reboot = self
                .tools
                .privileged(&[], &self.tools.shutdown, &["-r", "+1"])
                .await
                .and_then(|run| step(&mut output, run));
            if let Err(error) = reboot {
                output.error = Some(format!("updates applied, reboot failed: {error}"));
                if output.exit_code == 0 {
                    output.exit_code = -1;
                }
            }
        }
        output
    }
}
