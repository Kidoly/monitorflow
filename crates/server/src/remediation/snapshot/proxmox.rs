//! Proxmox VE VM snapshots over the HTTPS REST API (ADR-0044).
//!
//! [`ProxmoxClient`] speaks to one cluster with an API token
//! (`Authorization: PVEAPIToken=user@realm!tokenid=secret`): it starts
//! snapshot, rollback and deletion tasks under
//! `/api2/json/nodes/{node}/qemu/{vmid}/snapshot` and polls
//! `/nodes/{node}/tasks/{upid}/status` until they stop. Certificates are
//! always verified, against the cluster's `ca_file` alone when set.
//!
//! [`ProxmoxSnapshotter`] maps servers to VMs with the database and keeps
//! the newest `keep_snapshots` snapshots it took per VM.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use monitorflow_common::config::{valid_proxmox_token_id, ProxmoxClusterConfig, ProxmoxConfig};
use monitorflow_common::redact::Secret;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::store::{self, VmRef};
use super::{
    bounded, Snapshot, SnapshotError, SnapshotName, SnapshotRequest, Snapshotter, Support,
};
use crate::api::audit::{self, Event};

/// Longest UPID accepted from the API.
const MAX_UPID: usize = 256;

/// Timeouts of one cluster's client.
#[derive(Debug, Clone)]
pub struct ProxmoxOptions {
    /// One HTTP request, connection included.
    pub request_timeout: Duration,
    /// One task, from its start to its end.
    pub task_timeout: Duration,
    /// Between two task status polls.
    pub poll_interval: Duration,
}

impl ProxmoxOptions {
    pub fn from_config(config: &ProxmoxConfig) -> Self {
        Self {
            request_timeout: Duration::from_secs(config.request_timeout),
            task_timeout: Duration::from_secs(config.task_timeout),
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// A snapshot as the API lists it.
#[derive(Debug, Clone, Deserialize)]
pub struct ListedSnapshot {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Unix time; absent on the `current` pseudo-snapshot.
    pub snaptime: Option<i64>,
}

/// Client of one Proxmox VE cluster's API.
#[derive(Debug, Clone)]
pub struct ProxmoxClient {
    http: reqwest::Client,
    base: Url,
    auth: HeaderValue,
    options: ProxmoxOptions,
}

impl ProxmoxClient {
    /// `base_url` is `https://host:8006`; `ca_pem` holds the only CA
    /// certificates trusted, else the built-in web roots are.
    pub fn new(
        base_url: &str,
        token_id: &str,
        secret: &Secret,
        ca_pem: Option<&[u8]>,
        options: ProxmoxOptions,
    ) -> Result<Self, SnapshotError> {
        let config = |m: &str| SnapshotError::Config(m.to_owned());
        let base = Url::parse(base_url).map_err(|_| config("url is not a URL"))?;
        if base.scheme() != "https" || base.host_str().is_none() {
            return Err(config("url must be an https URL"));
        }
        if !valid_proxmox_token_id(token_id) {
            return Err(config("token_id must look like user@realm!tokenid"));
        }
        let secret = secret.expose();
        if secret.is_empty() || !secret.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(config(
                "the token secret must be printable ASCII without spaces",
            ));
        }
        let mut auth = HeaderValue::from_str(&format!("PVEAPIToken={token_id}={secret}"))
            .map_err(|_| config("the token is not a valid header value"))?;
        auth.set_sensitive(true);
        let mut builder = reqwest::Client::builder()
            .https_only(true)
            // The token must not follow a redirect anywhere.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(options.request_timeout)
            .connect_timeout(options.request_timeout.min(Duration::from_secs(10)));
        if let Some(pem) = ca_pem {
            let certs = reqwest::Certificate::from_pem_bundle(pem)
                .map_err(|_| config("ca_file is not PEM"))?;
            if certs.is_empty() {
                return Err(config("ca_file holds no certificate"));
            }
            builder = builder.tls_built_in_root_certs(false);
            for cert in certs {
                builder = builder.add_root_certificate(cert);
            }
        }
        let http = builder
            .build()
            .map_err(|e| SnapshotError::Config(e.without_url().to_string()))?;
        Ok(Self {
            http,
            base,
            auth,
            options,
        })
    }

    /// Reads the token secret and the CA file of `cluster`.
    pub async fn from_config(
        cluster: &ProxmoxClusterConfig,
        options: ProxmoxOptions,
    ) -> Result<Self, SnapshotError> {
        let secret = read_secret(&cluster.token_secret_file).await?;
        let ca = match &cluster.ca_file {
            Some(path) => Some(tokio::fs::read(path).await.map_err(|e| {
                SnapshotError::Config(format!("cannot read {}: {e}", path.display()))
            })?),
            None => None,
        };
        Self::new(
            &cluster.url,
            &cluster.token_id,
            &secret,
            ca.as_deref(),
            options,
        )
    }

    /// Starts snapshot `name` of `vm` (disk only, without RAM) and returns
    /// the task's UPID.
    pub async fn create_snapshot(
        &self,
        vm: &VmRef,
        name: &str,
        description: &str,
    ) -> Result<String, SnapshotError> {
        let vmid = vm.vmid.to_string();
        let form = [("snapname", name), ("description", description)];
        let path = ["nodes", &vm.node, "qemu", &vmid, "snapshot"];
        upid(self.call(Method::POST, &path, Some(&form)).await?)
    }

    pub async fn delete_snapshot(&self, vm: &VmRef, name: &str) -> Result<String, SnapshotError> {
        let vmid = vm.vmid.to_string();
        let path = ["nodes", &vm.node, "qemu", &vmid, "snapshot", name];
        upid(self.call(Method::DELETE, &path, None).await?)
    }

    pub async fn rollback_snapshot(&self, vm: &VmRef, name: &str) -> Result<String, SnapshotError> {
        let vmid = vm.vmid.to_string();
        let path = [
            "nodes", &vm.node, "qemu", &vmid, "snapshot", name, "rollback",
        ];
        upid(self.call(Method::POST, &path, None).await?)
    }

    /// Snapshots of `vm`, without the `current` pseudo-snapshot.
    pub async fn list_snapshots(&self, vm: &VmRef) -> Result<Vec<ListedSnapshot>, SnapshotError> {
        let vmid = vm.vmid.to_string();
        let path = ["nodes", &vm.node, "qemu", &vmid, "snapshot"];
        let data = self.call(Method::GET, &path, None).await?;
        let mut list: Vec<ListedSnapshot> = serde_json::from_value(data)
            .map_err(|e| SnapshotError::InvalidResponse(bounded(&e.to_string())))?;
        list.retain(|s| s.name != "current");
        Ok(list)
    }

    /// Polls task `upid` on `node` until it stops: `OK` or warnings
    /// succeed, any other exit status fails.
    pub async fn wait_task(&self, node: &str, upid: &str) -> Result<(), SnapshotError> {
        #[derive(Deserialize)]
        struct Status {
            status: String,
            exitstatus: Option<String>,
        }
        let deadline = tokio::time::Instant::now() + self.options.task_timeout;
        loop {
            let data = self
                .call(Method::GET, &["nodes", node, "tasks", upid, "status"], None)
                .await?;
            let status: Status = serde_json::from_value(data)
                .map_err(|e| SnapshotError::InvalidResponse(bounded(&e.to_string())))?;
            if status.status == "stopped" {
                let exit = status.exitstatus.unwrap_or_default();
                return if exit == "OK" || exit.starts_with("WARNINGS") {
                    Ok(())
                } else {
                    Err(SnapshotError::TaskFailed(bounded(&exit)))
                };
            }
            if tokio::time::Instant::now() + self.options.poll_interval > deadline {
                return Err(SnapshotError::Timeout {
                    upid: upid.to_owned(),
                });
            }
            tokio::time::sleep(self.options.poll_interval).await;
        }
    }

    /// Sends one request under `/api2/json/` and returns the answer's
    /// `data`. Every segment is percent-encoded.
    async fn call(
        &self,
        method: Method,
        segments: &[&str],
        form: Option<&[(&str, &str)]>,
    ) -> Result<Value, SnapshotError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("an https URL has a path")
            .pop_if_empty()
            .extend(["api2", "json"])
            .extend(segments);
        let mut request = self
            .http
            .request(method, url)
            .header(AUTHORIZATION, self.auth.clone());
        if let Some(form) = form {
            request = request.form(form);
        }
        let transport = |e: reqwest::Error| SnapshotError::Transport(e.without_url().to_string());
        let response = request.send().await.map_err(transport)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(transport)?;
        let body: Option<Value> = serde_json::from_slice(&bytes).ok();
        if status == 401 || status == 403 {
            return Err(SnapshotError::Auth {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            let message = body
                .as_ref()
                .and_then(|b| b.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| status.canonical_reason().map(str::to_owned))
                .unwrap_or_default();
            return Err(SnapshotError::Api {
                status: status.as_u16(),
                message: bounded(&message),
            });
        }
        body.and_then(|mut b| b.get_mut("data").map(Value::take))
            .ok_or_else(|| SnapshotError::InvalidResponse("no `data` in the answer".into()))
    }
}

/// The UPID a task-starting call answers.
fn upid(data: Value) -> Result<String, SnapshotError> {
    match data.as_str() {
        Some(upid) if upid.starts_with("UPID:") && upid.len() <= MAX_UPID => Ok(upid.to_owned()),
        _ => Err(SnapshotError::InvalidResponse(
            "expected a task UPID".into(),
        )),
    }
}

/// Reads a token secret file: the secret alone, surrounding whitespace
/// ignored. Errors never quote the content.
pub async fn read_secret(path: &Path) -> Result<Secret, SnapshotError> {
    let fail = |reason: String| {
        SnapshotError::Config(format!("token secret file {}: {reason}", path.display()))
    };
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| fail(e.to_string()))?;
    let secret = text.trim();
    if secret.is_empty() {
        return Err(fail("the file is empty".into()));
    }
    Ok(Secret::new(secret))
}

/// [`Snapshotter`] over Proxmox VE: servers mapped to VMs in
/// `server_proxmox_vms`, snapshots recorded in `proxmox_snapshots`.
pub struct ProxmoxSnapshotter {
    pool: PgPool,
    clusters: HashMap<String, ProxmoxClient>,
    /// Snapshots kept per VM, at least 1.
    keep: usize,
}

impl ProxmoxSnapshotter {
    /// With no cluster, every server is [`super::Unsupported::Disabled`].
    pub fn new(pool: PgPool, clusters: HashMap<String, ProxmoxClient>, keep: usize) -> Self {
        Self {
            pool,
            clusters,
            keep: keep.max(1),
        }
    }

    /// Builds a client per configured cluster, none when disabled.
    pub async fn from_config(pool: PgPool, config: &ProxmoxConfig) -> Result<Self, SnapshotError> {
        let mut clusters = HashMap::new();
        if config.enabled {
            let options = ProxmoxOptions::from_config(config);
            for cluster in &config.clusters {
                let client = ProxmoxClient::from_config(cluster, options.clone()).await?;
                clusters.insert(cluster.name.clone(), client);
            }
        }
        Ok(Self::new(pool, clusters, config.keep_snapshots as usize))
    }

    async fn vm(&self, server: Uuid) -> Result<(VmRef, &ProxmoxClient), SnapshotError> {
        match self.support(server).await? {
            Support::Available(vm) => {
                let client = &self.clusters[&vm.cluster];
                Ok((vm, client))
            }
            Support::Unavailable(reason) => Err(SnapshotError::Unsupported(reason)),
        }
    }

    async fn audit(&self, action: &str, server: Uuid, details: Value) -> Result<(), sqlx::Error> {
        audit::record(
            &self.pool,
            Event {
                actor_kind: "system",
                actor_id: "remediation",
                action,
                target: Some(("server", &server.to_string())),
                details,
            },
        )
        .await
    }

    /// Deletes MonitorFlow's snapshots of `vm` beyond the newest `keep`.
    /// One already gone from Proxmox is only marked deleted.
    async fn retain(
        &self,
        server: Uuid,
        vm: &VmRef,
        client: &ProxmoxClient,
    ) -> Result<(), SnapshotError> {
        let live = store::live(&self.pool, vm).await?;
        let old = store::beyond_retention(&live, self.keep);
        if old.is_empty() {
            return Ok(());
        }
        let present: HashSet<String> = client
            .list_snapshots(vm)
            .await?
            .into_iter()
            .map(|s| s.name)
            .collect();
        for name in old {
            // Recorded names always parse; checked again before deleting.
            if SnapshotName::parse(name).is_none() {
                continue;
            }
            if present.contains(name) {
                let upid = client.delete_snapshot(vm, name).await?;
                client.wait_task(&vm.node, &upid).await?;
            }
            store::mark_deleted(&self.pool, vm, name).await?;
            self.audit(
                "proxmox.snapshot.delete",
                server,
                json!({ "cluster": vm.cluster, "node": vm.node, "vmid": vm.vmid,
                        "name": name, "present": present.contains(name) }),
            )
            .await?;
            tracing::info!(cluster = %vm.cluster, vmid = vm.vmid, %name, "deleted old snapshot");
        }
        Ok(())
    }
}

#[async_trait]
impl Snapshotter for ProxmoxSnapshotter {
    async fn support(&self, server: Uuid) -> Result<Support, SnapshotError> {
        let names: Vec<String> = self.clusters.keys().cloned().collect();
        let mapping = store::mapping(&self.pool, server).await?;
        Ok(Support::of(!names.is_empty(), &names, mapping))
    }

    async fn take(
        &self,
        server: Uuid,
        request: SnapshotRequest,
    ) -> Result<Snapshot, SnapshotError> {
        let (vm, client) = self.vm(server).await?;
        let name = request.name();
        let description = request.description();
        let ours = |created_at| Snapshot {
            name: name.to_string(),
            description: description.clone(),
            created_at: Some(created_at),
            managed: true,
        };
        if let Some(at) = store::recorded(&self.pool, &vm, name.as_str()).await? {
            return Ok(ours(at));
        }
        // A task that outlived its timeout may have finished since: adopt
        // the snapshot when it carries this step's description.
        let existing = client.list_snapshots(&vm).await?;
        match existing.iter().find(|s| s.name == name.as_str()) {
            Some(s) if s.description.trim() == description => {}
            Some(_) => return Err(SnapshotError::NameTaken(name.to_string())),
            None => {
                let upid = client
                    .create_snapshot(&vm, name.as_str(), &description)
                    .await?;
                client.wait_task(&vm.node, &upid).await?;
            }
        }
        let at = store::record(&self.pool, &vm, server, name.as_str()).await?;
        self.audit(
            "proxmox.snapshot.create",
            server,
            json!({ "cluster": vm.cluster, "node": vm.node, "vmid": vm.vmid, "name": name,
                    "incident": request.incident, "step": request.step }),
        )
        .await?;
        tracing::info!(cluster = %vm.cluster, vmid = vm.vmid, %name, "took snapshot before a change");
        // Retention never fails the snapshot just taken.
        if let Err(error) = self.retain(server, &vm, client).await {
            tracing::warn!(cluster = %vm.cluster, vmid = vm.vmid, %error, "snapshot retention failed");
        }
        Ok(ours(at))
    }

    async fn list(&self, server: Uuid) -> Result<Vec<Snapshot>, SnapshotError> {
        let (vm, client) = self.vm(server).await?;
        let managed: HashSet<String> = store::live(&self.pool, &vm).await?.into_iter().collect();
        let mut list: Vec<Snapshot> = client
            .list_snapshots(&vm)
            .await?
            .into_iter()
            .map(|s| Snapshot {
                managed: managed.contains(&s.name),
                created_at: s
                    .snaptime
                    .and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)),
                description: s.description.trim().to_owned(),
                name: s.name,
            })
            .collect();
        list.sort_by_key(|s| s.created_at);
        Ok(list)
    }

    async fn rollback(&self, server: Uuid, name: &SnapshotName) -> Result<(), SnapshotError> {
        let (vm, client) = self.vm(server).await?;
        if store::recorded(&self.pool, &vm, name.as_str())
            .await?
            .is_none()
        {
            return Err(SnapshotError::UnknownSnapshot(name.to_string()));
        }
        let upid = client.rollback_snapshot(&vm, name.as_str()).await?;
        client.wait_task(&vm.node, &upid).await?;
        self.audit(
            "proxmox.snapshot.rollback",
            server,
            json!({ "cluster": vm.cluster, "node": vm.node, "vmid": vm.vmid, "name": name }),
        )
        .await?;
        tracing::warn!(cluster = %vm.cluster, vmid = vm.vmid, %name, "rolled a server back to a snapshot");
        Ok(())
    }
}
