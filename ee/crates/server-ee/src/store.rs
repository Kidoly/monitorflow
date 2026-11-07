//! The installed license key (ADR-0100): one row of `license`, the text as
//! given, checked again each time it is read.

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{PgExecutor, PgPool};

use crate::license::{License, LicenseError, Verifier};

/// From this many days before expiry, administrators are told.
pub const EXPIRES_SOON_DAYS: u64 = 30;

/// The row of `license`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Installed {
    pub text: String,
    pub installed_at: DateTime<Utc>,
    pub installed_by: Option<String>,
}

/// Where the license stands, for the License page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// No license key: a single central server needs none.
    None,
    Valid,
    /// Valid, and expires within [`EXPIRES_SOON_DAYS`].
    ExpiresSoon,
    Expired,
    /// Stored, but no longer verifies with this build's keys.
    Invalid,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: State,
    pub license: Option<License>,
    /// Why an installed key is [`State::Invalid`].
    pub problem: Option<String>,
    pub installed_at: Option<DateTime<Utc>>,
    pub installed_by: Option<String>,
}

impl Status {
    /// What `installed` amounts to on `today`.
    pub fn of(installed: Option<Installed>, verifier: &Verifier, today: NaiveDate) -> Self {
        let Some(installed) = installed else {
            return Self {
                state: State::None,
                license: None,
                problem: None,
                installed_at: None,
                installed_by: None,
            };
        };
        let (state, license, problem) = match verifier.verify(&installed.text) {
            Ok(license) => (state_on(&license, today), Some(license), None),
            Err(error) => (State::Invalid, None, Some(error.to_string())),
        };
        Self {
            state,
            license,
            problem,
            installed_at: Some(installed.installed_at),
            installed_by: installed.installed_by,
        }
    }

    /// The license, when it allows adding central servers today.
    pub fn usable(&self) -> Option<&License> {
        match self.state {
            State::Valid | State::ExpiresSoon => self.license.as_ref(),
            State::None | State::Expired | State::Invalid => None,
        }
    }
}

fn state_on(license: &License, today: NaiveDate) -> State {
    if !license.is_current(today) {
        State::Expired
    } else if today.checked_add_days(Days::new(EXPIRES_SOON_DAYS)) > Some(license.expires) {
        State::ExpiresSoon
    } else {
        State::Valid
    }
}

/// Today in UTC, the day licenses count in.
pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}

pub async fn read(pool: &PgPool) -> Result<Option<Installed>, sqlx::Error> {
    sqlx::query_as("SELECT text, installed_at, installed_by FROM license")
        .fetch_optional(pool)
        .await
}

/// Checks `text` and stores it in place of any other. Refuses a key that
/// does not verify or has expired: nothing is stored then.
pub async fn install(
    db: impl PgExecutor<'_>,
    verifier: &Verifier,
    text: &str,
    by: &str,
) -> Result<Result<License, InstallError>, sqlx::Error> {
    let license = match verifier.verify(text) {
        Ok(license) => license,
        Err(error) => return Ok(Err(InstallError::Invalid(error))),
    };
    if !license.is_current(today()) {
        return Ok(Err(InstallError::Expired(license.expires)));
    }
    sqlx::query(
        "INSERT INTO license (singleton, text, installed_by) VALUES (true, $1, $2)
         ON CONFLICT (singleton) DO UPDATE
             SET text = EXCLUDED.text, installed_by = EXCLUDED.installed_by,
                 installed_at = now()",
    )
    .bind(text.trim())
    .bind(by)
    .execute(db)
    .await?;
    Ok(Ok(license))
}

/// Deletes the license key; false if there was none.
pub async fn remove(db: impl PgExecutor<'_>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM license").execute(db).await?;
    Ok(done.rows_affected() > 0)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Invalid(LicenseError),
    #[error("this license expired on {0}")]
    Expired(NaiveDate),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::tests::{key_pair, payload, sign};

    fn installed(text: String) -> Option<Installed> {
        Some(Installed {
            text,
            installed_at: Utc::now(),
            installed_by: Some("admin".into()),
        })
    }

    #[test]
    fn states() {
        let (pair, pem) = key_pair();
        let verifier = Verifier::from_pem(&pem);
        let day = |d: &str| d.parse::<NaiveDate>().unwrap();
        let text = sign(&pair, &payload(2, "2027-03-12"));
        let at = |d| Status::of(installed(text.clone()), &verifier, day(d));
        assert_eq!(
            Status::of(None, &verifier, day("2027-01-01")).state,
            State::None
        );
        assert_eq!(at("2027-01-01").state, State::Valid);
        assert_eq!(at("2027-02-10").state, State::Valid);
        assert_eq!(at("2027-02-11").state, State::ExpiresSoon);
        assert_eq!(at("2027-03-12").state, State::ExpiresSoon);
        assert!(at("2027-03-12").usable().is_some());
        assert_eq!(at("2027-03-13").state, State::Expired);
        assert!(at("2027-03-13").usable().is_none());
        assert_eq!(at("2027-03-13").license.unwrap().server_instances, 2);

        // Signed by a key this build does not trust (any more).
        let (_, other) = key_pair();
        let status = Status::of(
            installed(text),
            &Verifier::from_pem(&other),
            day("2027-01-01"),
        );
        assert_eq!(status.state, State::Invalid);
        assert!(status.problem.unwrap().contains("signature"));
        assert!(status.license.is_none());
    }
}
