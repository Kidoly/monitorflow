//! The command line prints its result and nothing else on stdout. Tests
//! needing PostgreSQL are skipped without `DATABASE_URL`.

use std::process::{Command, Output};

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

/// Runs the server binary with an empty configuration file and a log level
/// that would show everything, had the command kept it.
fn server(database_url: &str, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(&config, "").unwrap();
    Command::new(env!("CARGO_BIN_EXE_monitorflow-server"))
        .arg("--config")
        .arg(&config)
        .args(args)
        .env("MONITORFLOW_DATABASE_URL", database_url)
        .env("MONITORFLOW_LOG_LEVEL", "debug")
        .env("MONITORFLOW_DATA_DIR", dir.path())
        .output()
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_command_prints_only_its_result(pool: sqlx::PgPool) {
        let name: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let base = std::env::var("DATABASE_URL").unwrap();
        let (server_url, _) = base.rsplit_once('/').unwrap();
        let url = format!("{server_url}/{name}");

        let output =
            tokio::task::spawn_blocking(move || server(&url, &["customer", "add", "--name", "acme"]))
                .await
                .unwrap();

        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "{stdout}");
        assert!(stdout.starts_with("created customer acme"), "{stdout}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    }
}
