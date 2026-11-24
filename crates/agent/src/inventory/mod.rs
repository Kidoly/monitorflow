//! The hardware and software inventory (ADR-0187): what the machine is
//! made of and what it runs, collected read-only every [`COLLECT_EVERY`]
//! by [`Collector`] and sent by the stream through [`Link`], which keeps
//! the traffic small: a hash when nothing changed, the changes since what
//! the server holds when something did, the whole inventory only when the
//! server holds nothing the agent knows.

pub mod linux;
pub mod windows;

use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::inventory;
use monitorflow_common::proto::v1::{inventory_report, Inventory, InventoryReport};
use prost::Message;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::shutdown::Shutdown;
use crate::unix_ms;

/// How often the agent looks: a few files and three short commands on
/// Linux, a PowerShell script of a few seconds on Windows.
#[cfg(not(windows))]
pub const COLLECT_EVERY: Duration = Duration::from_secs(3600);
#[cfg(windows)]
pub const COLLECT_EVERY: Duration = Duration::from_secs(4 * 3600);
/// Longest time without a report, so the server knows the inventory is
/// still current.
pub const REFRESH_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Longest a command or the Windows script may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Most output read from a command.
const MAX_OUTPUT: usize = 4 << 20;

/// One collected inventory, canonical, with its hash.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub inventory: Inventory,
    pub hash: Vec<u8>,
    pub collected_at_unix_ms: i64,
}

impl Snapshot {
    pub fn new(inventory: Inventory, collected_at_unix_ms: i64) -> Self {
        let inventory = inventory::bound(inventory).0;
        Self {
            hash: inventory::hash(&inventory),
            inventory,
            collected_at_unix_ms,
        }
    }
}

/// Collects the inventory and publishes it when it changed, and at least
/// every [`REFRESH_EVERY`].
pub struct Collector {
    latest: watch::Sender<Option<Arc<Snapshot>>>,
}

impl Collector {
    pub fn new() -> (Self, watch::Receiver<Option<Arc<Snapshot>>>) {
        let (latest, rx) = watch::channel(None);
        (Self { latest }, rx)
    }

    /// Publishes `found` unless it has the hash published less than
    /// [`REFRESH_EVERY`] before `now`; true when it did.
    pub fn offer(&self, found: Snapshot, now: Instant, last: &mut Option<Instant>) -> bool {
        let same = self
            .latest
            .borrow()
            .as_ref()
            .is_some_and(|s| s.hash == found.hash);
        if same && last.is_some_and(|at| now.duration_since(at) < REFRESH_EVERY) {
            return false;
        }
        *last = Some(now);
        self.latest.send_replace(Some(Arc::new(found)));
        true
    }

    /// Collects at once, then every [`COLLECT_EVERY`], until `shutdown`.
    pub async fn run(self, mut shutdown: Shutdown) {
        let mut last = None;
        loop {
            match collect().await {
                Some(inventory) => {
                    let found = Snapshot::new(inventory, unix_ms());
                    let items = found.inventory.items.len();
                    if self.offer(found, Instant::now(), &mut last) {
                        tracing::debug!(items, "inventory published");
                    }
                }
                None => tracing::debug!("inventory not collected this time"),
            }
            tokio::select! {
                () = tokio::time::sleep(COLLECT_EVERY) => {}
                () = shutdown.wait() => return,
            }
        }
    }
}

/// One look at the machine; `None` when a source failed, so a passing
/// failure does not read as hardware or software removed.
#[cfg(not(windows))]
pub async fn collect() -> Option<Inventory> {
    let outputs = linux::Outputs::run().await?;
    tokio::task::spawn_blocking(move || linux::collect_in(std::path::Path::new("/"), &outputs))
        .await
        .ok()
}

#[cfg(windows)]
pub async fn collect() -> Option<Inventory> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    match monitorflow_sensors::powershell::run_script(
        windows::SCRIPT,
        deadline,
        "the inventory script",
    )
    .await
    {
        Ok(output) => {
            let inventory = windows::parse(&output);
            // The script always lists services: none means it failed.
            inventory
                .items
                .iter()
                .any(|i| i.kind == "service")
                .then_some(inventory)
        }
        Err(error) => {
            tracing::warn!(%error, "could not collect the inventory");
            None
        }
    }
}

/// What a command printed; empty when it is not installed, `None` when it
/// failed or timed out.
#[cfg_attr(windows, allow(dead_code))]
async fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.truncate(text.floor_char_boundary(MAX_OUTPUT));
            Some(text)
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
        Ok(Ok(output)) => {
            tracing::debug!(program, status = %output.status, "inventory command failed");
            None
        }
        Ok(Err(error)) => {
            tracing::debug!(program, %error, "inventory command did not start");
            None
        }
        Err(_) => {
            tracing::debug!(program, "inventory command timed out");
            None
        }
    }
}

/// A serial number, unless it is one of the placeholders firmware and
/// disks give when they have none.
pub fn clean_serial(text: &str) -> Option<String> {
    let serial = text.trim();
    let lower = serial.to_ascii_lowercase();
    let placeholder = matches!(
        lower.as_str(),
        "" | "none"
            | "n/a"
            | "na"
            | "unknown"
            | "invalid"
            | "not specified"
            | "not applicable"
            | "not available"
            | "to be filled by o.e.m."
            | "default string"
            | "system serial number"
            | "chassis serial number"
            | "base board serial number"
            | "serial number"
            | "0123456789"
            | "123456789"
    );
    let one_char = serial.chars().all(|c| serial.starts_with(c));
    (!placeholder && !one_char).then(|| serial.to_owned())
}

/// The stream's side: what to send when a stream opens, when a new
/// inventory is published and when the server asks. It remembers the
/// last inventory it sent in full or as changes, which is what the server
/// should hold, across streams.
pub struct Link {
    snapshots: watch::Receiver<Option<Arc<Snapshot>>>,
    sent: Option<Arc<Snapshot>>,
}

impl Link {
    pub fn new(snapshots: watch::Receiver<Option<Arc<Snapshot>>>) -> Self {
        Self {
            snapshots,
            sent: None,
        }
    }

    /// A new stream: the hash of the latest inventory, if any.
    pub fn opened(&mut self) -> Option<InventoryReport> {
        let latest = self.snapshots.borrow_and_update().clone()?;
        Some(hash_only(&latest))
    }

    /// The next published inventory; never when the collector is gone.
    pub async fn published(&mut self) -> Arc<Snapshot> {
        loop {
            if self.snapshots.changed().await.is_err() {
                return std::future::pending().await;
            }
            if let Some(snapshot) = self.snapshots.borrow_and_update().clone() {
                return snapshot;
            }
        }
    }

    /// What to send for a newly published `snapshot`: its hash when the
    /// server should hold it already, or when the agent does not know
    /// what the server holds (it asks then); else the changes.
    pub fn report(&mut self, snapshot: Arc<Snapshot>) -> InventoryReport {
        match self.sent.clone() {
            Some(sent) if sent.hash != snapshot.hash => {
                let report = with_body(&snapshot, Some(&sent));
                self.sent = Some(snapshot);
                report
            }
            _ => hash_only(&snapshot),
        }
    }

    /// The server holds `known_hash` and wants more: the changes since it
    /// when the agent sent it, else the whole inventory. `None` before
    /// the first collection, or when the server already holds the latest.
    pub fn requested(&mut self, known_hash: &[u8]) -> Option<InventoryReport> {
        let latest = self.snapshots.borrow().clone()?;
        if latest.hash == known_hash {
            self.sent = Some(latest);
            return None;
        }
        let base = self.sent.clone().filter(|s| s.hash == known_hash);
        let report = with_body(&latest, base.as_deref());
        self.sent = Some(latest);
        Some(report)
    }
}

fn hash_only(snapshot: &Snapshot) -> InventoryReport {
    InventoryReport {
        collected_at_unix_ms: snapshot.collected_at_unix_ms,
        hash: snapshot.hash.clone(),
        body: None,
    }
}

/// `snapshot` as the changes since `base`, or whole when there is no
/// base or the changes would be larger.
fn with_body(snapshot: &Snapshot, base: Option<&Snapshot>) -> InventoryReport {
    let full = inventory_report::Body::Full(snapshot.inventory.clone());
    let body = match base {
        Some(base) => {
            let delta = inventory::delta(&base.inventory, &snapshot.inventory);
            if delta.encoded_len() < snapshot.inventory.encoded_len() {
                inventory_report::Body::Delta(delta)
            } else {
                full
            }
        }
        None => full,
    };
    InventoryReport {
        collected_at_unix_ms: snapshot.collected_at_unix_ms,
        hash: snapshot.hash.clone(),
        body: Some(body),
    }
}

#[cfg(test)]
mod tests;
