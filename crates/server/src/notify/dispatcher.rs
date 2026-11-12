//! Delivers the outbox: claims pending events, renders them, and sends
//! each to every channel that wants it, a few tries per delivery. At most
//! once: a claimed event is not sent again after a restart. What a silence
//! covers is held back instead of claimed (ADR-0079). A customer's events
//! are claimed together, at most once a [`WINDOW`], and grouped into a few
//! messages per channel (ADR-0080). Whoever is on call for an event's place
//! gets the same messages in person (ADR-0081).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio::time::{interval, sleep, MissedTickBehavior};
use uuid::Uuid;

use super::backup;
use super::chat::gather;
use super::follow_up;
use super::group::{self, render_group, Group, Outgoing};
use super::message::{render, with_acknowledge, Event, Message, ACKNOWLEDGE_PATH};
use super::on_call;
use super::send::{deliver, http_client, public_http_client, SendError};
use super::silence::{self, Budget};
use super::{channels, record_outcome, Channel, Coverage, Destination, Service};
use crate::api::one_time::OneTimeToken;

/// Time between two looks at the outbox.
pub const POLL: Duration = Duration::from_secs(5);
/// Events claimed per pass.
const BATCH: i64 = 100;
/// Events older than this when claimed are dropped: after a long outage a
/// flood of stale alerts helps no one.
const MAX_AGE: chrono::TimeDelta = chrono::TimeDelta::hours(24);
/// Passes between two looks at the updater's status: a minute at the
/// default poll.
const BACKUP_CHECK_PASSES: u64 = 12;
/// Delivered events are kept this long, then deleted.
const KEEP_DAYS: i32 = 7;
/// How long the first event of a quiet customer waits, for what the same
/// check writes with it (ADR-0080).
pub const WAIT: Duration = Duration::from_secs(10);
/// After a customer's events were claimed, the next ones wait until this
/// long after, so that a customer gets at most one round of messages per
/// window. With [`POLL`], the most an event waits for grouping.
pub const WINDOW: Duration = Duration::from_secs(60);

/// Events of a pass copied for one channel or one person, by event ID.
#[derive(Debug, Default)]
struct Targets {
    channels: HashMap<i64, Uuid>,
    people: HashMap<i64, Uuid>,
}

impl Targets {
    fn contains(&self, id: i64) -> bool {
        self.channels.contains_key(&id) || self.people.contains_key(&id)
    }
}

#[derive(Clone)]
pub struct Dispatcher {
    pool: PgPool,
    /// Base URL of the web interface, for links.
    web_url: String,
    http: reqwest::Client,
    /// For channels limited to public addresses.
    public_http: reqwest::Client,
    /// Waits before the second and later tries of a delivery.
    retries: Vec<Duration>,
    /// Whether chat channels get their service's pace.
    paced: bool,
    /// The updater's shared directory (`server_update_dir`), to hear of
    /// failed backups; `None` without an updater.
    status_dir: Option<PathBuf>,
    /// [`WAIT`] and [`WINDOW`].
    wait: Duration,
    window: Duration,
}

impl Dispatcher {
    pub fn new(pool: PgPool, web_url: impl Into<String>) -> Self {
        Self {
            pool,
            web_url: web_url.into().trim_end_matches('/').to_owned(),
            http: http_client(),
            public_http: public_http_client(),
            retries: vec![Duration::from_secs(2), Duration::from_secs(10)],
            paced: true,
            status_dir: None,
            wait: WAIT,
            window: WINDOW,
        }
    }

    /// Without the grouping waits, for tests: each pass claims everything
    /// pending, and groups what it claimed.
    pub fn without_waits(mut self) -> Self {
        self.wait = Duration::ZERO;
        self.window = Duration::ZERO;
        self
    }

    /// Without waits between tries, for tests.
    pub fn with_retries(mut self, retries: Vec<Duration>) -> Self {
        self.retries = retries;
        self
    }

    /// Without waits between messages to a chat channel, for tests.
    pub fn without_pace(mut self) -> Self {
        self.paced = false;
        self
    }

    /// Notifies failed backups reported in the updater's `dir`.
    pub fn with_status_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.status_dir = dir;
        self
    }

    /// Delivers the outbox every `poll` until `stop` resolves.
    pub async fn run(self, poll: Duration, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        let mut tick = interval(poll);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut passes = 0u64;
        loop {
            tokio::select! {
                _ = &mut stop => return,
                _ = tick.tick() => {}
            }
            if let Some(dir) = self
                .status_dir
                .as_ref()
                .filter(|_| passes.is_multiple_of(BACKUP_CHECK_PASSES))
            {
                if let Err(error) = backup::check(&self.pool, dir).await {
                    tracing::warn!(%error, "could not record a failed backup; will retry");
                }
            }
            if let Err(error) = self.pass().await {
                tracing::warn!(%error, "could not read the notification outbox; will retry");
            }
            passes += 1;
            // About hourly at the default poll.
            if passes % 720 == 1 {
                if let Err(error) = self.prune().await {
                    tracing::warn!(%error, "could not prune the notification outbox");
                }
            }
        }
    }

    /// Claims pending events that are due and that a silence does not
    /// hold, and writes them as sent. In one transaction, so a held event
    /// is never lost and nothing slips past a silence between looking and
    /// claiming. Also returns the events copied for one channel or one
    /// person ([`silence::requeue`]), by ID.
    ///
    /// A customer's events are due together (ADR-0080): when the oldest has
    /// waited [`WAIT`], or, if some were claimed within the last
    /// [`WINDOW`], once the window since that claim is over. What was
    /// already written at a claim is due with it, for a claim that did not
    /// take everything. Events of no customer, and copies for one channel
    /// or person, are due at once.
    async fn claim(&self) -> Result<(Vec<Event>, Targets), sqlx::Error> {
        let now = chrono::Utc::now();
        let mut tx = self.pool.begin().await?;
        let pending: Vec<Event> = sqlx::query_as(
            "WITH last AS (
                 SELECT customer_id, max(sent_at) AS at FROM notification_outbox
                 WHERE sent_at > now() - make_interval(secs => $2) AND held_at IS NULL
                   AND channel_id IS NULL AND user_id IS NULL AND customer_id IS NOT NULL
                 GROUP BY customer_id),
             first AS (
                 SELECT customer_id, min(created_at) AS at FROM notification_outbox
                 WHERE sent_at IS NULL AND held_at IS NULL
                   AND channel_id IS NULL AND user_id IS NULL AND customer_id IS NOT NULL
                 GROUP BY customer_id)
             SELECT o.id, o.event, o.customer_id, o.site_id, o.details, o.created_at
             FROM notification_outbox o
             LEFT JOIN last l ON l.customer_id = o.customer_id
             LEFT JOIN first f ON f.customer_id = o.customer_id
             WHERE o.sent_at IS NULL AND o.held_at IS NULL
               AND (o.customer_id IS NULL OR o.channel_id IS NOT NULL OR o.user_id IS NOT NULL
                    OR (l.at IS NOT NULL AND o.created_at <= l.at)
                    OR (l.at IS NULL AND f.at <= now() - make_interval(secs => $3)))
             ORDER BY o.id LIMIT $1 FOR UPDATE OF o SKIP LOCKED",
        )
        .bind(BATCH)
        .bind(self.window.as_secs_f64())
        .bind(self.wait.as_secs_f64())
        .fetch_all(&mut *tx)
        .await?;
        let Some(oldest) = pending.iter().map(|e| e.created_at).min() else {
            return Ok((Vec::new(), Targets::default()));
        };
        let ids: Vec<i64> = pending.iter().map(|e| e.id).collect();
        let copies: Vec<(i64, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, channel_id, user_id FROM notification_outbox
             WHERE id = ANY($1) AND (channel_id IS NOT NULL OR user_id IS NOT NULL)",
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        let mut targets = Targets::default();
        for (id, channel, user) in copies {
            if let Some(channel) = channel {
                targets.channels.insert(id, channel);
            }
            if let Some(user) = user {
                targets.people.insert(id, user);
            }
        }
        let silences = silence::load(&mut *tx, now, oldest).await?;
        let held = silence::held_incidents(&mut *tx).await?;
        let mut events = Vec::with_capacity(pending.len());
        for event in pending {
            // A copy for one channel or person was settled already.
            let covering = silences.iter().find(|s| s.holds(&event, now));
            if !targets.contains(event.id) && silence::waits(&event, covering, &held) {
                silence::hold(&mut tx, &event, covering.map(|s| s.id), now).await?;
            } else {
                events.push(event);
            }
        }
        let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
        sqlx::query("UPDATE notification_outbox SET sent_at = now() WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        // What is sent now starts or stops follow-ups (ADR-0082); copies for
        // one channel or person were told before.
        let told: Vec<Event> = events
            .iter()
            .filter(|e| !targets.contains(e.id))
            .cloned()
            .collect();
        follow_up::start(&mut tx, &told, now).await?;
        tx.commit().await?;
        Ok((events, targets))
    }

    /// Claims and delivers pending events; returns how many were claimed.
    /// What a silence released is sent by the next pass, once this one has
    /// also held what was pending, so a release sees all of an incident.
    /// A release fits in the narrowest chat channel's pass, personal ones
    /// included.
    pub async fn pass(&self) -> Result<usize, sqlx::Error> {
        let (events, targets) = self.claim().await?;
        let channels = channels(&self.pool).await?;
        let personal: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT kind FROM user_contacts WHERE kind IN ('slack', 'teams', 'discord')",
        )
        .fetch_all(&self.pool)
        .await?;
        let budget = channels
            .iter()
            .filter_map(|c| c.destination.chat())
            .map(|(service, _)| service)
            .chain(personal.iter().filter_map(|kind| Service::parse(kind)))
            .map(|service| Budget {
                messages: service.burst() - 1,
                lines: service.group_lines(),
            })
            .reduce(|a, b| Budget {
                messages: a.messages.min(b.messages),
                lines: a.lines.min(b.lines),
            });
        silence::release(&self.pool, chrono::Utc::now(), budget).await?;
        // Follow-up steps due now (ADR-0082), with a customer's other
        // events when it has some, or once its window is over.
        let customers: HashSet<Uuid> = events.iter().filter_map(|e| e.customer_id).collect();
        let due =
            follow_up::take_due(&self.pool, chrono::Utc::now(), self.window, &customers).await?;
        if events.is_empty() && due.is_empty() {
            return Ok(0);
        }
        let claimed = events.len() + due.len();
        let oldest = chrono::Utc::now() - MAX_AGE;
        let (mut fresh, stale): (Vec<_>, Vec<_>) =
            events.into_iter().partition(|e| e.created_at >= oldest);
        if !stale.is_empty() {
            tracing::warn!(
                events = stale.len(),
                "dropped notifications older than a day"
            );
        }
        if let Err(error) = count_behind_relays(&self.pool, &mut fresh).await {
            tracing::warn!(%error, "could not count the servers behind an offline relay");
        }
        // A single company never reads "customer" (ADR-0104).
        let several = crate::api::customers::several(&self.pool).await?;
        // The person on call is whoever it is now, as this pass sends:
        // every event of the pass goes to the same people.
        let now = chrono::Utc::now();
        let mut conn = self.pool.acquire().await?;
        let directory = on_call::Directory::load(&mut conn).await?;
        let schedules = on_call::schedules(&mut conn, now, None).await?;
        // Who each follow-up step tells, and who was told of what resolves
        // now, which they hear too.
        let mut extra = on_call::Extra {
            copies: targets.people.clone(),
            ..on_call::Extra::default()
        };
        let mut extra_channels: HashMap<i64, Vec<Uuid>> = HashMap::new();
        let mut steps = Vec::with_capacity(due.len());
        for step in &due {
            let event = &step.event;
            let told = follow_up::recipients(
                &step.step,
                &schedules,
                &directory,
                &channels,
                (event.customer_id, event.site_id),
                now,
            );
            extra.unrouted.insert(event.id);
            extra.users.insert(event.id, told.users.clone());
            extra_channels.insert(event.id, told.channels.clone());
            steps.push((event, told));
        }
        let resolved: HashMap<Uuid, i64> = fresh
            .iter()
            .filter(|e| e.event == "incident.resolved")
            .filter_map(|e| Some((incident_id(e)?, e.id)))
            .collect();
        if !resolved.is_empty() {
            let ids: Vec<Uuid> = resolved.keys().copied().collect();
            for (incident, told) in follow_up::told_before(&mut *conn, &ids).await? {
                let id = resolved[&incident];
                extra.users.insert(id, told.users);
                extra_channels.insert(id, told.channels);
            }
        }
        for (event, told) in &steps {
            let names = follow_up::names(told, &directory, &channels);
            follow_up::record_step(&mut conn, event, &names).await?;
        }
        fresh.extend(due.into_iter().map(|d| d.event));
        let people = on_call::deliveries(
            &schedules,
            &directory,
            &channels,
            &fresh,
            &targets.channels,
            &extra,
            now,
        );
        let mut groups = Groups::default();
        let mut links = Links::default();
        let mut told: HashMap<Uuid, follow_up::Told> = HashMap::new();
        let mut work = Vec::new();
        for channel in channels {
            let events: Vec<&Event> = fresh
                .iter()
                .filter(|e| match extra_channels.get(&e.id) {
                    Some(named) if named.contains(&channel.id) => true,
                    _ if extra.unrouted.contains(&e.id) || channel.follow_up_only => false,
                    _ => channel.coverage.includes(e.customer_id, e.site_id),
                })
                .filter(|e| !targets.people.contains_key(&e.id))
                .filter(|e| {
                    targets
                        .channels
                        .get(&e.id)
                        .is_none_or(|only| *only == channel.id)
                })
                .collect();
            if events.is_empty() {
                continue;
            }
            for event in events.iter().filter(|e| extra.unrouted.contains(&e.id)) {
                if let Some(incident) = incident_id(event) {
                    told.entry(incident).or_default().channels.push(channel.id);
                }
            }
            let messages = self.messages(&channel, &events, several, &mut groups);
            work.push((channel, messages));
        }
        // People on call get what a channel of their schedule's place gets:
        // the same groups, with the same IDs, or one request per event to a
        // webhook, and a link that acknowledges what they may.
        for (channel, ids) in people {
            let events: Vec<&Event> = fresh.iter().filter(|e| ids.contains(&e.id)).collect();
            for event in events.iter().filter(|e| problem(e)) {
                if let Some(incident) = incident_id(event) {
                    told.entry(incident).or_default().users.push(channel.id);
                }
            }
            let mut messages = self.messages(&channel, &events, several, &mut groups);
            if let Some(person) = directory.people.get(&channel.id) {
                for message in &mut messages {
                    let incidents = acknowledgeable(message, &events, person, &directory);
                    if !incidents.is_empty() {
                        let url = links.add(person.id, incidents, &self.web_url);
                        *message = with_acknowledge(std::mem::take(message), &url);
                    }
                }
            }
            work.push((channel, messages));
        }
        for (incident, told) in &told {
            follow_up::record_told(&mut conn, *incident, told, now).await?;
        }
        drop(conn);
        // Before sending, so that the links work; a failure costs the list
        // or the links, not the messages.
        if let Err(error) = groups.save(&self.pool).await {
            tracing::warn!(%error, "could not record notification groups");
        }
        if let Err(error) = links.save(&self.pool, now).await {
            tracing::warn!(%error, "could not record acknowledge links");
        }
        let mut tasks = JoinSet::new();
        for (channel, messages) in work {
            let this = self.clone();
            tasks.spawn(async move { this.deliver_all(&channel, &messages).await });
        }
        while tasks.join_next().await.is_some() {}
        Ok(claimed)
    }

    /// What `channel` is sent for `events`, the ones it covers. Webhooks
    /// get one request per event: their receivers may follow each incident
    /// by its ID (ADR-0080). Email and chat channels get groups, and name
    /// the customer only when there are `several` (ADR-0104); a webhook's
    /// payload is the same either way.
    fn messages(
        &self,
        channel: &Channel,
        events: &[&Event],
        several: bool,
        groups: &mut Groups,
    ) -> Vec<Message> {
        if let Destination::Webhook(_) = channel.destination {
            return events.iter().map(|e| render(e, &self.web_url)).collect();
        }
        let service = channel.destination.chat().map(|(service, _)| service);
        let unnamed: Vec<Event>;
        let events: Vec<&Event> = if several {
            events.to_vec()
        } else {
            unnamed = events.iter().map(|e| without_customer(e)).collect();
            unnamed.iter().collect()
        };
        group::group(&events)
            .into_iter()
            .flat_map(|outgoing| match outgoing {
                Outgoing::One(event) => vec![render(event, &self.web_url)],
                Outgoing::Group(group) => {
                    let id = groups.id(&group);
                    render_group(&group, id, &self.web_url, service)
                }
            })
            .collect()
    }

    /// Sends `messages` in order. After a delivery failed every try, the
    /// rest get one try each, so a dead channel does not hold the pass.
    /// Chat channels get their service's pace, a burst is gathered, and a
    /// service still limiting after every try gets nothing more this pass.
    /// What a silence released is owed (ADR-0079): it is never gathered,
    /// and a limit gives it to this channel again at the next pass.
    async fn deliver_all(&self, channel: &Channel, messages: &[Message]) {
        let service = channel.destination.chat().map(|(service, _)| service);
        let gathered = service.filter(|s| messages.len() > s.burst()).map(|s| {
            let (mut owed, rest): (Vec<Message>, Vec<Message>) = messages
                .iter()
                .cloned()
                .partition(|m| silence::owed(&m.payload));
            let keep = s.burst().saturating_sub(owed.len() + 1);
            owed.extend(gather(&rest, keep, &self.web_url));
            owed
        });
        let messages = gathered.as_deref().unwrap_or(messages);
        let mut last = Ok(());
        let mut sent = 0usize;
        for (index, message) in messages.iter().enumerate() {
            if let (Some(service), true) = (service, index > 0 && self.paced) {
                sleep(service.pace()).await;
            }
            let retries = if last.is_ok() { &self.retries[..] } else { &[] };
            last = self.deliver_with_retries(channel, message, retries).await;
            match &last {
                Ok(()) => sent += 1,
                Err(error) => {
                    tracing::warn!(channel = %channel.id, kind = channel.destination.kind(), %error, "notification not delivered");
                    if error.retry_after.is_some() {
                        self.requeue_owed(channel, &messages[index..]).await;
                        break;
                    }
                }
            }
        }
        if sent > 0 {
            tracing::info!(channel = %channel.id, sent, failed = messages.len() - sent, "notifications delivered");
        }
        let error = last.err().map(|e| e.message);
        if let Err(db) = record_outcome(&self.pool, channel, error.as_deref()).await {
            tracing::warn!(channel = %channel.id, error = %db, "could not record a notification outcome");
        }
    }

    /// Gives this channel the owed messages of `left` again at the next pass.
    async fn requeue_owed(&self, channel: &Channel, left: &[Message]) {
        let ids: Vec<i64> = left
            .iter()
            .filter(|m| silence::owed(&m.payload))
            .flat_map(|m| group::rows(&m.payload))
            .collect();
        if ids.is_empty() {
            return;
        }
        match silence::requeue(&self.pool, &ids, channel.id, channel.person).await {
            Ok(copied) => {
                tracing::info!(channel = %channel.id, copied, "kept what a silence held back for the next pass")
            }
            Err(error) => {
                tracing::warn!(channel = %channel.id, %error, "could not keep what a silence held back")
            }
        }
    }

    fn client(&self, channel: &Channel) -> &reqwest::Client {
        if channel.public_only {
            &self.public_http
        } else {
            &self.http
        }
    }

    async fn deliver_with_retries(
        &self,
        channel: &Channel,
        message: &Message,
        retries: &[Duration],
    ) -> Result<(), SendError> {
        let mut result = self.deliver(channel, message).await;
        for wait in retries {
            let asked = match &result {
                Ok(()) => break,
                Err(error) => error.retry_after,
            };
            sleep(asked.map_or(*wait, |asked| asked.max(*wait))).await;
            result = self.deliver(channel, message).await;
        }
        result
    }

    /// One try: a plugin channel by its plugin (ADR-0103), the others
    /// with the client for their [`Channel::public_only`].
    async fn deliver(&self, channel: &Channel, message: &Message) -> Result<(), SendError> {
        // Never without its password or signing secret (ADR-0110).
        if let Some(error) = &channel.secret_error {
            return Err(SendError::new(error.clone()));
        }
        match &channel.destination {
            Destination::Plugin(_) => {
                crate::plugins::channel::deliver(&self.pool, channel, message).await
            }
            _ => deliver(channel, message, self.client(channel)).await,
        }
    }

    /// Sends a test to `channel` once and records the outcome.
    pub async fn test(&self, channel: &Channel) -> Result<Result<(), SendError>, sqlx::Error> {
        let test = render(&Event::test(), &self.web_url);
        let result = self.deliver(channel, &test).await;
        let error = result.as_ref().err().map(|e| e.message.as_str());
        record_outcome(&self.pool, channel, error).await?;
        Ok(result)
    }

    /// Deletes events delivered more than a week ago.
    pub async fn prune(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM notification_outbox
             WHERE sent_at < now() - make_interval(days => $1)",
        )
        .bind(KEEP_DAYS)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}

/// The groups of a pass, one ID per set of members, so that channels
/// sent the same group link to the same list.
#[derive(Default)]
struct Groups {
    /// Member rows, and the group's ID, customer and incidents.
    seen: HashMap<Vec<i64>, (Uuid, Uuid, Vec<Uuid>)>,
}

impl Groups {
    fn id(&mut self, group: &Group) -> Uuid {
        let rows = group.members.iter().map(|e| e.id).collect();
        let customer = group.members[0].customer_id.unwrap_or_default();
        let incidents: BTreeSet<Uuid> = group
            .members
            .iter()
            .filter_map(|e| e.details["incident"]["id"].as_str()?.parse().ok())
            .collect();
        self.seen
            .entry(rows)
            .or_insert_with(|| (Uuid::new_v4(), customer, incidents.into_iter().collect()))
            .0
    }

    /// Records the groups for the interface's list (`?group=`).
    async fn save(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        if self.seen.is_empty() {
            return Ok(());
        }
        let mut tx = pool.begin().await?;
        for (id, customer, incidents) in self.seen.values() {
            sqlx::query(
                "INSERT INTO notification_groups (id, customer_id, incident_ids)
                 VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(customer)
            .bind(incidents)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
}

/// The links of a pass that acknowledge incidents from a personal message
/// (ADR-0082), each for one person and the incidents of one message.
#[derive(Default)]
struct Links {
    /// Hash of the secret, user, incidents.
    rows: Vec<(Vec<u8>, Uuid, Vec<Uuid>)>,
}

impl Links {
    /// A new link for `user` to acknowledge `incidents`, under `web_url`.
    fn add(&mut self, user: Uuid, incidents: Vec<Uuid>, web_url: &str) -> String {
        let token = OneTimeToken::generate();
        self.rows.push((token.hash(), user, incidents));
        token.url(web_url, ACKNOWLEDGE_PATH)
    }

    async fn save(
        &self,
        pool: &PgPool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), sqlx::Error> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let mut tx = pool.begin().await?;
        for (hash, user, incidents) in &self.rows {
            sqlx::query(
                "INSERT INTO acknowledge_links (token_hash, user_id, incident_ids, created_at,
                                                expires_at)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(hash)
            .bind(user)
            .bind(incidents)
            .bind(now)
            .bind(now + follow_up::LINK_LIFETIME)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM acknowledge_links WHERE expires_at < $1")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
}

fn incident_id(event: &Event) -> Option<Uuid> {
    event
        .event
        .starts_with("incident.")
        .then(|| event.details["incident"]["id"].as_str()?.parse().ok())
        .flatten()
}

/// Whether `event` tells of an incident still a problem: opened or worse,
/// or a follow-up step.
fn problem(event: &Event) -> bool {
    matches!(
        event.event.as_str(),
        "incident.opened" | "incident.escalated"
    )
}

/// The incidents `message` tells as problems that `person` may
/// acknowledge, from `events`, the ones it was rendered from.
fn acknowledgeable(
    message: &Message,
    events: &[&Event],
    person: &on_call::Person,
    directory: &on_call::Directory,
) -> Vec<Uuid> {
    let rows = group::rows(&message.payload);
    let mut incidents: Vec<Uuid> = events
        .iter()
        .filter(|e| rows.contains(&e.id) && problem(e))
        .filter(|e| {
            e.site_id
                .is_some_and(|site| directory.acknowledges(person, Coverage::Site(site)))
        })
        .filter_map(|e| incident_id(e))
        .collect();
    incidents.sort();
    incidents.dedup();
    incidents
}

/// `event` without its customer's name, for a single company (ADR-0104).
fn without_customer(event: &Event) -> Event {
    let mut event = event.clone();
    if let Some(customer) = event.details["customer"].as_object_mut() {
        customer.remove("name");
    }
    event
}

/// Adds to each relay's offline incident how many servers are behind it,
/// which say nothing of their own while it is down (ADR-0072), as
/// `relay.servers`.
async fn count_behind_relays(pool: &PgPool, events: &mut [Event]) -> Result<(), sqlx::Error> {
    let down =
        |e: &Event| e.event == "incident.opened" && e.details["incident"]["metric"] == "offline";
    let relays: Vec<Uuid> = events
        .iter()
        .filter(|e| down(e))
        .filter_map(|e| e.details["relay"]["id"].as_str()?.parse().ok())
        .collect();
    if relays.is_empty() {
        return Ok(());
    }
    let counts: HashMap<Uuid, i64> = sqlx::query_as(
        "SELECT relay_id, count(*) FROM servers
         WHERE relay_id = ANY($1) AND revoked_at IS NULL GROUP BY relay_id",
    )
    .bind(&relays)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    for event in events.iter_mut().filter(|e| down(e)) {
        let relay = event.details["relay"]["id"]
            .as_str()
            .and_then(|id| id.parse().ok());
        if let (Some(n), Some(object)) = (
            relay.and_then(|r: Uuid| counts.get(&r)),
            event.details["relay"].as_object_mut(),
        ) {
            object.insert("servers".into(), (*n).into());
        }
    }
    Ok(())
}
