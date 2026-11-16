//! Prefetching pending updates into the on-demand store (ADR-0142): what
//! agents report as pending and the current snapshot lists is fetched
//! from the upstream before any request, and nothing else. Ignored without
//! `DATABASE_URL`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use monitorflow_common::config::MirrorConfig;
use monitorflow_common::mirror::origin::{Origin, ORIGIN};
use monitorflow_common::mirror::packages::{PackageList, PACKAGES};
use monitorflow_common::mirror::repo::meta::{Algorithm, Entry, Package};
use monitorflow_common::mirror::swap_current;
use monitorflow_server::mirror::{
    prefetch, store, CommandRunner, Family, Layout, MirrorSpec, Mirrors, Mode,
};
use sqlx::PgPool;
use uuid::Uuid;

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
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

fn package(name: &str, version: &str) -> (Package, Vec<u8>) {
    let body = format!("{name} {version}").into_bytes();
    let mut hasher = Algorithm::Sha256.hasher();
    hasher.update(&body);
    let package = Package {
        entry: Entry {
            path: format!("pool/main/{name}_{version}_amd64.deb"),
            size: body.len() as u64,
            checksum: hasher.finish(),
        },
        name: name.into(),
        version: version.into(),
        arch: "amd64".into(),
    };
    (package, body)
}

async fn served(State(hits): State<Arc<AtomicUsize>>, uri: Uri) -> (StatusCode, Vec<u8>) {
    hits.fetch_add(1, Ordering::SeqCst);
    // `/debian/pool/main/<name>_<version>_amd64.deb` holds `<name> <version>`.
    let file = uri.path().rsplit('/').next().unwrap_or("");
    let mut parts = file.trim_end_matches("_amd64.deb").splitn(2, '_');
    let (name, version) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    (StatusCode::OK, format!("{name} {version}").into_bytes())
}

async fn server(pool: &PgPool, peer: &str, pending: &[(&str, &str, &str)]) -> Uuid {
    let id = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
    let customer = id("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(peer)
        .fetch_one(pool)
        .await
        .unwrap();
    let site = id("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
        .bind(peer)
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
    let server = id("INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id")
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    for (name, version, upgradable) in pending {
        sqlx::query(
            "INSERT INTO server_packages (server_id, name, arch, version, upgradable_version)
             VALUES ($1, $2, 'amd64', $3, NULLIF($4, ''))",
        )
        .bind(server)
        .bind(name)
        .bind(version)
        .bind(upgradable)
        .execute(pool)
        .await
        .unwrap();
    }
    server
}

db_tests! {
    #[sqlx::test]
    async fn pending_updates_of_the_current_snapshot_are_fetched_ahead(pool: PgPool) {
        let hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().fallback(served).with_state(hits.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        let config = MirrorConfig {
            aptly: "/nonexistent/aptly".into(),
            reposync: "/nonexistent/reposync".into(),
            createrepo_c: "/nonexistent/createrepo_c".into(),
            gpg: "/nonexistent/gpg".into(),
            sync_timeout: 30,
            max_concurrent_syncs: 1,
        };
        let layout = Layout::new(dir.path());
        layout.prepare().unwrap();
        let runner = CommandRunner::new(&config, layout.gnupg());
        let mirrors = Mirrors::new(pool.clone(), Arc::new(runner), layout.clone());
        let mirror = store::create(&pool, &MirrorSpec {
            name: "debian".into(),
            family: Family::Apt,
            upstream_url: format!("{base}/debian"),
            distribution: "bookworm".into(),
            components: vec!["main".into()],
            architectures: vec!["amd64".into()],
            sync_interval_s: None,
            auto_snapshot: true,
            mode: Mode::OnDemand,
        }, ("system", "test")).await.unwrap();

        // The current snapshot lists bash 5.2-2 (pending on two servers),
        // curl 8.1 (pending on one) and vim (installed, nothing pending).
        let (bash, bash_body) = package("bash", "5.2-2");
        let (curl, curl_body) = package("curl", "8.1");
        let (vim, vim_body) = package("vim", "9.1");
        let snapshot = layout.mirror_dir(&mirror).join("s1");
        std::fs::create_dir_all(&snapshot).unwrap();
        PackageList::new(vec![bash, curl, vim]).unwrap().write(&snapshot.join(PACKAGES)).unwrap();
        Origin { upstream: format!("{base}/debian"), archives: vec![] }
            .write(&snapshot.join(ORIGIN)).unwrap();
        swap_current(&layout.mirror_dir(&mirror), "s1").unwrap();
        server(&pool, "web-1", &[("bash", "5.2-1", "5.2-2"), ("vim", "9.1", "")]).await;
        server(&pool, "web-2", &[("bash", "5.2-1", "5.2-2"), ("curl", "8.0", "8.1"), ("openssl", "3.0", "3.1")]).await;

        let done = prefetch::after_sync(&mirrors, &mirror).await;
        assert_eq!((done.matched, done.stored, done.failed), (2, 2, 0));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let stored = |body: &[u8]| {
            let mut hasher = Algorithm::Sha256.hasher();
            hasher.update(body);
            layout.store().package_path(&hasher.finish()).is_file()
        };
        assert!(stored(&bash_body) && stored(&curl_body));
        assert!(!stored(&vim_body));

        // Again: everything is in the store, nothing is downloaded.
        let done = prefetch::after_sync(&mirrors, &mirror).await;
        assert_eq!(done.stored, 2);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A full mirror is never prefetched for.
        let full = monitorflow_server::mirror::Mirror { mode: Mode::Full, ..mirror };
        assert_eq!(prefetch::after_sync(&mirrors, &full).await.matched, 0);
    }
}
