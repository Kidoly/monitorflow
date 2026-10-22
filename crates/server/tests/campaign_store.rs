//! Campaign storage (ADR-0033): targets frozen into waves, windows and
//! their evaluation in PostgreSQL, user actions and their audit. Ignored
//! without `DATABASE_URL`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_server::campaign::{
    Action, CampaignError, CampaignState, CampaignStore, FailureThreshold, LocalTime, NewCampaign,
    UpdateSpec, WaveSpec, Window,
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

const USER: (&str, &str) = ("user", "tester");

struct Tenant {
    customer: Uuid,
    site: Uuid,
    /// web-1, web-2, web-3 of the site.
    servers: Vec<Uuid>,
}

async fn tenant(pool: &PgPool, name: &str) -> Tenant {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid =
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(format!("site-{name}"))
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
    let mut servers = Vec::new();
    // Inserted out of order: targets are sorted by hostname.
    for n in [3, 1, 2] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, $3, 'debian') RETURNING id",
        )
        .bind(format!("agent-{name}-{n}"))
        .bind(site)
        .bind(format!("web-{n}"))
        .fetch_one(pool)
        .await
        .unwrap();
        servers.push((n, id));
    }
    servers.sort();
    Tenant {
        customer,
        site,
        servers: servers.into_iter().map(|(_, id)| id).collect(),
    }
}

fn campaign(t: &Tenant) -> NewCampaign {
    NewCampaign {
        customer_id: t.customer,
        name: "october".into(),
        site_id: Some(t.site),
        server_ids: Vec::new(),
        waves: vec![WaveSpec::Size(1)],
        spec: UpdateSpec {
            security_only: true,
            ..UpdateSpec::default()
        },
        command_ttl: Duration::from_secs(3600),
        failure_threshold: FailureThreshold::default(),
        windows: Vec::new(),
        created_by: None,
    }
}

async fn waves(pool: &PgPool, campaign: Uuid) -> Vec<Vec<Uuid>> {
    let rows: Vec<(i32, Vec<Uuid>)> = sqlx::query_as(
        "SELECT wave, array_agg(t.server_id ORDER BY s.hostname) FROM campaign_targets t
         JOIN servers s ON s.id = t.server_id
         WHERE campaign_id = $1 GROUP BY wave ORDER BY wave",
    )
    .bind(campaign)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().map(|(_, ids)| ids).collect()
}

async fn window_end(pool: &PgPool, campaign: Uuid, at: &str) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT campaign_window_end($1, $2::timestamptz)")
        .bind(campaign)
        .bind(at)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

async fn audit_actions(pool: &PgPool, campaign: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE target_kind = 'campaign' AND target_id = $1
         ORDER BY id",
    )
    .bind(campaign.to_string())
    .fetch_all(pool)
    .await
    .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn targets_are_frozen_into_waves(pool: PgPool) {
        let t = tenant(&pool, "acme").await;
        let store = CampaignStore::new(pool.clone());
        let row = store.create(&campaign(&t), USER).await.unwrap();
        assert_eq!(row.state, CampaignState::Draft);
        assert_eq!(row.current_wave, None);
        assert!(row.security_only);
        assert_eq!(
            waves(&pool, row.id).await,
            vec![vec![t.servers[0]], vec![t.servers[1], t.servers[2]]]
        );
        assert_eq!(audit_actions(&pool, row.id).await, ["campaign.create"]);

        // Explicit servers and waves, and a revoked server of the site left out.
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(t.servers[0])
            .execute(&pool)
            .await
            .unwrap();
        let mut new = campaign(&t);
        new.waves = vec![WaveSpec::Servers(vec![t.servers[2]])];
        let row = store.create(&new, USER).await.unwrap();
        assert_eq!(waves(&pool, row.id).await, vec![vec![t.servers[2]], vec![t.servers[1]]]);
    }

    #[sqlx::test]
    async fn targets_stay_within_the_customer(pool: PgPool) {
        let a = tenant(&pool, "acme").await;
        let b = tenant(&pool, "globex").await;
        let store = CampaignStore::new(pool.clone());
        let invalid = |e: Result<_, CampaignError>| match e {
            Err(CampaignError::Invalid(message)) => message,
            other => panic!("{other:?}"),
        };

        let mut new = campaign(&a);
        new.server_ids = vec![b.servers[0]];
        assert!(invalid(store.create(&new, USER).await).starts_with("unknown server"));
        let mut new = campaign(&a);
        new.site_id = Some(b.site);
        assert!(invalid(store.create(&new, USER).await).starts_with("unknown site"));
        let mut new = campaign(&a);
        new.site_id = None;
        new.waves = vec![WaveSpec::Servers(vec![b.servers[0]])];
        assert_eq!(invalid(store.create(&new, USER).await), "the campaign targets no server");
        new.server_ids = vec![a.servers[0]];
        assert!(invalid(store.create(&new, USER).await).contains("is not a target"));

        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(a.servers[0])
            .execute(&pool)
            .await
            .unwrap();
        let mut new = campaign(&a);
        new.server_ids = vec![a.servers[0]];
        assert!(invalid(store.create(&new, USER).await).ends_with("is revoked"));

        let mut new = campaign(&a);
        new.windows = vec![Window::Weekly {
            weekdays: vec![1],
            start: LocalTime(60),
            end: LocalTime(120),
            timezone: "Mars/Olympus".into(),
        }];
        assert!(invalid(store.create(&new, USER).await).starts_with("unknown time zone"));
        let campaigns: i64 = sqlx::query_scalar("SELECT count(*) FROM campaigns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(campaigns, 0);
    }

    #[sqlx::test]
    async fn windows_are_evaluated_in_their_time_zone(pool: PgPool) {
        let t = tenant(&pool, "acme").await;
        let store = CampaignStore::new(pool.clone());
        let row = store.create(&campaign(&t), USER).await.unwrap();
        // No window: always open.
        let open: bool = sqlx::query_scalar("SELECT campaign_window_end($1, now()) = 'infinity'")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(open);

        let mut new = campaign(&t);
        new.windows = vec![
            // Saturdays 22:00 to Sunday 04:00 in Paris, across the end of
            // summer time on 2026-10-25.
            Window::Weekly {
                weekdays: vec![6],
                start: "22:00".parse().unwrap(),
                end: "04:00".parse().unwrap(),
                timezone: "Europe/Paris".into(),
            },
            Window::Once {
                starts_at: at("2026-10-28T10:00:00Z"),
                ends_at: at("2026-10-28T11:00:00Z"),
            },
        ];
        let row = store.create(&new, USER).await.unwrap();
        assert_eq!(store.windows(row.id).await.unwrap(), new.windows);
        let cases = [
            ("2026-10-24T19:59:00Z", None),
            ("2026-10-24T20:00:00Z", Some("2026-10-25T03:00:00Z")),
            ("2026-10-25T01:30:00Z", Some("2026-10-25T03:00:00Z")),
            ("2026-10-25T03:00:00Z", None),
            ("2026-10-26T20:30:00Z", None),
            ("2026-10-28T10:30:00Z", Some("2026-10-28T11:00:00Z")),
            ("2026-10-28T11:00:00Z", None),
            ("2026-10-31T23:00:00Z", Some("2026-11-01T03:00:00Z")),
        ];
        for (now, end) in cases {
            assert_eq!(window_end(&pool, row.id, now).await, end.map(at), "{now}");
        }
    }

    #[sqlx::test]
    async fn actions_follow_the_state_machine_and_are_audited(pool: PgPool) {
        let t = tenant(&pool, "acme").await;
        let store = CampaignStore::new(pool.clone());
        let id = store.create(&campaign(&t), USER).await.unwrap().id;
        let conflict = |e: Result<_, CampaignError>| {
            assert!(matches!(e, Err(CampaignError::Transition { .. })), "{e:?}");
        };
        conflict(store.act(id, Action::Pause, USER).await);
        conflict(store.act(id, Action::Resume, USER).await);
        let row = store.act(id, Action::Start, USER).await.unwrap();
        assert_eq!(row.state, CampaignState::Scheduled);
        assert!(row.started_at.is_some());
        conflict(store.act(id, Action::Start, USER).await);
        assert_eq!(store.act(id, Action::Pause, USER).await.unwrap().state, CampaignState::Paused);
        assert_eq!(store.act(id, Action::Resume, USER).await.unwrap().state, CampaignState::Scheduled);
        let row = store.act(id, Action::Stop, USER).await.unwrap();
        assert_eq!(row.state, CampaignState::Stopped);
        assert!(row.finished_at.is_some());
        assert_eq!(row.error.as_deref(), Some("stopped by user"));
        conflict(store.act(id, Action::Resume, USER).await);
        assert!(matches!(
            store.act(Uuid::new_v4(), Action::Start, USER).await,
            Err(CampaignError::NotFound)
        ));
        assert_eq!(
            audit_actions(&pool, id).await,
            ["campaign.create", "campaign.start", "campaign.pause", "campaign.resume", "campaign.stop"]
        );
    }

    #[sqlx::test]
    async fn pausing_cancels_unsent_commands_and_detaches_them(pool: PgPool) {
        let t = tenant(&pool, "acme").await;
        let store = CampaignStore::new(pool.clone());
        let id = store.create(&campaign(&t), USER).await.unwrap().id;
        store.act(id, Action::Start, USER).await.unwrap();
        // What the runner does when it queues the first wave.
        let command: Uuid = sqlx::query_scalar(
            "WITH c AS (INSERT INTO commands (server_id, kind, payload, expires_at, campaign_id)
                        VALUES ($1, 'apply_updates', '', now() + interval '1 hour', $2) RETURNING id)
             UPDATE campaign_targets SET command_id = (SELECT id FROM c)
             WHERE campaign_id = $2 AND server_id = $1 RETURNING command_id",
        )
        .bind(t.servers[0])
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE campaigns SET state = 'running', current_wave = 0 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        store.act(id, Action::Pause, USER).await.unwrap();
        let (state, error): (String, Option<String>) =
            sqlx::query_as("SELECT state, error FROM commands WHERE id = $1")
                .bind(command)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((state.as_str(), error.as_deref()), ("cancelled", Some("campaign paused")));
        let attached: Option<Uuid> =
            sqlx::query_scalar("SELECT command_id FROM campaign_targets WHERE server_id = $1")
                .bind(t.servers[0])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(attached, None);
        let row = store.act(id, Action::Resume, USER).await.unwrap();
        assert_eq!(row.state, CampaignState::Running);
        // The foreign key from ADR-0031's column now holds.
        let bad = sqlx::query(
            "INSERT INTO commands (server_id, kind, payload, expires_at, campaign_id)
             VALUES ($1, 'apply_updates', '', now() + interval '1 hour', $2)",
        )
        .bind(t.servers[0])
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(bad.is_err());
    }
}
