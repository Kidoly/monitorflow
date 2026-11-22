//! Sends scheduled reports (ADR-0155). Every minute the leading central
//! server looks for schedules whose latest period has ended and was not
//! sent, claims each for 10 minutes so no other server sends it too, and
//! emails it through the schedule's channel with the PDF and the CSV.
//! A failure is told on the schedule and tried again 15 minutes later.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::{interval, MissedTickBehavior};
use uuid::Uuid;

use super::pdf::{self, period_words};
use super::report::Report;
use super::schedule::{due, Frequency};
use super::words::Language;
use super::{csv, for_customer, Span};
use crate::api::audit::{self, Event};
use crate::notify;
use crate::notify::send::{email_with_attachments, Attachment};

/// How often the schedules are looked at.
pub const POLL: Duration = Duration::from_secs(60);
/// How long a server holds a schedule while it sends.
pub const CLAIM: TimeDelta = TimeDelta::minutes(10);
/// How long after a failure it is tried again.
pub const RETRY: TimeDelta = TimeDelta::minutes(15);

/// A schedule as the sender reads it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Stored {
    pub id: Uuid,
    pub customer_id: Option<Uuid>,
    pub frequency: String,
    pub time_zone: String,
    pub language: String,
    pub pdf: bool,
    pub csv: bool,
    pub channel_id: Uuid,
    pub recipients: Vec<String>,
    pub last_period_end: Option<DateTime<Utc>>,
}

pub const SELECT: &str = "SELECT id, customer_id, frequency, time_zone, language, pdf, csv,
        channel_id, recipients, last_period_end FROM report_schedules";

impl Stored {
    pub fn zone(&self) -> Tz {
        self.time_zone.parse().unwrap_or(Tz::UTC)
    }

    pub fn language(&self) -> Language {
        Language::parse(&self.language).unwrap_or(Language::En)
    }

    pub fn frequency(&self) -> Frequency {
        Frequency::parse(&self.frequency).unwrap_or(Frequency::Monthly)
    }
}

/// A file's name: `availability-<customer>-<from>-<to>.<extension>`, with
/// only letters, digits and dashes.
pub fn file_name(report: &Report, zone: Tz, extension: &str) -> String {
    let mut name = String::from("availability");
    if let Some(customer) = &report.customer_name {
        name.push('-');
        name.extend(customer.chars().map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        }));
    }
    let day = |at: DateTime<Utc>| at.with_timezone(&zone).format("%Y-%m-%d").to_string();
    format!("{name}-{}-{}.{extension}", day(report.from), day(report.to))
}

/// The subject and text of the email of `report`.
pub fn message(report: &Report, zone: Tz, language: Language) -> (String, String) {
    let title = language.pick("Availability report", "Rapport de disponibilité");
    let period = period_words(report.from, report.to, zone, language);
    let subject = match &report.customer_name {
        Some(customer) => format!("{title}, {customer}, {period}"),
        None => format!("{title}, {period}"),
    };
    let body = format!(
        "{subject}\n\n{}\n\n{}\n",
        pdf::summary_text(report, language),
        language.pick(
            "The full report is attached.",
            "Le rapport complet est en pièce jointe."
        )
    );
    (subject, body)
}

/// What was sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub period: Span,
    pub recipients: Vec<String>,
}

/// Makes and sends `schedule`'s report of `period`; a scheduled sending
/// (`manual` false) records the period as sent. Records the outcome on
/// the schedule and, when sent, in the audit log as `actor`.
pub async fn send(
    pool: &PgPool,
    schedule: &Stored,
    period: Span,
    manual: bool,
    actor: (&str, &str),
) -> Result<Sent, String> {
    let outcome = make_and_send(pool, schedule, period).await;
    let now = Utc::now();
    let stored = match &outcome {
        Ok(sent) => {
            let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
            sqlx::query(
                "UPDATE report_schedules SET last_sent_at = $2, last_error = NULL,
                        last_error_at = NULL, claimed_until = NULL, retry_at = NULL,
                        last_period_end = CASE WHEN $3 THEN last_period_end ELSE $4 END
                 WHERE id = $1",
            )
            .bind(schedule.id)
            .bind(now)
            .bind(manual)
            .bind(period.1)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            audit::record(
                &mut *tx,
                Event {
                    actor_kind: actor.0,
                    actor_id: actor.1,
                    action: "report.sent",
                    target: Some(("report_schedule", &schedule.id.to_string())),
                    details: json!({
                        "customer_id": schedule.customer_id,
                        "from": period.0,
                        "to": period.1,
                        "recipients": sent.recipients,
                        "pdf": schedule.pdf,
                        "csv": schedule.csv,
                        "manual": manual,
                    }),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())
        }
        Err(error) => sqlx::query(
            "UPDATE report_schedules SET last_error = $2, last_error_at = $3, claimed_until = NULL,
                    retry_at = CASE WHEN $4 THEN retry_at ELSE $5 END
             WHERE id = $1",
        )
        .bind(schedule.id)
        .bind(error)
        .bind(now)
        .bind(manual)
        .bind(now + RETRY)
        .execute(pool)
        .await
        .map(drop)
        .map_err(|e| e.to_string()),
    };
    stored?;
    outcome
}

/// Whether a channel covering `coverage` may carry the report of
/// `customer`, `None` for everything: one covering everything, or that
/// customer as a whole. Checked when a schedule is saved, and again when
/// it is sent, since a channel may move or narrow in between.
pub fn channel_fits(coverage: notify::Coverage, customer: Option<Uuid>) -> bool {
    match (coverage, customer) {
        (notify::Coverage::Everything, _) => true,
        (notify::Coverage::Customer(c), Some(customer)) => c == customer,
        _ => false,
    }
}

async fn make_and_send(pool: &PgPool, schedule: &Stored, period: Span) -> Result<Sent, String> {
    let (zone, language) = (schedule.zone(), schedule.language());
    let mut conn = pool.acquire().await.map_err(|e| e.to_string())?;
    let channel = notify::load(&mut *conn, schedule.channel_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("The email channel was removed.")?;
    if !channel_fits(channel.coverage, schedule.customer_id) {
        return Err(
            "The email channel no longer covers this customer as a whole, or everything: choose another one."
                .into(),
        );
    }
    let uptime_zone = schedule.pdf.then_some(zone);
    let (report, uptime) = for_customer(
        &mut conn,
        schedule.customer_id,
        period,
        Utc::now(),
        uptime_zone,
    )
    .await
    .map_err(|e| format!("The report could not be made: {e}"))?;
    drop(conn);
    let mut attachments = Vec::new();
    if schedule.pdf {
        let copy = report.clone();
        let body = tokio::task::spawn_blocking(move || {
            pdf::render(&copy, uptime.as_ref(), zone, language)
        })
        .await
        .map_err(|e| format!("The PDF could not be made: {e}"))?;
        attachments.push(Attachment {
            name: file_name(&report, zone, "pdf"),
            content_type: "application/pdf",
            body,
        });
    }
    if schedule.csv {
        attachments.push(Attachment {
            name: file_name(&report, zone, "csv"),
            content_type: "text/csv; charset=utf-8",
            body: csv::render(&report, zone, language).into_bytes(),
        });
    }
    let (subject, body) = message(&report, zone, language);
    email_with_attachments(&channel, &schedule.recipients, &subject, &body, attachments)
        .await
        .map_err(|e| e.message)?;
    let recipients = match (&channel.destination, schedule.recipients.is_empty()) {
        (notify::Destination::Email(email), true) => email.to.clone(),
        _ => schedule.recipients.clone(),
    };
    Ok(Sent { period, recipients })
}

/// Takes `schedule`, as read, for one sending: not when another server
/// holds it, nor when its last period moved since it was read, as another
/// server has sent that period meanwhile.
pub async fn claim(
    pool: &PgPool,
    schedule: &Stored,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let claimed = sqlx::query(
        "UPDATE report_schedules SET claimed_until = $2
         WHERE id = $1 AND (claimed_until IS NULL OR claimed_until < $3)
           AND last_period_end IS NOT DISTINCT FROM $4",
    )
    .bind(schedule.id)
    .bind(now + CLAIM)
    .bind(now)
    .bind(schedule.last_period_end)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(claimed > 0)
}

/// One look: sends what is due and not held by another server.
pub async fn pass(pool: &PgPool, now: DateTime<Utc>) -> Result<usize, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let schedules: Vec<Stored> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE enabled AND (claimed_until IS NULL OR claimed_until < $1)
           AND (retry_at IS NULL OR retry_at <= $1)
         ORDER BY created_at, id"
    )))
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut sent = 0;
    for schedule in schedules {
        let Some(period) = due(
            schedule.frequency(),
            schedule.zone(),
            schedule.last_period_end,
            now,
        ) else {
            continue;
        };
        if !claim(pool, &schedule, now).await? {
            continue;
        }
        match send(
            pool,
            &schedule,
            period,
            false,
            ("system", "report-schedule"),
        )
        .await
        {
            Ok(_) => sent += 1,
            Err(error) => {
                tracing::warn!(schedule = %schedule.id, %error, "a scheduled report was not sent; will retry");
            }
        }
    }
    Ok(sent)
}

/// Looks every `poll` until `stop`.
pub async fn run(pool: PgPool, poll: Duration, stop: impl Future<Output = ()>) {
    tokio::pin!(stop);
    let mut tick = interval(poll);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut stop => return,
            _ = tick.tick() => {}
        }
        if let Err(error) = pass(&pool, Utc::now()).await {
            tracing::warn!(%error, "could not read the report schedules; will retry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::data::Data;
    use crate::availability::report::build;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    #[test]
    fn names_and_words_of_the_email() {
        let period = (at("2026-09-30T22:00:00Z"), at("2026-10-31T23:00:00Z"));
        let mut report = build(&Data::default(), period, at("2026-11-01T05:00:00Z"));
        report.customer_name = Some("Acme & Co".into());
        let paris = chrono_tz::Europe::Paris;
        assert_eq!(
            file_name(&report, paris, "pdf"),
            "availability-acme---co-2026-10-01-2026-11-01.pdf"
        );
        let (subject, body) = message(&report, paris, Language::Fr);
        assert_eq!(subject, "Rapport de disponibilité, Acme & Co, octobre 2026");
        assert!(body.contains("Il n’y a eu aucune panne."), "{body}");
        assert!(body.ends_with("Le rapport complet est en pièce jointe.\n"));
    }
}
