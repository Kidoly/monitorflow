//! The `apply_updates` command: refresh, upgrade, report, and reboot when
//! allowed and needed.

use std::sync::Arc;

use monitorflow_common::proto::v1::ApplyUpdates;
use prost::Message;

use super::exec::{Run, Tools};
use super::{valid_name, PackageError, PackageManager, Reports};
use crate::commands::{Handler, Output};

pub struct ApplyUpdatesHandler {
    manager: Arc<dyn PackageManager>,
    reports: Reports,
    tools: Tools,
}

impl ApplyUpdatesHandler {
    /// `tools` gives `sudo` and `shutdown` for the reboot.
    pub fn new(manager: Arc<dyn PackageManager>, reports: Reports, tools: Tools) -> Self {
        Self {
            manager,
            reports,
            tools,
        }
    }

    /// The steps up to the reboot. Every run is appended to `output`;
    /// the first failing one ends the command.
    async fn apply(&self, request: &ApplyUpdates, output: &mut Output) -> Result<(), PackageError> {
        if let Some(run) = self.manager.refresh().await? {
            step(output, run)?;
        }
        match self.manager.upgrade(request).await? {
            Some(run) => step(output, run)?,
            None => output.stdout.extend_from_slice(b"nothing to upgrade\n"),
        }
        Ok(())
    }
}

/// Appends `run` to the command's output; a failure becomes the error.
fn step(output: &mut Output, run: Run) -> Result<(), PackageError> {
    output
        .stdout
        .extend_from_slice(format!("$ {}\n", run.line).as_bytes());
    output.stdout.extend_from_slice(&run.stdout);
    output.stderr.extend_from_slice(&run.stderr);
    output.exit_code = run.code;
    if run.success() {
        Ok(())
    } else {
        Err(PackageError::failed(&run))
    }
}

#[async_trait::async_trait]
impl Handler for ApplyUpdatesHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        let Ok(request) = ApplyUpdates::decode(payload) else {
            return Output::rejected("invalid ApplyUpdates payload");
        };
        if let Some(bad) = request.packages.iter().find(|p| !valid_name(p)) {
            return Output::rejected(format!("invalid package name `{}`", bad.escape_debug()));
        }
        let mut output = Output::default();
        let result = self.apply(&request, &mut output).await;
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
