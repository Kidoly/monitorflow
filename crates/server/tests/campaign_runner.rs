//! The campaign runner (ADR-0033) against PostgreSQL, with an injected
//! clock: waves advance on results, windows gate queuing and cap TTLs,
//! failures stop the campaign, and steps are idempotent across runner
//! instances. Results go through `CommandStore` as an agent's would.
//! Ignored without `DATABASE_URL`.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use monitorflow_common::proto::v1::{ApplyUpdates, CommandResult, CommandStatus};
use monitorflow_server::campaign::{
    Action, CampaignState, CampaignStore, FailureThreshold, FailureUnit, NewCampaign, Runner, Step,
    UpdateSpec, WaveSpec, Window,
};
use monitorflow_server::commands::{CommandStore, NewCommand, State};
use prost::Message;
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

struct Fleet {
    pool: PgPool,
    store: CampaignStore,
    commands: CommandStore,
    customer: Uuid,
    site: Uuid,
    /// `(id, peer_id)` of web-1 to web-n, by hostname.
    servers: Vec<(Uuid, String)>,
}

impl Fleet {
    async fn new(pool: &PgPool, n: usize) -> Self {
        let customer: Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        let site: Uuid = sqlx::query_scalar(
            "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
        )
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
        let mut servers = Vec::new();
        for i in 1..=n {
            let peer = format!("agent-{i}");
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO servers (peer_id, site_id, hostname, os_family)
                 VALUES ($1, $2, $3, 'debian') RETURNING id",
            )
            .bind(&peer)
            .bind(site)
            .bind(format!("web-{i}"))
            .fetch_one(pool)
            .await
            .unwrap();
            servers.push((id, peer));
        }
        Self {
            pool: pool.clone(),
            store: CampaignStore::new(pool.clone()),
            commands: CommandStore::new(pool.clone()),
            customer,
            site,
            servers,
        }
    }

    fn campaign(&self, waves: Vec<WaveSpec>) -> NewCampaign {
        NewCampaign {
            customer_id: self.customer,
            name: "october".into(),
            site_id: Some(self.site),
            server_ids: Vec::new(),
            waves,
            spec: UpdateSpec {
                packages: vec!["openssl".into()],
                ..UpdateSpec::default()
            },
            command_ttl: Duration::from_secs(3600),
            failure_threshold: FailureThreshold::default(),
            windows: Vec::new(),
            sources: None,
            created_by: None,
        }
    }

    /// Creates and starts `new`.
    async fn start(&self, new: &NewCampaign) -> Uuid {
        let id = self.store.create(new, USER).await.unwrap().id;
        self.store.act(id, Action::Start, USER).await.unwrap();
        id
    }

    async fn state(&self, id: Uuid) -> (CampaignState, Option<i32>) {
        let row = self.store.get(id).await.unwrap().unwrap();
        (row.state, row.current_wave)
    }

    /// `(server index, command state)` of the campaign's commands, by
    /// server then issue time.
    async fn commands(&self, campaign: Uuid) -> Vec<(usize, State)> {
        let rows: Vec<(Uuid, State)> = sqlx::query_as(
            "SELECT c.server_id, c.state FROM commands c JOIN servers s ON s.id = c.server_id
             WHERE c.campaign_id = $1 ORDER BY s.hostname, c.issued_at",
        )
        .bind(campaign)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(server, state)| (self.index(server), state))
            .collect()
    }

    fn index(&self, server: Uuid) -> usize {
        self.servers
            .iter()
            .position(|(id, _)| *id == server)
            .unwrap()
    }

    /// The campaign command of server `i` in flight or queued.
    async fn command_of(&self, campaign: Uuid, i: usize) -> Uuid {
        sqlx::query_scalar(
            "SELECT command_id FROM campaign_targets WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(campaign)
        .bind(self.servers[i].0)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Server `i` receives its command and reports `status`.
    async fn report(&self, campaign: Uuid, i: usize, status: CommandStatus) {
        let id = self.command_of(campaign, i).await;
        assert!(self.commands.mark_sent(id).await.unwrap());
        let result = CommandResult {
            command_id: id.to_string(),
            exit_code: i32::from(status != CommandStatus::Succeeded),
            status: status as i32,
            ..CommandResult::default()
        };
        let state = self.commands.record(&self.servers[i].1, &result).await;
        assert!(state.unwrap().unwrap().is_final());
    }

    async fn audit(&self, campaign: Uuid) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE target_kind = 'campaign' AND target_id = $1
             ORDER BY id",
        )
        .bind(campaign.to_string())
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn queued(step: Step) -> Vec<String> {
    match step {
        Step::Queued(peers) => peers,
        other => panic!("expected queued commands, got {other:?}"),
    }
}

db_tests! {
    #[sqlx::test]
    async fn waves_advance_on_results_and_complete(pool: PgPool) {
        let f = Fleet::new(&pool, 3).await;
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        let id = f.start(&f.campaign(vec![WaveSpec::Size(1)])).await;
        assert_eq!(f.state(id).await, (CampaignState::Scheduled, None));

        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-1"]);
        assert_eq!(f.state(id).await, (CampaignState::Running, Some(0)));
        let command = f.commands.get(f.command_of(id, 0).await).await.unwrap().unwrap();
        assert_eq!(command.campaign_id, Some(id));
        assert_eq!(command.requested_by, None);
        let payload = ApplyUpdates::decode(command.payload.as_slice()).unwrap();
        assert_eq!(payload.packages, ["openssl"]);
        // Waiting for the result queues nothing more.
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        assert_eq!(f.commands(id).await, [(0, State::Queued)]);

        f.report(id, 0, CommandStatus::Succeeded).await;
        let mut peers = queued(runner.advance(id, now).await.unwrap());
        peers.sort();
        assert_eq!(peers, ["agent-2", "agent-3"]);
        assert_eq!(f.state(id).await, (CampaignState::Running, Some(1)));
        f.report(id, 1, CommandStatus::Succeeded).await;
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        f.report(id, 2, CommandStatus::Succeeded).await;
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Completed);
        let row = f.store.get(id).await.unwrap().unwrap();
        assert_eq!(row.state, CampaignState::Completed);
        assert!(row.finished_at.is_some());
        // Final campaigns are left alone.
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Idle);
        assert_eq!(
            f.audit(id).await,
            [
                "campaign.create", "campaign.start", "campaign.queue", "campaign.wave_finish",
                "campaign.queue", "campaign.wave_finish", "campaign.complete"
            ]
        );
    }

    #[sqlx::test]
    async fn commands_are_queued_only_inside_a_window(pool: PgPool) {
        let f = Fleet::new(&pool, 2).await;
        let runner = Runner::new(pool.clone());
        let mut new = f.campaign(vec![WaveSpec::Size(1)]);
        new.windows = vec![
            Window::Once {
                starts_at: at("2026-11-02T22:00:00Z"),
                ends_at: at("2026-11-02T23:00:00Z"),
            },
            // Saturdays 22:00 to 02:00 UTC.
            Window::Weekly {
                weekdays: vec![6],
                start: "22:00".parse().unwrap(),
                end: "02:00".parse().unwrap(),
                timezone: "UTC".into(),
            },
        ];
        let id = f.start(&new).await;
        // Friday, outside both windows.
        runner.tick(at("2026-10-30T23:00:00Z")).await;
        assert_eq!(f.state(id).await, (CampaignState::Scheduled, None));
        assert!(f.commands(id).await.is_empty());
        // Sunday 01:59:30: under a minute left.
        assert_eq!(runner.advance(id, at("2026-11-01T01:59:30Z")).await.unwrap(), Step::Idle);
        // Sunday 01:30, in Saturday's window: the TTL ends with it.
        queued(runner.advance(id, at("2026-11-01T01:30:00Z")).await.unwrap());
        let first = f.commands.get(f.command_of(id, 0).await).await.unwrap().unwrap();
        let ttl = first.expires_at - first.issued_at;
        assert!((ttl - TimeDelta::minutes(30)).abs() < TimeDelta::seconds(2), "{ttl}");

        // The next wave waits for the next window, here the one-off one,
        // which caps the TTL below `command_ttl`.
        f.report(id, 0, CommandStatus::Succeeded).await;
        assert_eq!(runner.advance(id, at("2026-11-02T12:00:00Z")).await.unwrap(), Step::Idle);
        assert_eq!(f.state(id).await, (CampaignState::Running, Some(0)));
        queued(runner.advance(id, at("2026-11-02T22:15:00Z")).await.unwrap());
        assert_eq!(f.state(id).await, (CampaignState::Running, Some(1)));
        let second = f.commands.get(f.command_of(id, 1).await).await.unwrap().unwrap();
        let ttl = second.expires_at - second.issued_at;
        assert!((ttl - TimeDelta::minutes(45)).abs() < TimeDelta::seconds(2), "{ttl}");
    }

    #[sqlx::test]
    async fn a_failure_stops_the_campaign_and_in_flight_commands_finish(pool: PgPool) {
        let f = Fleet::new(&pool, 4).await;
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        let id = f.start(&f.campaign(vec![WaveSpec::Size(3)])).await;
        queued(runner.advance(id, now).await.unwrap());
        // web-1 fails, web-2 is still running, web-3 not sent yet.
        f.report(id, 0, CommandStatus::Failed).await;
        let in_flight = f.command_of(id, 1).await;
        assert!(f.commands.mark_sent(in_flight).await.unwrap());

        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Failed);
        let row = f.store.get(id).await.unwrap().unwrap();
        assert_eq!(row.state, CampaignState::Failed);
        assert_eq!(row.error.as_deref(), Some("wave 0: 1 of 3 servers failed"));
        assert_eq!(
            f.commands(id).await,
            [(0, State::Failed), (1, State::Sent), (2, State::Cancelled)]
        );
        // Nothing more is queued, and the late result is still recorded.
        runner.tick(now).await;
        f.commands
            .record(
                "agent-2",
                &CommandResult {
                    command_id: in_flight.to_string(),
                    status: CommandStatus::Succeeded as i32,
                    ..CommandResult::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(f.commands(id).await.len(), 3);
        assert_eq!(f.commands(id).await[1], (1, State::Succeeded));
        assert_eq!(f.audit(id).await.last().unwrap(), "campaign.fail");
    }

    #[sqlx::test]
    async fn a_percentage_threshold_tolerates_some_failures(pool: PgPool) {
        let f = Fleet::new(&pool, 3).await;
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        let mut new = f.campaign(vec![WaveSpec::Size(2)]);
        new.failure_threshold = FailureThreshold { value: 50, unit: FailureUnit::Percent };
        let id = f.start(&new).await;
        queued(runner.advance(id, now).await.unwrap());
        f.report(id, 0, CommandStatus::Failed).await;
        f.report(id, 1, CommandStatus::Succeeded).await;
        // 1 of 2 is not more than 50 %: the next wave goes.
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-3"]);
        // 1 of 1 is.
        f.report(id, 2, CommandStatus::Rejected).await;
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Failed);
    }

    #[sqlx::test]
    async fn steps_are_idempotent_across_restarts_and_pauses(pool: PgPool) {
        let f = Fleet::new(&pool, 2).await;
        let now = Utc::now();
        let id = f.start(&f.campaign(Vec::new())).await;
        // Two runners, as after a restart or with two server processes.
        let (first, second) = (Runner::new(pool.clone()), Runner::new(pool.clone()));
        tokio::join!(first.tick(now), second.tick(now));
        first.tick(now).await;
        second.tick(now).await;
        assert_eq!(f.commands(id).await, [(0, State::Queued), (1, State::Queued)]);

        // web-1 starts, then the campaign is paused: web-2's unsent
        // command is cancelled and nothing is queued while paused.
        let running = f.command_of(id, 0).await;
        assert!(f.commands.mark_sent(running).await.unwrap());
        f.store.act(id, Action::Pause, USER).await.unwrap();
        first.tick(now).await;
        assert_eq!(f.commands(id).await, [(0, State::Sent), (1, State::Cancelled)]);
        // On resume web-2 gets a new command, once.
        f.store.act(id, Action::Resume, USER).await.unwrap();
        assert_eq!(queued(second.advance(id, now).await.unwrap()), ["agent-2"]);
        first.tick(now).await;
        assert_eq!(
            f.commands(id).await,
            [(0, State::Sent), (1, State::Cancelled), (1, State::Queued)]
        );
    }

    #[sqlx::test]
    async fn campaign_commands_respect_one_in_flight_per_server(pool: PgPool) {
        let f = Fleet::new(&pool, 1).await;
        let runner = Runner::new(pool.clone());
        // A manual update is in flight on web-1.
        let manual = f
            .commands
            .enqueue(&NewCommand {
                server_id: f.servers[0].0,
                kind: "apply_updates".into(),
                payload: Vec::new(),
                ttl: Duration::from_secs(600),
                requested_by: None,
                campaign_id: None,
            })
            .await
            .unwrap();
        assert!(f.commands.mark_sent(manual.id).await.unwrap());
        let id = f.start(&f.campaign(Vec::new())).await;
        queued(runner.advance(id, Utc::now()).await.unwrap());
        // Queued behind it: the dispatcher cannot send it yet.
        let command = f.command_of(id, 0).await;
        assert!(!f.commands.mark_sent(command).await.unwrap());
        assert_eq!(f.commands.next("agent-1").await.unwrap().unwrap().id, manual.id);
    }

    #[sqlx::test]
    async fn servers_revoked_since_creation_count_as_failed(pool: PgPool) {
        let f = Fleet::new(&pool, 2).await;
        let runner = Runner::new(pool.clone());
        let now = Utc::now();
        let mut new = f.campaign(Vec::new());
        new.failure_threshold = FailureThreshold { value: 1, unit: FailureUnit::Count };
        let id = f.start(&new).await;
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(f.servers[0].0)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(queued(runner.advance(id, now).await.unwrap()), ["agent-2"]);
        let error: Option<String> = sqlx::query_scalar(
            "SELECT error FROM campaign_targets WHERE campaign_id = $1 AND server_id = $2",
        )
        .bind(id)
        .bind(f.servers[0].0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(error.as_deref(), Some("server revoked"));
        f.report(id, 1, CommandStatus::Succeeded).await;
        assert_eq!(runner.advance(id, now).await.unwrap(), Step::Completed);
    }
}
