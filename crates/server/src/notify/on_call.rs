//! On call (ADR-0081): who is on duty for everything, a customer or a site
//! gets each notification about it in person, besides the channels.
//!
//! A [`Rota`] hands off at a local time in its time zone, daily or weekly;
//! an [`Override`] puts someone else on for a period, the newest winning
//! where several overlap. The person on call is looked up once per pass of
//! the dispatcher, at the moment it sends. When that person cannot be told
//! (no contact, disabled, removed, or no longer sees the place) it is a gap,
//! and the nearest administrators of the place are told instead.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{
    DateTime, Datelike, Days, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeDelta,
    TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use super::message::Event;
use super::{Channel, Coverage, Destination, Email};

/// How often the rotation hands off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Rotation {
    Daily,
    Weekly,
}

impl Rotation {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }

    fn days(self) -> i64 {
        match self {
            Self::Daily => 1,
            Self::Weekly => 7,
        }
    }
}

/// The instant of a local date and time in `tz`. A time that happens twice
/// when clocks go back is the first; one skipped when they go forward is
/// read with the offset before the change, so 02:30 on a night that jumps
/// from 02:00 to 03:00 is 03:30 (RFC 5545, 3.3.5).
pub fn local_instant(tz: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => at.with_timezone(&Utc),
        LocalResult::None => {
            // Changes are months apart, so a day earlier has the offset
            // that was in force before this one.
            let before = tz
                .offset_from_utc_datetime(&(local - TimeDelta::days(1)))
                .fix();
            (local - TimeDelta::seconds(before.local_minus_utc().into())).and_utc()
        }
    }
}

/// The turns of a schedule: who takes over at each hand-off.
#[derive(Debug, Clone, PartialEq)]
pub struct Rota {
    pub time_zone: Tz,
    pub rotation: Rotation,
    /// Local time of each hand-off.
    pub handoff: NaiveTime,
    /// The local date of one hand-off; weekly ones fall on its weekday.
    pub anchor: NaiveDate,
    /// The member who took over at the anchor's hand-off.
    pub anchor_position: usize,
    /// In order; `None` is the turn of a removed user.
    pub members: Vec<Option<Uuid>>,
}

impl Rota {
    /// When turn `turn` starts, the anchor's being turn 0.
    pub fn handoff(&self, turn: i64) -> DateTime<Utc> {
        let days = turn * self.rotation.days();
        let date = if days >= 0 {
            self.anchor.checked_add_days(Days::new(days.unsigned_abs()))
        } else {
            self.anchor.checked_sub_days(Days::new(days.unsigned_abs()))
        };
        let date = date.unwrap_or(self.anchor);
        local_instant(self.time_zone, date.and_time(self.handoff))
    }

    /// The turn running at `at`: it started at or before `at`, and the next
    /// starts after it.
    pub fn turn_at(&self, at: DateTime<Utc>) -> i64 {
        let local = at.with_timezone(&self.time_zone).date_naive();
        let days = (local - self.anchor).num_days();
        let mut turn = days.div_euclid(self.rotation.days());
        while self.handoff(turn) > at {
            turn -= 1;
        }
        while self.handoff(turn + 1) <= at {
            turn += 1;
        }
        turn
    }

    /// The position in `members` of turn `turn`; `None` without members.
    pub fn position(&self, turn: i64) -> Option<usize> {
        let count = i64::try_from(self.members.len()).ok().filter(|n| *n > 0)?;
        let start = i64::try_from(self.anchor_position).unwrap_or(0);
        usize::try_from((start + turn).rem_euclid(count)).ok()
    }

    /// The anchor of the latest hand-off at or before `now`, with these
    /// settings: weekly ones on `weekday`.
    pub fn last_handoff_date(
        time_zone: Tz,
        rotation: Rotation,
        handoff: NaiveTime,
        weekday: Weekday,
        now: DateTime<Utc>,
    ) -> NaiveDate {
        let mut date = now.with_timezone(&time_zone).date_naive();
        if rotation == Rotation::Weekly {
            let back =
                (7 + date.weekday().num_days_from_monday() - weekday.num_days_from_monday()) % 7;
            date -= TimeDelta::days(back.into());
        }
        if local_instant(time_zone, date.and_time(handoff)) > now {
            date -= TimeDelta::days(rotation.days());
        }
        date
    }
}

/// Someone on call instead of the rotation for a while.
#[derive(Debug, Clone, PartialEq)]
pub struct Override {
    pub id: Uuid,
    pub user: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl Override {
    fn covers(&self, at: DateTime<Utc>) -> bool {
        self.starts_at <= at && at < self.ends_at
    }
}

/// What put someone on call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The turn of the member at this position.
    Rotation(usize),
    Override(Uuid),
    /// No members at all.
    Nobody,
}

/// One period with the same person on call for the same reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// `None` for a removed user or a schedule without members.
    pub user: Option<Uuid>,
    pub source: Source,
}

/// A schedule as the dispatcher and the API use it.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub id: Uuid,
    pub coverage: Coverage,
    pub rota: Rota,
    pub overrides: Vec<Override>,
}

impl Schedule {
    /// Who is on call at `at`, and why. The newest override covering `at`
    /// wins; the rotation otherwise.
    pub fn on_call(&self, at: DateTime<Utc>) -> (Option<Uuid>, Source) {
        let newest = self
            .overrides
            .iter()
            .filter(|o| o.covers(at))
            .max_by_key(|o| (o.created_at, o.id));
        if let Some(o) = newest {
            return (Some(o.user), Source::Override(o.id));
        }
        match self.rota.position(self.rota.turn_at(at)) {
            Some(position) => (self.rota.members[position], Source::Rotation(position)),
            None => (None, Source::Nobody),
        }
    }

    /// Who is on call from `from` to `to`, in order, without two neighbours
    /// alike.
    pub fn spans(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span> {
        let mut cuts = vec![from, to];
        if !self.rota.members.is_empty() {
            let mut turn = self.rota.turn_at(from) + 1;
            loop {
                let at = self.rota.handoff(turn);
                if at >= to {
                    break;
                }
                cuts.push(at);
                turn += 1;
            }
        }
        for o in &self.overrides {
            cuts.extend(
                [o.starts_at, o.ends_at]
                    .into_iter()
                    .filter(|t| from < *t && *t < to),
            );
        }
        cuts.sort();
        cuts.dedup();
        let mut spans: Vec<Span> = Vec::new();
        for pair in cuts.windows(2) {
            let (user, source) = self.on_call(pair[0]);
            match spans.last_mut() {
                Some(last) if last.user == user && last.source == source => {
                    last.ends_at = pair[1];
                }
                _ => spans.push(Span {
                    starts_at: pair[0],
                    ends_at: pair[1],
                    user,
                    source,
                }),
            }
        }
        spans
    }
}

/// Why the person on call cannot be told: a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// Nobody: a schedule without members.
    Nobody,
    /// A removed user's turn.
    Removed,
    Disabled,
    /// No longer has a role that sees the schedule's place.
    NoAccess,
    /// Neither an email address nor a chat or webhook destination.
    NoContact,
    /// Only an email address, and no email channel to send it through.
    NoMailServer,
}

/// A user as on-call delivery needs them.
#[derive(Debug, Clone)]
pub struct Person {
    pub id: Uuid,
    pub username: String,
    pub disabled: bool,
    /// `(administrator, customer_id, site_id)` per grant.
    pub grants: Vec<(bool, Option<Uuid>, Option<Uuid>)>,
    /// `(customer_id, site_id)` of the grants of an operator or above,
    /// who may acknowledge incidents there (ADR-0082).
    pub operates: Vec<(Option<Uuid>, Option<Uuid>)>,
    pub email: Option<String>,
    /// Their chat or webhook destination, its secret or why it does not
    /// open (ADR-0125), and whether only public addresses may be reached.
    pub chat: Option<(Destination, Option<crate::secrets::Opened>, bool)>,
}

/// Every user, and which customer each site belongs to.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub people: HashMap<Uuid, Person>,
    pub site_customer: HashMap<Uuid, Uuid>,
}

impl Directory {
    pub async fn load(db: &mut sqlx::PgConnection) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.disabled_at IS NOT NULL AS disabled, c.email, c.kind,
                    c.settings, c.secret, coalesce(c.public_only, true) AS public_only
             FROM users u LEFT JOIN user_contacts c ON c.user_id = u.id",
        )
        .fetch_all(&mut *db)
        .await?;
        let mut people = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let chat = match row.try_get::<Option<String>, _>("kind")? {
                Some(kind) => {
                    let settings = row.try_get("settings")?;
                    match Destination::from_row(&kind, settings) {
                        Ok(destination) => Some((
                            destination,
                            crate::secrets::open_stored(
                                crate::secrets::Column::UserContacts,
                                row.try_get("secret")?,
                            ),
                            row.try_get("public_only")?,
                        )),
                        Err(error) => {
                            tracing::warn!(user = %id, %error, "ignoring an unreadable contact");
                            None
                        }
                    }
                }
                None => None,
            };
            people.insert(
                id,
                Person {
                    id,
                    username: row.try_get("username")?,
                    disabled: row.try_get("disabled")?,
                    grants: Vec::new(),
                    operates: Vec::new(),
                    email: row.try_get("email")?,
                    chat,
                },
            );
        }
        let grants: Vec<(Uuid, String, Option<Uuid>, Option<Uuid>)> =
            sqlx::query_as("SELECT user_id, role, customer_id, site_id FROM user_roles")
                .fetch_all(&mut *db)
                .await?;
        for (user, role, customer, site) in grants {
            if let Some(person) = people.get_mut(&user) {
                person
                    .grants
                    .push((role == "administrator", customer, site));
                if role != "viewer" {
                    person.operates.push((customer, site));
                }
            }
        }
        let site_customer = sqlx::query_as("SELECT id, customer_id FROM sites")
            .fetch_all(&mut *db)
            .await?
            .into_iter()
            .collect();
        Ok(Self {
            people,
            site_customer,
        })
    }

    /// Whether `outer` takes every event `inner` takes.
    pub fn contains(&self, outer: Coverage, inner: Coverage) -> bool {
        match (outer, inner) {
            (Coverage::Everything, _) => true,
            (Coverage::Customer(c), Coverage::Customer(d)) => c == d,
            (Coverage::Customer(c), Coverage::Site(s)) => self.site_customer.get(&s) == Some(&c),
            (Coverage::Site(s), Coverage::Site(t)) => s == t,
            _ => false,
        }
    }

    /// Whether a grant on `(customer, site)` reaches all of `coverage`.
    fn grant_reaches(
        &self,
        customer: Option<Uuid>,
        site: Option<Uuid>,
        coverage: Coverage,
    ) -> bool {
        Coverage::from_columns(customer, site).is_some_and(|grant| self.contains(grant, coverage))
    }

    /// Whether `person` sees all of `coverage`, through any role.
    pub fn sees(&self, person: &Person, coverage: Coverage) -> bool {
        person
            .grants
            .iter()
            .any(|(_, customer, site)| self.grant_reaches(*customer, *site, coverage))
    }

    /// Whether `person` may acknowledge incidents of `coverage`: an operator
    /// or above of all of it, enabled.
    pub fn acknowledges(&self, person: &Person, coverage: Coverage) -> bool {
        !person.disabled
            && person
                .operates
                .iter()
                .any(|(customer, site)| self.grant_reaches(*customer, *site, coverage))
    }

    /// Why `user` cannot be told about `coverage`, if they cannot. `mail`
    /// says whether an email channel can send for it.
    pub fn problem(&self, user: Option<Uuid>, coverage: Coverage, mail: bool) -> Option<Problem> {
        let Some(person) = user.and_then(|id| self.people.get(&id)) else {
            return Some(Problem::Removed);
        };
        if person.disabled {
            Some(Problem::Disabled)
        } else if !self.sees(person, coverage) {
            Some(Problem::NoAccess)
        } else if person.email.is_none() && person.chat.is_none() {
            Some(Problem::NoContact)
        } else if person.chat.is_none() && !mail {
            Some(Problem::NoMailServer)
        } else {
            None
        }
    }

    /// Who is told during a gap of a schedule for `coverage`: the
    /// administrators of the place itself who can be told, else those of
    /// its customer, else the global ones. Empty when none can be: the
    /// channels still are.
    pub fn fallback(&self, coverage: Coverage, mail: bool) -> Vec<Uuid> {
        let customer = match coverage {
            Coverage::Site(site) => self.site_customer.get(&site).copied(),
            Coverage::Customer(customer) => Some(customer),
            Coverage::Everything => None,
        };
        let mut levels = Vec::new();
        if let Coverage::Site(site) = coverage {
            levels.push((None, Some(site)));
        }
        if let Some(customer) = customer {
            levels.push((Some(customer), None));
        }
        levels.push((None, None));
        for level in levels {
            let mut found: Vec<Uuid> = self
                .people
                .values()
                .filter(|p| p.grants.contains(&(true, level.0, level.1)))
                .filter(|p| self.problem(Some(p.id), coverage, mail).is_none())
                .map(|p| p.id)
                .collect();
            if !found.is_empty() {
                found.sort_by(|a, b| self.people[a].username.cmp(&self.people[b].username));
                return found;
            }
        }
        Vec::new()
    }
}

/// How narrow a coverage is: a site before a customer before everything.
pub fn narrowness(coverage: Coverage) -> u8 {
    match coverage {
        Coverage::Everything => 0,
        Coverage::Customer(_) => 1,
        Coverage::Site(_) => 2,
    }
}

/// The email channel whose mail server sends personal email about
/// `coverage`: the narrowest that covers all of it, the oldest of equals.
/// Never one narrower, so a customer's mail server carries nothing about
/// another customer.
pub fn mail_channel<'a>(
    channels: &'a [Channel],
    directory: &Directory,
    coverage: Coverage,
) -> Option<&'a Channel> {
    channels
        .iter()
        .filter(|c| matches!(c.destination, Destination::Email(_)))
        .filter(|c| directory.contains(c.coverage, coverage))
        .rev()
        .max_by_key(|c| narrowness(c.coverage))
}

/// `(schedule, id, user, starts_at, ends_at, created_at)`.
type OverrideRow = (
    Uuid,
    Uuid,
    Uuid,
    DateTime<Utc>,
    DateTime<Utc>,
    DateTime<Utc>,
);

/// Every schedule, or only `only`, with the overrides that end after
/// `from`.
pub async fn schedules(
    db: &mut sqlx::PgConnection,
    from: DateTime<Utc>,
    only: Option<Uuid>,
) -> Result<Vec<Schedule>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, customer_id, site_id, time_zone, rotation, handoff_time, anchor_date,
                anchor_position
         FROM on_call_schedules WHERE $1::uuid IS NULL OR id = $1 ORDER BY created_at, id",
    )
    .bind(only)
    .fetch_all(&mut *db)
    .await?;
    let mut members: HashMap<Uuid, Vec<Option<Uuid>>> = HashMap::new();
    let rows_m: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT schedule_id, user_id FROM on_call_members ORDER BY schedule_id, position",
    )
    .fetch_all(&mut *db)
    .await?;
    for (schedule, user) in rows_m {
        members.entry(schedule).or_default().push(user);
    }
    let mut overrides: HashMap<Uuid, Vec<Override>> = HashMap::new();
    let rows_o: Vec<OverrideRow> = sqlx::query_as(
        "SELECT schedule_id, id, user_id, starts_at, ends_at, created_at
             FROM on_call_overrides WHERE ends_at > $1 ORDER BY starts_at, id",
    )
    .bind(from)
    .fetch_all(&mut *db)
    .await?;
    for (schedule, id, user, starts_at, ends_at, created_at) in rows_o {
        overrides.entry(schedule).or_default().push(Override {
            id,
            user,
            starts_at,
            ends_at,
            created_at,
        });
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let Some(coverage) =
            Coverage::from_columns(row.try_get("customer_id")?, row.try_get("site_id")?)
        else {
            continue;
        };
        let zone: String = row.try_get("time_zone")?;
        let time_zone = zone.parse::<Tz>().unwrap_or_else(|_| {
            tracing::warn!(schedule = %id, zone, "unknown time zone; using UTC");
            Tz::UTC
        });
        let rotation: String = row.try_get("rotation")?;
        let position: i32 = row.try_get("anchor_position")?;
        out.push(Schedule {
            id,
            coverage,
            rota: Rota {
                time_zone,
                rotation: Rotation::parse(&rotation).unwrap_or(Rotation::Weekly),
                handoff: row.try_get("handoff_time")?,
                anchor: row.try_get("anchor_date")?,
                anchor_position: usize::try_from(position).unwrap_or(0),
                members: members.remove(&id).unwrap_or_default(),
            },
            overrides: overrides.remove(&id).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Who gets a schedule's messages at `now`: the person on call, or during
/// a gap its [`Directory::fallback`].
pub fn recipients(
    schedule: &Schedule,
    directory: &Directory,
    mail: bool,
    now: DateTime<Utc>,
) -> Vec<Uuid> {
    let (user, _) = schedule.on_call(now);
    match directory.problem(user, schedule.coverage, mail) {
        None => user.into_iter().collect(),
        Some(_) => directory.fallback(schedule.coverage, mail),
    }
}

/// The destinations that reach `person` in person: email through `mail`'s
/// server, and their chat or webhook. Each is a [`Channel`] whose `id` is
/// the user's and `person` set.
pub fn contact_channels(person: &Person, mail: Option<&Channel>) -> Vec<Channel> {
    let mut out = Vec::new();
    if let (Some(address), Some(mail)) = (&person.email, mail) {
        if let Destination::Email(server) = &mail.destination {
            out.push(Channel {
                id: person.id,
                destination: Destination::Email(Email {
                    to: vec![address.clone()],
                    ..server.clone()
                }),
                secret: mail.secret.clone(),
                secret_error: mail.secret_error.clone(),
                coverage: Coverage::Everything,
                public_only: mail.public_only,
                person: true,
                follow_up_only: false,
            });
        }
    }
    if let Some((destination, secret, public_only)) = &person.chat {
        let (secret, secret_error) = crate::notify::split(secret.clone());
        out.push(Channel {
            id: person.id,
            destination: destination.clone(),
            secret,
            secret_error,
            coverage: Coverage::Everything,
            public_only: *public_only,
            person: true,
            follow_up_only: false,
        });
    }
    out
}

/// People told of the events of a pass besides the schedules that cover
/// them.
#[derive(Debug, Default)]
pub struct Extra {
    /// Events copied for one person, which go to that person's chat or
    /// webhook alone.
    pub copies: HashMap<i64, Uuid>,
    /// More people per event: a follow-up step's, or those told of an
    /// incident before its resolution (ADR-0082). Those who cannot be told
    /// about the event's place are skipped.
    pub users: HashMap<i64, Vec<Uuid>>,
    /// Events no schedule hears of: follow-up steps, told only to whom the
    /// step names.
    pub unrouted: HashSet<i64>,
}

/// Personal deliveries of one pass: each destination with the IDs of the
/// events it takes, in order. `targets` are the events copied for one
/// channel; `extra` adds people to the schedules'.
pub fn deliveries(
    schedules: &[Schedule],
    directory: &Directory,
    channels: &[Channel],
    events: &[Event],
    targets: &HashMap<i64, Uuid>,
    extra: &Extra,
    now: DateTime<Utc>,
) -> Vec<(Channel, Vec<i64>)> {
    let found: Vec<(&Schedule, Option<&Channel>, Vec<Uuid>)> = schedules
        .iter()
        .map(|s| {
            let mail = mail_channel(channels, directory, s.coverage);
            (s, mail, recipients(s, directory, mail.is_some(), now))
        })
        .collect();
    // (user, mail channel or none for their chat) -> events
    let mut out: BTreeMap<(Uuid, Option<Uuid>), Vec<i64>> = BTreeMap::new();
    for event in events {
        if targets.contains_key(&event.id) {
            continue;
        }
        if let Some(user) = extra.copies.get(&event.id) {
            out.entry((*user, None)).or_default().push(event.id);
            continue;
        }
        // Per user, the narrowest schedule that names them chooses the
        // mail server.
        let mut told: HashMap<Uuid, (u8, Option<Uuid>)> = HashMap::new();
        if let Some(users) = extra.users.get(&event.id) {
            // The event's own place chooses: the narrowest of all.
            let place = super::follow_up::place(event.customer_id, event.site_id);
            let mail = mail_channel(channels, directory, place);
            for user in users {
                if directory
                    .problem(Some(*user), place, mail.is_some())
                    .is_none()
                {
                    told.insert(*user, (u8::MAX, mail.map(|m| m.id)));
                }
            }
        }
        for (schedule, mail, users) in &found {
            if extra.unrouted.contains(&event.id)
                || !schedule.coverage.includes(event.customer_id, event.site_id)
            {
                continue;
            }
            let rank = narrowness(schedule.coverage);
            for user in users {
                let entry = told.entry(*user).or_insert((rank, mail.map(|m| m.id)));
                if rank > entry.0 {
                    *entry = (rank, mail.map(|m| m.id));
                }
            }
        }
        for (user, (_, mail)) in told {
            let Some(person) = directory.people.get(&user) else {
                continue;
            };
            if person.email.is_some() {
                if let Some(mail) = mail {
                    out.entry((user, Some(mail))).or_default().push(event.id);
                }
            }
            if person.chat.is_some() {
                out.entry((user, None)).or_default().push(event.id);
            }
        }
    }
    out.into_iter()
        .filter_map(|((user, mail), ids)| {
            let person = directory.people.get(&user)?;
            let channel = match mail {
                Some(mail) => {
                    let mail = channels.iter().find(|c| c.id == mail)?;
                    contact_channels(person, Some(mail)).into_iter().next()?
                }
                None => contact_channels(person, None).into_iter().next()?,
            };
            Some((channel, ids))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Webhook;
    use chrono_tz::{America::New_York, Europe::Paris};
    use serde_json::json;

    const ALICE: Uuid = Uuid::from_u128(1);
    const BOB: Uuid = Uuid::from_u128(2);
    const CAROL: Uuid = Uuid::from_u128(3);
    const CUSTOMER: Uuid = Uuid::from_u128(10);
    const OTHER: Uuid = Uuid::from_u128(11);
    const SITE: Uuid = Uuid::from_u128(20);
    const SIBLING: Uuid = Uuid::from_u128(21);

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn rota(tz: Tz, rotation: Rotation, handoff: NaiveTime, anchor: &str) -> Rota {
        Rota {
            time_zone: tz,
            rotation,
            handoff,
            anchor: date(anchor),
            anchor_position: 0,
            members: vec![Some(ALICE), Some(BOB), Some(CAROL)],
        }
    }

    fn schedule(rota: Rota) -> Schedule {
        Schedule {
            id: Uuid::from_u128(99),
            coverage: Coverage::Customer(CUSTOMER),
            rota,
            overrides: Vec::new(),
        }
    }

    fn cover(id: u128, user: Uuid, from: &str, to: &str, created: &str) -> Override {
        Override {
            id: Uuid::from_u128(id),
            user,
            starts_at: utc(from),
            ends_at: utc(to),
            created_at: utc(created),
        }
    }

    #[test]
    fn a_weekly_rotation_hands_off_at_the_local_time() {
        // 2026-11-02 is a Monday; Paris is UTC+1 in winter.
        let s = schedule(rota(Paris, Rotation::Weekly, time(9, 0), "2026-11-02"));
        assert_eq!(s.on_call(utc("2026-11-02T08:00:00Z")).0, Some(ALICE));
        assert_eq!(s.on_call(utc("2026-11-09T07:59:59Z")).0, Some(ALICE));
        assert_eq!(s.on_call(utc("2026-11-09T08:00:00Z")).0, Some(BOB));
        assert_eq!(s.on_call(utc("2026-11-16T08:00:00Z")).0, Some(CAROL));
        assert_eq!(s.on_call(utc("2026-11-23T08:00:00Z")).0, Some(ALICE));
        // Before the anchor, the turns run backwards.
        assert_eq!(s.on_call(utc("2026-11-02T07:59:59Z")).0, Some(CAROL));
        assert_eq!(s.on_call(utc("2026-10-20T12:00:00Z")).0, Some(BOB));
    }

    #[test]
    fn hand_offs_keep_the_local_time_across_daylight_saving() {
        // Paris goes from +2 to +1 on 2026-10-25 and from +1 to +2 on
        // 2027-03-28.
        let s = schedule(rota(Paris, Rotation::Weekly, time(9, 0), "2026-10-19"));
        assert_eq!(s.rota.handoff(0), utc("2026-10-19T07:00:00Z"));
        assert_eq!(s.rota.handoff(1), utc("2026-10-26T08:00:00Z"));
        let spring = s.rota.turn_at(utc("2027-03-29T07:00:00Z"));
        assert_eq!(s.rota.handoff(spring), utc("2027-03-29T07:00:00Z"));
        assert_eq!(s.rota.handoff(spring - 1), utc("2027-03-22T08:00:00Z"));
        // The week across the change lasts an hour more, and its turn is
        // still one person's.
        let span = s.spans(utc("2026-10-20T00:00:00Z"), utc("2026-10-27T00:00:00Z"));
        assert_eq!(span.len(), 2);
        assert_eq!(
            span[0].ends_at - utc("2026-10-19T07:00:00Z"),
            TimeDelta::hours(169)
        );
    }

    #[test]
    fn a_skipped_or_repeated_hand_off_time_has_one_answer() {
        // 02:30 does not exist in Paris on 2027-03-28, and happens twice on
        // 2026-10-25.
        let s = schedule(rota(Paris, Rotation::Daily, time(2, 30), "2027-03-27"));
        assert_eq!(s.rota.handoff(0), utc("2027-03-27T01:30:00Z"));
        assert_eq!(s.rota.handoff(1), utc("2027-03-28T01:30:00Z")); // 03:30 summer time
        assert_eq!(s.rota.handoff(2), utc("2027-03-29T00:30:00Z"));
        assert_eq!(s.on_call(utc("2027-03-28T01:29:59Z")).0, Some(ALICE));
        assert_eq!(s.on_call(utc("2027-03-28T01:30:00Z")).0, Some(BOB));
        let s = schedule(rota(Paris, Rotation::Daily, time(2, 30), "2026-10-24"));
        assert_eq!(s.rota.handoff(1), utc("2026-10-25T00:30:00Z")); // the first 02:30
        assert_eq!(s.on_call(utc("2026-10-25T01:30:00Z")).0, Some(BOB));
        // New York changes on other dates, at 02:00 local.
        let s = schedule(rota(New_York, Rotation::Daily, time(2, 0), "2027-03-13"));
        assert_eq!(s.rota.handoff(1), utc("2027-03-14T07:00:00Z"));
        assert_eq!(s.rota.handoff(2), utc("2027-03-15T06:00:00Z"));
    }

    #[test]
    fn every_instant_has_exactly_one_turn() {
        let s = schedule(rota(Paris, Rotation::Daily, time(2, 30), "2026-10-01"));
        let mut at = utc("2026-10-20T00:00:00Z");
        while at < utc("2027-04-10T00:00:00Z") {
            let turn = s.rota.turn_at(at);
            assert!(
                s.rota.handoff(turn) <= at && at < s.rota.handoff(turn + 1),
                "{at}"
            );
            at += TimeDelta::minutes(37);
        }
    }

    #[test]
    fn the_hand_off_instant_belongs_to_the_next_person() {
        let s = schedule(rota(Tz::UTC, Rotation::Daily, time(9, 0), "2026-11-02"));
        let handoff = utc("2026-11-03T09:00:00Z");
        assert_eq!(
            s.on_call(handoff - TimeDelta::milliseconds(1)).0,
            Some(ALICE)
        );
        assert_eq!(s.on_call(handoff).0, Some(BOB));
    }

    #[test]
    fn the_newest_override_wins_where_they_overlap() {
        let mut s = schedule(rota(Tz::UTC, Rotation::Weekly, time(9, 0), "2026-11-02"));
        // Bob covers Alice from Friday to Monday; later, Carol takes the
        // Saturday of it.
        s.overrides = vec![
            cover(
                1,
                CAROL,
                "2026-11-07T00:00:00Z",
                "2026-11-08T00:00:00Z",
                "2026-11-03T00:00:00Z",
            ),
            cover(
                2,
                BOB,
                "2026-11-06T09:00:00Z",
                "2026-11-09T09:00:00Z",
                "2026-11-02T00:00:00Z",
            ),
        ];
        assert_eq!(s.on_call(utc("2026-11-06T08:59:59Z")).0, Some(ALICE));
        assert_eq!(
            s.on_call(utc("2026-11-06T09:00:00Z")),
            (Some(BOB), Source::Override(Uuid::from_u128(2)))
        );
        assert_eq!(s.on_call(utc("2026-11-07T12:00:00Z")).0, Some(CAROL));
        assert_eq!(s.on_call(utc("2026-11-08T12:00:00Z")).0, Some(BOB));
        // Bob's own turn starts as his override ends.
        assert_eq!(
            s.on_call(utc("2026-11-09T09:00:00Z")),
            (Some(BOB), Source::Rotation(1))
        );
        let spans = s.spans(utc("2026-11-05T00:00:00Z"), utc("2026-11-10T00:00:00Z"));
        let who: Vec<_> = spans.iter().map(|s| (s.user, s.starts_at)).collect();
        assert_eq!(
            who,
            vec![
                (Some(ALICE), utc("2026-11-05T00:00:00Z")),
                (Some(BOB), utc("2026-11-06T09:00:00Z")),
                (Some(CAROL), utc("2026-11-07T00:00:00Z")),
                (Some(BOB), utc("2026-11-08T00:00:00Z")),
                (Some(BOB), utc("2026-11-09T09:00:00Z")),
            ]
        );
        // Equal creation times are settled by ID, never by list order.
        let a = cover(
            5,
            ALICE,
            "2026-11-04T00:00:00Z",
            "2026-11-05T00:00:00Z",
            "2026-11-01T00:00:00Z",
        );
        let c = cover(
            6,
            CAROL,
            "2026-11-04T00:00:00Z",
            "2026-11-05T00:00:00Z",
            "2026-11-01T00:00:00Z",
        );
        for order in [vec![a.clone(), c.clone()], vec![c, a]] {
            s.overrides = order;
            assert_eq!(s.on_call(utc("2026-11-04T12:00:00Z")).0, Some(CAROL));
        }
    }

    #[test]
    fn empty_turns_and_empty_schedules_are_spans_of_nobody() {
        let mut r = rota(Tz::UTC, Rotation::Daily, time(9, 0), "2026-11-02");
        r.members[1] = None;
        let s = schedule(r.clone());
        assert_eq!(
            s.on_call(utc("2026-11-03T10:00:00Z")),
            (None, Source::Rotation(1))
        );
        r.members.clear();
        let s = schedule(r);
        assert_eq!(
            s.on_call(utc("2026-11-03T10:00:00Z")),
            (None, Source::Nobody)
        );
        let spans = s.spans(utc("2026-11-03T00:00:00Z"), utc("2026-11-10T00:00:00Z"));
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn the_last_hand_off_is_the_anchor_of_a_new_rotation() {
        // Monday 2026-11-02 at 10:00 in Paris.
        let now = utc("2026-11-02T09:00:00Z");
        let weekly =
            |weekday, h| Rota::last_handoff_date(Paris, Rotation::Weekly, time(h, 0), weekday, now);
        assert_eq!(weekly(Weekday::Mon, 9), date("2026-11-02"));
        assert_eq!(weekly(Weekday::Mon, 11), date("2026-10-26"));
        assert_eq!(weekly(Weekday::Fri, 9), date("2026-10-30"));
        let daily =
            |h| Rota::last_handoff_date(Paris, Rotation::Daily, time(h, 0), Weekday::Mon, now);
        assert_eq!(daily(9), date("2026-11-02"));
        assert_eq!(daily(11), date("2026-11-01"));
    }

    fn person(id: Uuid, grants: Vec<(bool, Option<Uuid>, Option<Uuid>)>, email: bool) -> Person {
        Person {
            id,
            username: format!("u{}", id.as_u128()),
            disabled: false,
            operates: grants.iter().map(|(_, c, s)| (*c, *s)).collect(),
            grants,
            email: email.then(|| format!("u{}@x.io", id.as_u128())),
            chat: None,
        }
    }

    fn directory(people: Vec<Person>) -> Directory {
        Directory {
            people: people.into_iter().map(|p| (p.id, p)).collect(),
            site_customer: [(SITE, CUSTOMER), (SIBLING, CUSTOMER)].into(),
        }
    }

    #[test]
    fn someone_who_cannot_be_told_is_a_gap() {
        let mut d = directory(vec![
            person(ALICE, vec![(false, Some(CUSTOMER), None)], true),
            person(BOB, vec![(false, None, Some(SITE))], true),
            person(CAROL, vec![(false, Some(CUSTOMER), None)], false),
        ]);
        let customer = Coverage::Customer(CUSTOMER);
        assert_eq!(d.problem(Some(ALICE), customer, true), None);
        assert_eq!(
            d.problem(Some(ALICE), customer, false),
            Some(Problem::NoMailServer)
        );
        assert_eq!(
            d.problem(Some(BOB), customer, true),
            Some(Problem::NoAccess)
        );
        assert_eq!(d.problem(Some(BOB), Coverage::Site(SITE), true), None);
        assert_eq!(
            d.problem(Some(BOB), Coverage::Site(SIBLING), true),
            Some(Problem::NoAccess)
        );
        assert_eq!(
            d.problem(Some(CAROL), customer, true),
            Some(Problem::NoContact)
        );
        assert_eq!(d.problem(None, customer, true), Some(Problem::Removed));
        assert_eq!(
            d.problem(Some(Uuid::from_u128(77)), customer, true),
            Some(Problem::Removed)
        );
        d.people.get_mut(&ALICE).unwrap().disabled = true;
        assert_eq!(
            d.problem(Some(ALICE), customer, true),
            Some(Problem::Disabled)
        );
        // A chat destination needs no mail server.
        let carol = d.people.get_mut(&CAROL).unwrap();
        carol.chat = Some((
            Destination::Webhook(Webhook {
                url: "https://h/".into(),
            }),
            None,
            true,
        ));
        assert_eq!(d.problem(Some(CAROL), customer, false), None);
    }

    #[test]
    fn a_gap_goes_to_the_nearest_administrators() {
        let site_admin = Uuid::from_u128(40);
        let customer_admin = Uuid::from_u128(41);
        let global_admin = Uuid::from_u128(42);
        let other_admin = Uuid::from_u128(43);
        let mut d = directory(vec![
            person(site_admin, vec![(true, None, Some(SITE))], true),
            person(customer_admin, vec![(true, Some(CUSTOMER), None)], true),
            person(global_admin, vec![(true, None, None)], true),
            person(other_admin, vec![(true, Some(OTHER), None)], true),
            person(ALICE, vec![(false, Some(CUSTOMER), None)], true),
        ]);
        assert_eq!(d.fallback(Coverage::Site(SITE), true), vec![site_admin]);
        assert_eq!(
            d.fallback(Coverage::Site(SIBLING), true),
            vec![customer_admin]
        );
        assert_eq!(
            d.fallback(Coverage::Customer(CUSTOMER), true),
            vec![customer_admin]
        );
        assert_eq!(d.fallback(Coverage::Everything, true), vec![global_admin]);
        // Administrators who cannot be told are skipped, level by level.
        d.people.get_mut(&site_admin).unwrap().email = None;
        d.people.get_mut(&customer_admin).unwrap().disabled = true;
        assert_eq!(d.fallback(Coverage::Site(SITE), true), vec![global_admin]);
        assert_eq!(d.fallback(Coverage::Site(SITE), false), Vec::<Uuid>::new());
        // The schedule's recipients: the person on call, else the fallback.
        let mut s = schedule(rota(Tz::UTC, Rotation::Daily, time(9, 0), "2026-11-02"));
        s.rota.members = vec![Some(ALICE), Some(BOB)];
        let now = utc("2026-11-02T10:00:00Z");
        assert_eq!(recipients(&s, &d, true, now), vec![ALICE]);
        assert_eq!(
            recipients(&s, &d, true, now + TimeDelta::days(1)),
            vec![global_admin]
        );
    }

    fn email_channel(id: u128, coverage: Coverage, host: &str) -> Channel {
        Channel {
            id: Uuid::from_u128(id),
            destination: Destination::Email(Email {
                host: host.into(),
                port: 587,
                username: "mf".into(),
                from: "mf@x.io".into(),
                to: vec!["team@x.io".into()],
            }),
            secret: Some(monitorflow_common::redact::Secret::new("pw")),
            secret_error: None,
            coverage,
            public_only: false,
            person: false,
            follow_up_only: false,
        }
    }

    #[test]
    fn mail_goes_through_the_narrowest_server_that_covers_the_schedule() {
        let d = directory(Vec::new());
        let channels = vec![
            email_channel(1, Coverage::Everything, "msp"),
            email_channel(2, Coverage::Customer(CUSTOMER), "acme"),
            email_channel(3, Coverage::Customer(CUSTOMER), "acme-2"),
            email_channel(4, Coverage::Site(SIBLING), "sibling"),
            email_channel(5, Coverage::Customer(OTHER), "other"),
        ];
        let host = |coverage| match &mail_channel(&channels, &d, coverage).unwrap().destination {
            Destination::Email(e) => e.host.clone(),
            _ => unreachable!(),
        };
        assert_eq!(host(Coverage::Site(SITE)), "acme");
        assert_eq!(host(Coverage::Site(SIBLING)), "sibling");
        assert_eq!(host(Coverage::Customer(CUSTOMER)), "acme");
        assert_eq!(host(Coverage::Customer(OTHER)), "other");
        assert_eq!(host(Coverage::Everything), "msp");
        assert!(mail_channel(&channels[1..], &d, Coverage::Everything).is_none());
    }

    fn event(id: i64, customer: Option<Uuid>, site: Option<Uuid>) -> Event {
        Event {
            id,
            event: "incident.opened".into(),
            customer_id: customer,
            site_id: site,
            details: json!({}),
            created_at: utc("2026-11-02T10:00:00Z"),
        }
    }

    #[test]
    fn deliveries_follow_coverage_and_reach_each_person_once() {
        let mut chatty = person(BOB, vec![(true, None, None)], false);
        chatty.chat = Some((
            Destination::Webhook(Webhook {
                url: "https://h/".into(),
            }),
            None,
            true,
        ));
        let d = directory(vec![
            person(ALICE, vec![(false, Some(CUSTOMER), None)], true),
            chatty,
        ]);
        let channels = vec![
            email_channel(1, Coverage::Everything, "msp"),
            email_channel(2, Coverage::Customer(CUSTOMER), "acme"),
        ];
        let mut acme = schedule(rota(Tz::UTC, Rotation::Daily, time(9, 0), "2026-11-02"));
        acme.rota.members = vec![Some(ALICE)];
        let mut everything = acme.clone();
        everything.coverage = Coverage::Everything;
        everything.rota.members = vec![Some(BOB)];
        let mut also = acme.clone();
        also.coverage = Coverage::Site(SITE);
        let events = vec![
            event(1, Some(CUSTOMER), Some(SITE)),
            event(2, Some(OTHER), None),
            event(3, None, None),
            event(4, Some(CUSTOMER), None),
            event(5, Some(CUSTOMER), Some(SITE)),
            event(6, Some(CUSTOMER), Some(SITE)),
        ];
        let targets = HashMap::from([(5, Uuid::from_u128(1))]);
        let people = Extra {
            copies: HashMap::from([(6, BOB)]),
            ..Extra::default()
        };
        let now = utc("2026-11-02T10:00:00Z");
        let out = deliveries(
            &[acme, everything, also],
            &d,
            &channels,
            &events,
            &targets,
            &people,
            now,
        );
        let mut seen: Vec<(Uuid, String, Vec<i64>)> = out
            .iter()
            .map(|(c, ids)| {
                let to = match &c.destination {
                    Destination::Email(e) => format!("{} via {}", e.to[0], e.host),
                    other => other.kind().to_owned(),
                };
                assert!(c.person);
                (c.id, to, ids.clone())
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                // Alice once per event, though two schedules name her, and
                // through her customer's mail server.
                (ALICE, "u1@x.io via acme".into(), vec![1, 4]),
                (BOB, "webhook".into(), vec![1, 2, 3, 4, 6]),
            ]
        );
    }
}
