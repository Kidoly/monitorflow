//! The agent's side, as its own user: the storage part of the package
//! report, and the `take_snapshot` and `delete_snapshot` commands. Both go
//! through `sudo -n /usr/bin/monitorflow-agent snapshots …` (ADR-0094).

use monitorflow_common::proto::v1::{Command, DeleteSnapshot, StorageReport, TakeSnapshot};
use monitorflow_common::snapshot::valid_name;
use prost::Message;

use super::helper::Inventory;
use crate::commands::{Handler, Output};
use crate::helper_trust::HelperRequest;
use crate::packages::{PackageError, Reports, Tools};

/// The host's system volumes and MonitorFlow's snapshots, from the helper.
/// When the helper cannot run, the report says why instead, so the server
/// can tell an old sudoers file from a host on plain partitions.
pub async fn storage(tools: &Tools) -> StorageReport {
    let run = match tools
        .privileged(&[], &tools.agent, &["snapshots", "inspect"])
        .await
    {
        Ok(run) => run,
        Err(error) => return failed(error),
    };
    if !run.success() {
        return failed(PackageError::helper(&run));
    }
    match serde_json::from_slice::<Inventory>(&run.stdout) {
        Ok(inventory) => inventory.to_report(),
        Err(error) => failed(PackageError::Helper(format!(
            "unexpected output of `snapshots inspect`: {error}"
        ))),
    }
}

fn failed(error: PackageError) -> StorageReport {
    tracing::warn!(%error, "cannot inspect the storage; snapshots unavailable");
    StorageReport {
        error: error.to_string(),
        ..StorageReport::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotAction {
    Take,
    Delete,
}

impl SnapshotAction {
    fn helper_arg(self) -> &'static str {
        match self {
            Self::Take => "take",
            Self::Delete => "delete",
        }
    }
}

/// Runs `take_snapshot` or `delete_snapshot` through the helper, then
/// collects a report so the server sees the result at once.
pub struct SnapshotHandler {
    action: SnapshotAction,
    tools: Tools,
    reports: Reports,
}

impl SnapshotHandler {
    pub fn new(action: SnapshotAction, tools: Tools, reports: Reports) -> Self {
        Self {
            action,
            tools,
            reports,
        }
    }
}

impl SnapshotAction {
    /// The snapshot a payload of this action names: the agent checks it
    /// before calling the helper, and the helper reads it from the signed
    /// command, never from its caller (ADR-0108).
    pub fn name(self, payload: &[u8]) -> Result<String, String> {
        let name = match self {
            SnapshotAction::Take => TakeSnapshot::decode(payload).map(|t| t.name),
            SnapshotAction::Delete => DeleteSnapshot::decode(payload).map(|d| d.name),
        }
        .map_err(|e| format!("invalid payload: {e}"))?;
        if !valid_name(&name) {
            return Err(format!("invalid snapshot name `{name}`"));
        }
        Ok(name)
    }
}

#[async_trait::async_trait]
impl Handler for SnapshotHandler {
    async fn run(&self, _payload: &[u8]) -> Output {
        Output::rejected("snapshots need their signed command")
    }

    async fn run_command(&self, command: &Command) -> Output {
        if let Err(error) = self.action.name(&command.payload) {
            return Output::rejected(error);
        }
        let request = HelperRequest::of(command).framed();
        let run = self
            .tools
            .privileged_with_input(
                &[],
                &self.tools.agent,
                &["snapshots", self.action.helper_arg()],
                Some(&request),
            )
            .await;
        let mut output = Output::default();
        match run {
            Ok(run) => {
                tracing::info!(exit_code = run.code, "snapshots helper finished");
                if !run.success() {
                    output.exit_code = if run.code == 0 { 1 } else { run.code };
                    output.error = Some(PackageError::helper(&run).to_string());
                }
                output.stdout = run.stdout;
                output.stderr = run.stderr;
            }
            Err(error) => {
                output.exit_code = 1;
                output.error = Some(error.to_string());
            }
        }
        self.reports.collect().await;
        output
    }
}
