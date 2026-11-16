//! The storage cap of on-demand mirrors (ADR-0142): its default and the
//! administrator's value, and eviction down to it that spares what a
//! campaign pins. Packages are sparse files, so gigabytes cost nothing.
//! Ignored without `DATABASE_URL`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use monitorflow_common::config::MirrorConfig;
use monitorflow_common::mirror::evict::GIB;
use monitorflow_common::mirror::packages::{PackageList, PACKAGES};
use monitorflow_common::mirror::repo::meta::{Algorithm, Checksum, Entry, Package};
use monitorflow_common::mirror::{swap_current, write_pinned};
use monitorflow_server::mirror::{cache, CommandRunner, Family, Layout, Mirrors};
use sqlx::PgPool;

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

fn checksum(c: char) -> Checksum {
    Checksum::new(Algorithm::Sha256, &c.to_string().repeat(64)).unwrap()
}

/// A stored package of `size` bytes (sparse), last used `days` ago.
fn put(layout: &Layout, c: char, size: u64, days: u64) {
    let path = layout.store().package_path(&checksum(c));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(size).unwrap();
    let used = SystemTime::now() - Duration::from_secs(days * 24 * 3600);
    file.set_times(std::fs::FileTimes::new().set_modified(used))
        .unwrap();
}

fn list(dir: &Path, packages: &[char]) {
    std::fs::create_dir_all(dir).unwrap();
    let packages = packages
        .iter()
        .map(|c| Package {
            entry: Entry {
                path: format!("pool/{c}.deb"),
                size: 1,
                checksum: checksum(*c),
            },
            name: c.to_string(),
            version: "1".into(),
            arch: "amd64".into(),
        })
        .collect();
    PackageList::new(packages)
        .unwrap()
        .write(&dir.join(PACKAGES))
        .unwrap();
}

fn mirrors(pool: &PgPool, root: &Path) -> Mirrors {
    let config = MirrorConfig {
        aptly: "/nonexistent/aptly".into(),
        reposync: "/nonexistent/reposync".into(),
        createrepo_c: "/nonexistent/createrepo_c".into(),
        gpg: "/nonexistent/gpg".into(),
        sync_timeout: 30,
        max_concurrent_syncs: 1,
    };
    let layout = Layout::new(root);
    layout.prepare().unwrap();
    let runner = CommandRunner::new(&config, layout.gnupg());
    Mirrors::new(pool.clone(), Arc::new(runner), layout)
}

db_tests! {
    #[sqlx::test]
    async fn the_cap_defaults_then_follows_the_administrator(pool: PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let mirrors = mirrors(&pool, dir.path());
        let cap = cache::cap(&pool, mirrors.layout()).await.unwrap();
        assert!(!cap.set);
        assert_eq!(cap.bytes, cap.default_bytes);
        assert!(cap.bytes <= 100 * GIB);

        cache::set_cap(&pool, Some(50 * GIB), ("user", "u1")).await.unwrap();
        let cap = cache::cap(&pool, mirrors.layout()).await.unwrap();
        assert_eq!((cap.bytes, cap.set), (50 * GIB, true));
        cache::set_cap(&pool, None, ("user", "u1")).await.unwrap();
        assert!(!cache::cap(&pool, mirrors.layout()).await.unwrap().set);

        let audited: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'mirror.cache_cap' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audited.len(), 2);
        assert_eq!(audited[0]["cap_bytes"], 50 * GIB);
        assert!(audited[1]["cap_bytes"].is_null());
        // Below 1 GiB is refused by the schema as by the API.
        assert!(cache::set_cap(&pool, Some(1), ("user", "u1")).await.is_err());
    }

    #[sqlx::test]
    async fn eviction_respects_the_cap_and_spares_pinned_packages(pool: PgPool) {
        let dir = tempfile::tempdir().unwrap();
        let mirrors = mirrors(&pool, dir.path());
        let layout = mirrors.layout();
        // A campaign pins `s1`, which lists `1`, the oldest package.
        let mirror = layout.served(Family::Apt).join("debian");
        list(&mirror.join("s1"), &['1']);
        list(&mirror.join("s2"), &['a', 'b']);
        swap_current(&mirror, "s2").unwrap();
        write_pinned(&mirror, &["s1".to_owned()]).unwrap();
        put(layout, '1', GIB, 90);
        put(layout, 'a', GIB, 30);
        put(layout, 'b', GIB, 20);
        put(layout, 'c', GIB, 1);

        cache::set_cap(&pool, Some(2 * GIB), ("user", "u1")).await.unwrap();
        let outcome = cache::evict_now(&mirrors).await.unwrap();
        assert_eq!(outcome.evicted, 2);
        assert_eq!(outcome.used_bytes, 2 * GIB);
        assert!(!outcome.over_cap);
        let store = layout.store();
        assert!(store.package_path(&checksum('1')).is_file(), "pinned");
        assert!(store.package_path(&checksum('c')).is_file(), "the most recent");
        assert!(!store.package_path(&checksum('a')).exists());
        assert!(!store.package_path(&checksum('b')).exists());
        assert_eq!(mirrors.last_eviction(), Some(outcome));

        // Below what is protected: the pinned package stays, and the
        // outcome says the cap is too small.
        cache::set_cap(&pool, Some(GIB), ("user", "u1")).await.unwrap();
        put(layout, 'd', GIB / 2, 0);
        let outcome = cache::evict_now(&mirrors).await.unwrap();
        assert!(store.package_path(&checksum('1')).is_file());
        assert_eq!(outcome.used_bytes, GIB);
        assert!(!outcome.over_cap);
        cache::set_cap(&pool, Some(GIB), ("user", "u1")).await.unwrap();
        put(layout, 'e', GIB, 0);
        list(&mirror.join("s1"), &['1', 'e']);
        let outcome = cache::evict_now(&mirrors).await.unwrap();
        assert!(outcome.over_cap);
        assert_eq!(outcome.protected_bytes, 2 * GIB);
    }
}
