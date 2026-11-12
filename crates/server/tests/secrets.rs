//! The secrets key of the server binary (ADR-0110): an added central server
//! never makes a key of its own, and refuses to start without the first
//! one's before it takes a place. Ignored without `DATABASE_URL`.

use std::process::Command;

use sqlx::PgPool;

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

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn an_added_server_without_the_key_does_not_start(pool: PgPool) {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let base = std::env::var("DATABASE_URL").unwrap();
        let (server, _) = base.rsplit_once('/').unwrap();
        let url = format!("{server}/{database}?sslmode=disable");
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server.toml");
        std::fs::write(&config, "").unwrap();
        let data = dir.path().to_owned();
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_monitorflow-server"))
                .arg("--config")
                .arg(&config)
                .env("MONITORFLOW_DATABASE_URL", url)
                .env("MONITORFLOW_ADDITIONAL_INSTANCE", "true")
                .env("MONITORFLOW_DATA_DIR", &data)
                .env("HOSTNAME", "host2")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(!output.status.success());
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("there is no secrets key at"), "{said}");
        assert!(said.contains("install-server.sh --add-server"), "{said}");
        assert!(!dir.path().join("secrets").exists());
        let places: i64 = sqlx::query_scalar("SELECT count(*) FROM server_instance_admissions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(places, 0);
    }
}
