//! Local LVM and ZFS snapshots (ADR-0094): storage reports stored with
//! package reports, the rollback view, the remediation hook falling back
//! to a local snapshot without a Proxmox VM, and retention. Ignored
//! without `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use monitorflow_common::command::{DELETE_SNAPSHOT, TAKE_SNAPSHOT};
use monitorflow_common::proto::v1::{
    DeleteSnapshot, LocalSnapshot, PackageReport, StorageReport, TakeSnapshot, Volume, VolumeKind,
};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::ca::{CaOptions, CertificateAuthority};
use monitorflow_server::commands::{CommandStore, Commands, DispatcherSettings, PackageStore};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::risk::Tier;
use monitorflow_server::remediation::runner::hook::{Change, ChangeHook};
use monitorflow_server::remediation::snapshot::{
    local, GuestKind, LocalSnapshots, NoSnapshots, Snapshot, SnapshotError, SnapshotHook,
    SnapshotName, SnapshotRequest, Snapshotter, Support, Unsupported,
};
use prost::Message;
use serde_json::{json, Value};
use sqlx::PgPool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const GIB: u64 = 1 << 30;

macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

struct Harness {
    pool: PgPool,
    settings: ApiSettings,
    customer: Uuid,
    server: Uuid,
    _ca_dir: TempDir,
    commands: Commands,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let customer: Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('acme-site', $1) RETURNING id",
        )
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        let server: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-01', $1, 'web', 'debian') RETURNING id",
        )
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
        let dir = TempDir::new().unwrap();
        let signer = Arc::new(
            CertificateAuthority::open_or_create(&dir.path().join("ca"), CaOptions::default())
                .unwrap(),
        );
        let commands = Commands::spawn(
            CommandStore::new(pool.clone()),
            Registry::default(),
            signer,
            DispatcherSettings::default(),
        );
        Self {
            pool: pool.clone(),
            settings: ApiSettings::default(),
            customer,
            server,
            _ca_dir: dir,
            commands,
        }
    }

    async fn call(&self, uri: &str, cookie: &str) -> (StatusCode, Value) {
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        );
        let request = Request::get(uri)
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn login(&self, name: &str, role: Role, scope: GrantScope) -> String {
        users::create_user_with(
            &self.pool,
            name,
            &Secret::new(PASSWORD),
            role,
            scope,
            "test",
        )
        .await
        .unwrap();
        let body = json!({ "username": name, "password": PASSWORD });
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            self.settings.clone(),
        );
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn report(&self, at_ms: i64, storage: Option<StorageReport>) {
        let report = PackageReport {
            collected_at_unix_ms: at_ms,
            storage: storage.map(Box::new),
            ..PackageReport::default()
        };
        assert!(PackageStore::new(self.pool.clone())
            .replace("web-01", &report)
            .await
            .unwrap());
    }

    async fn commands_of(&self, kind: &str) -> Vec<(Uuid, Vec<u8>)> {
        sqlx::query_as("SELECT id, payload FROM commands WHERE kind = $1 ORDER BY issued_at, id")
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    async fn finish(&self, id: Uuid, state: &str, error: Option<&str>) {
        sqlx::query(
            "UPDATE commands SET state = $2, error = $3, exit_code = 0, finished_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(state)
        .bind(error)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

fn volume(mount: &str, kind: VolumeKind, source: &str) -> Volume {
    Volume {
        mount_point: mount.into(),
        fs_type: "ext4".into(),
        kind: kind.into(),
        source: source.into(),
        size_bytes: 20 * GIB,
        free_bytes: 40 * GIB,
        pool_bytes: 100 * GIB,
        metadata_percent: 10.0,
    }
}

fn thin_host(snapshots: Vec<LocalSnapshot>) -> StorageReport {
    StorageReport {
        volumes: vec![
            volume("/", VolumeKind::LvmThin, "vg0/root"),
            volume("/boot", VolumeKind::Plain, "/dev/sda1"),
        ],
        snapshots,
        error: String::new(),
    }
}

fn snapshot(name: &str, kind: VolumeKind, created: i64) -> LocalSnapshot {
    LocalSnapshot {
        name: name.into(),
        mount_point: "/".into(),
        kind: kind.into(),
        source: format!("vg0/{name}_root"),
        origin: "vg0/root".into(),
        created_at_unix: created,
        ..LocalSnapshot::default()
    }
}

fn change(server: Uuid, incident: Uuid) -> Change<'static> {
    Change {
        incident_id: incident,
        server_id: server,
        step: 2,
        command: "systemctl restart nginx",
        tier: Tier::LowImpact,
    }
}

db_tests! {
    #[sqlx::test]
    async fn reports_say_what_a_change_gets(pool: PgPool) {
        let mut h = Harness::new(&pool).await;
        let viewer = h.login("eve", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let uri = format!("/api/servers/{}/rollback", h.server);

        let (status, body) = h.call(&uri, &viewer).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let rollback = |body: &Value| body["snapshots"].clone();
        assert_eq!(rollback(&body)["available"], false);
        assert_eq!(rollback(&body)["detail"], "the agent has not reported its storage");

        let now = Utc::now().timestamp();
        h.report(1_000, Some(thin_host(vec![snapshot("monitorflow-incident-1a2b3c4d5e6f-1", VolumeKind::LvmThin, now)]))).await;
        let (_, body) = h.call(&uri, &viewer).await;
        assert_eq!(rollback(&body)["available"], true, "{body}");
        assert_eq!(rollback(&body)["method"], "lvm");
        assert_eq!(rollback(&body)["detail"], "LVM snapshot of / (the thin pool has 40.0 GiB free)");
        assert_eq!(body["not_covered"], json!(["/boot"]));
        assert_eq!(body["local_snapshots"][0]["name"], "monitorflow-incident-1a2b3c4d5e6f-1");
        assert_eq!(body["local_snapshots"][0]["volumes"][0]["origin"], "vg0/root");

        // On a plain partition, or with sudo rules from before snapshots.
        let mut plain = thin_host(Vec::new());
        plain.volumes[0].kind = VolumeKind::Plain.into();
        h.report(2_000, Some(plain)).await;
        let (_, body) = h.call(&uri, &viewer).await;
        assert_eq!(rollback(&body)["available"], false);
        assert_eq!(rollback(&body)["detail"], "/ is on plain ext4");
        let sudo = StorageReport { error: "sudo: a password is required".into(), ..StorageReport::default() };
        h.report(3_000, Some(sudo)).await;
        assert_eq!(
            rollback(&h.call(&uri, &viewer).await.1)["detail"],
            "the sudo rules on this server predate snapshots; run the install command again"
        );

        // A report without storage forgets the previous one.
        h.report(4_000, None).await;
        assert_eq!(rollback(&h.call(&uri, &viewer).await.1)["detail"], "the agent has not reported its storage");

        // A Proxmox guest is Proxmox's, whether it can snapshot or not.
        h.report(5_000, Some(thin_host(Vec::new()))).await;
        sqlx::query("INSERT INTO server_proxmox_vms (server_id, cluster, node, vmid) VALUES ($1, 'pve', 'pve1', 101)")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        h.settings.proxmox_enabled = true;
        h.settings.proxmox_clusters = vec!["pve".into()];
        let (_, body) = h.call(&uri, &viewer).await;
        assert_eq!(rollback(&body)["method"], "proxmox");
        assert_eq!(body["not_covered"], json!([]));
        sqlx::query("UPDATE server_proxmox_vms SET cannot_snapshot = 'rootfs is on storage local (dir), which cannot take snapshots'")
            .execute(&pool)
            .await
            .unwrap();
        let (_, body) = h.call(&uri, &viewer).await;
        assert_eq!(rollback(&body)["available"], false, "no local snapshot of a guest");
        assert_eq!(rollback(&body)["reason"], "cannot_snapshot");
        assert_eq!(rollback(&body)["detail"], "rootfs is on storage local (dir), which cannot take snapshots");
        h.settings.proxmox_clusters = vec!["lab".into()];
        assert_eq!(rollback(&h.call(&uri, &viewer).await.1)["reason"], "unknown_cluster");
        // Proxmox off: the mapping says nothing, and the local snapshot is used.
        h.settings.proxmox_enabled = false;
        assert_eq!(rollback(&h.call(&uri, &viewer).await.1)["method"], "lvm");
    }

    #[sqlx::test]
    async fn the_hook_takes_a_local_snapshot_without_proxmox(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let server = h.server;
        let hook = SnapshotHook::new(Arc::new(NoSnapshots), "proxmox")
            .with_local(LocalSnapshots::new(h.commands.clone()));

        // A plain root: no command, and the reason in the event.
        let mut plain = thin_host(Vec::new());
        plain.volumes[0].kind = VolumeKind::Plain.into();
        local::store(&pool, h.server, Some(&plain)).await.unwrap();
        let snapshot = hook.before_change(&change(h.server, Uuid::new_v4())).await.unwrap();
        assert_eq!(snapshot.method, "none");
        assert_eq!(snapshot.detail, "/ is on plain ext4; rollback is limited");
        assert!(h.commands_of(TAKE_SNAPSHOT).await.is_empty());

        // On thin LVM, a take_snapshot command the hook waits for.
        local::store(&pool, h.server, Some(&thin_host(Vec::new()))).await.unwrap();
        let incident = Uuid::parse_str("1a2b3c4d-5e6f-4000-8000-000000000000").unwrap();
        let task = {
            let hook = hook.clone();
            tokio::spawn(async move { hook.before_change(&change(server, incident)).await })
        };
        let command = wait_for_command(&h, 1).await;
        let payload = TakeSnapshot::decode(command.1.as_slice()).unwrap();
        assert_eq!(payload.name, "monitorflow-incident-1a2b3c4d5e6f-2");
        h.finish(command.0, "succeeded", None).await;
        let snapshot = task.await.unwrap().unwrap();
        assert_eq!(snapshot.method, "lvm");
        assert_eq!(snapshot.name.as_deref(), Some("monitorflow-incident-1a2b3c4d5e6f-2"));
        assert_eq!(
            snapshot.detail,
            "LVM snapshot of / (the thin pool has 40.0 GiB free), before a low_impact command"
        );

        // A refusal by the helper fails the change.
        let task = {
            let hook = hook.clone();
            tokio::spawn(async move { hook.before_change(&change(server, Uuid::new_v4())).await })
        };
        let command = wait_for_command(&h, 2).await;
        h.finish(command.0, "failed", Some("no snapshot: the storage of / has only 1.0 GiB free of 100.0 GiB")).await;
        let error = task.await.unwrap().unwrap_err();
        assert!(error.contains("only 1.0 GiB free"), "{error}");

        let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'server.snapshot.take'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(audited, 2);
    }

    #[sqlx::test]
    async fn a_proxmox_guest_never_takes_a_local_snapshot(pool: PgPool) {
        let h = Harness::new(&pool).await;
        local::store(&pool, h.server, Some(&thin_host(Vec::new()))).await.unwrap();
        // Matched to a container whose storage cannot snapshot: the change
        // runs with limited rollback, and no command goes to the agent.
        let guest = Unsupported::CannotSnapshot(GuestKind::Lxc);
        let hook = SnapshotHook::new(Arc::new(Fixed(Support::Unavailable(guest))), "proxmox")
            .with_local(LocalSnapshots::new(h.commands.clone()));
        let snapshot = hook.before_change(&change(h.server, Uuid::new_v4())).await.unwrap();
        assert_eq!(snapshot.method, "none");
        assert_eq!(
            snapshot.detail,
            "the container's storage cannot take snapshots; rollback is limited"
        );
        assert!(h.commands_of(TAKE_SNAPSHOT).await.is_empty());
    }

    #[sqlx::test]
    async fn retention_deletes_once_what_is_past_its_time(pool: PgPool) {
        let h = Harness::new(&pool).await;
        // Commands are issued at the database's time.
        let now = Utc::now();
        let hours_ago = |h: i64| (now - chrono::Duration::hours(h)).timestamp();
        let report = thin_host(vec![
            // Classic, older than a day.
            snapshot("monitorflow-classic", VolumeKind::Lvm, hours_ago(25)),
            // Thin, a day old: kept.
            snapshot("monitorflow-thin", VolumeKind::LvmThin, hours_ago(24)),
        ]);
        local::store(&pool, h.server, Some(&report)).await.unwrap();

        assert_eq!(local::prune(&h.commands, now).await.unwrap(), 1);
        let deletes = h.commands_of(DELETE_SNAPSHOT).await;
        assert_eq!(deletes.len(), 1);
        assert_eq!(DeleteSnapshot::decode(deletes[0].1.as_slice()).unwrap().name, "monitorflow-classic");
        // Asked once; again only hours later if the snapshot is still there.
        assert_eq!(local::prune(&h.commands, now).await.unwrap(), 0);
        let later = now + chrono::Duration::hours(7);
        assert_eq!(local::prune(&h.commands, later).await.unwrap(), 1);

        // Revoked servers are left alone.
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(h.server)
            .execute(&pool)
            .await
            .unwrap();
        let much_later = now + chrono::Duration::days(30);
        assert_eq!(local::prune(&h.commands, much_later).await.unwrap(), 0);
    }
}

/// Proxmox answering `support` with a fixed answer, and nothing else.
struct Fixed(Support);

#[async_trait::async_trait]
impl Snapshotter for Fixed {
    async fn support(&self, _: Uuid) -> Result<Support, SnapshotError> {
        Ok(self.0.clone())
    }

    async fn take(&self, _: Uuid, _: SnapshotRequest) -> Result<Snapshot, SnapshotError> {
        panic!("no Proxmox snapshot expected")
    }

    async fn list(&self, _: Uuid) -> Result<Vec<Snapshot>, SnapshotError> {
        Ok(Vec::new())
    }

    async fn rollback(&self, _: Uuid, _: &SnapshotName) -> Result<(), SnapshotError> {
        Ok(())
    }
}

async fn wait_for_command(h: &Harness, count: usize) -> (Uuid, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let commands = h.commands_of(TAKE_SNAPSHOT).await;
        if commands.len() >= count {
            return commands[count - 1].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no take_snapshot command"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
