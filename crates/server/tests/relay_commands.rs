//! Results of relay commands (ADR-0055) stored as agents' are: from the
//! relay itself only, with output that PostgreSQL text takes. Ignored
//! without `DATABASE_URL`.

use std::time::Duration;

use monitorflow_common::proto::v1::{CommandResult, CommandStatus};
use monitorflow_server::commands::{MAX_ERROR, MAX_OUTPUT};
use monitorflow_server::relay_commands::{enqueue_in, record_result};
use sqlx::PgPool;
use uuid::Uuid;

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

async fn relay(pool: &PgPool, peer: &str) -> Uuid {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(peer)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(peer)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name, relay_version)
         VALUES ($1, $2, $1, '0.0.1') RETURNING id",
    )
    .bind(peer)
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn a_result_with_nul_and_a_long_error_is_recorded(pool: PgPool) {
        let first = relay(&pool, "relay-1").await;
        relay(&pool, "relay-2").await;
        let mut conn = pool.acquire().await.unwrap();
        let command = enqueue_in(&mut conn, first, "0.0.2", b"", Duration::from_secs(3600), None)
            .await
            .unwrap()
            .unwrap();
        drop(conn);
        sqlx::query("UPDATE relay_commands SET state = 'sent', sent_at = now() WHERE id = $1")
            .bind(command.id)
            .execute(&pool)
            .await
            .unwrap();
        let result = CommandResult {
            command_id: command.id.to_string(),
            status: CommandStatus::Failed as i32,
            exit_code: 1,
            stdout: [b"updater says \0".as_slice(), &vec![b'x'; MAX_OUTPUT]].concat(),
            stderr: b"health check failed\0".to_vec(),
            error: "e\0".repeat(MAX_ERROR),
            ..CommandResult::default()
        };

        // Another relay cannot report it.
        assert_eq!(record_result(&pool, "relay-2", &result).await.unwrap(), None);
        let recorded = record_result(&pool, "relay-1", &result).await.unwrap();
        assert_eq!(recorded, Some(("0.0.2".to_owned(), "failed")));
        let (state, stdout, stderr, error): (String, String, String, String) = sqlx::query_as(
            "SELECT state, stdout, stderr, error FROM relay_commands WHERE id = $1",
        )
        .bind(command.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, "failed");
        assert_eq!(stdout.len(), MAX_OUTPUT);
        assert_eq!(stderr, "health check failed\u{FFFD}");
        assert!(error.len() <= MAX_ERROR * 3 && !error.contains('\0'), "{}", error.len());
        // Final: a second result changes nothing.
        assert_eq!(record_result(&pool, "relay-1", &result).await.unwrap(), None);
    }
}
