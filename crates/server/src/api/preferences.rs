//! The signed-in person's own preferences (ADR-0137): how the web interface
//! looks for them, kept with their account so it follows them from one
//! browser to another. One small JSON document per user, of a known shape:
//! unknown keys are refused, lists and the whole body are capped.
//!
//! Only for a person in a browser: v1 leaves `/api/me/` out
//! ([`super::v1::left_out`]) and an API key gets 403. Not audited: a
//! preference changes nothing beyond the person's own screen.

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::{ApiError, ApiState};

/// Largest body `PUT /api/me/preferences` accepts, and the cap the table
/// holds the stored document to.
pub const MAX_BYTES: usize = 16 * 1024;
/// Most entries in one list of the document.
pub const MAX_ITEMS: usize = 64;
/// Longest item identifier, like `automaticUpdates`.
pub const MAX_ID: usize = 40;

/// The person's preferences. Every key is optional: absent means the
/// interface's default (the browser's language, the system theme).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<Sidebar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// The languages of the web interface (ADR-0136).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    Fr,
}

/// The main navigation. Items are named by the web interface's own
/// identifiers; the server checks their form only, so a new page needs no
/// server change. What a role cannot see stays hidden whatever this says.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sidebar {
    /// Icons only, without labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed: Option<bool>,
    /// Items the person does not use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Items in the person's order; those not listed keep their default place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

impl Preferences {
    /// Reads a document sent by the web interface: objects only (serde
    /// would read a struct from an array too), then [`Self::validate`].
    pub fn from_json(value: serde_json::Value) -> Result<Self, String> {
        let objects = value.is_object()
            && value
                .get("sidebar")
                .is_none_or(|sidebar| sidebar.is_object());
        if !objects {
            return Err("Preferences are a JSON object, and so is `sidebar`.".into());
        }
        let preferences: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        preferences.validate()?;
        Ok(preferences)
    }

    /// Checks what the types do not: the lists and their identifiers.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(sidebar) = &self.sidebar {
            check_ids("sidebar.hidden", &sidebar.hidden)?;
            check_ids("sidebar.order", &sidebar.order)?;
        }
        Ok(())
    }
}

fn check_ids(field: &str, ids: &[String]) -> Result<(), String> {
    if ids.len() > MAX_ITEMS {
        return Err(format!("`{field}` holds at most {MAX_ITEMS} items."));
    }
    for (i, id) in ids.iter().enumerate() {
        let well_formed = (1..=MAX_ID).contains(&id.len())
            && id.starts_with(|c: char| c.is_ascii_alphabetic())
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !well_formed {
            return Err(format!(
                "`{field}`: an item is a letter, then letters, digits or hyphens, at most {MAX_ID}."
            ));
        }
        if ids[..i].contains(id) {
            return Err(format!("`{field}` lists `{id}` twice."));
        }
    }
    Ok(())
}

/// The stored preferences of `user`, empty when none. A stored document
/// this version no longer reads counts as empty rather than failing every
/// page: the person picks again.
pub async fn load(db: impl PgExecutor<'_>, user: Uuid) -> Result<Preferences, sqlx::Error> {
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT document FROM user_preferences WHERE user_id = $1")
            .bind(user)
            .fetch_optional(db)
            .await?;
    Ok(match stored {
        None => Preferences::default(),
        Some(value) => serde_json::from_value(value).unwrap_or_else(|error| {
            tracing::warn!(%user, %error, "ignoring stored preferences this version does not read");
            Preferences::default()
        }),
    })
}

/// `GET /api/me/preferences`: the signed-in person's preferences, `{}`
/// when they never saved any.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<Preferences>, ApiError> {
    user.in_person()?;
    Ok(Json(load(&state.pool, user.id).await?))
}

/// `PUT /api/me/preferences`: replaces the signed-in person's preferences
/// with the body and returns them. 400 for an unknown key or value, a list
/// too long, an identifier out of form, or a body past [`MAX_BYTES`].
pub async fn put(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Json<Preferences>, ApiError> {
    user.in_person()?;
    let value = match request {
        Ok(Json(value)) => value,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Err(ApiError::Invalid(format!(
                "Preferences are limited to {} KiB.",
                MAX_BYTES / 1024
            )))
        }
        Err(rejection) => return Err(ApiError::Invalid(rejection.body_text())),
    };
    let preferences = Preferences::from_json(value).map_err(ApiError::Invalid)?;
    let document = serde_json::to_value(&preferences)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    sqlx::query(
        "INSERT INTO user_preferences (user_id, document) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET document = EXCLUDED.document, updated_at = now()",
    )
    .bind(user.id)
    .bind(&document)
    .execute(&state.pool)
    .await?;
    Ok(Json(preferences))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse(value: serde_json::Value) -> Result<Preferences, String> {
        Preferences::from_json(value)
    }

    #[test]
    fn a_full_document_round_trips() {
        let value = json!({
            "theme": "dark",
            "language": "fr",
            "sidebar": { "collapsed": false, "hidden": ["audit"], "order": ["incidents", "fleet"] }
        });
        let preferences = parse(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&preferences).unwrap(), value);
    }

    #[test]
    fn an_empty_document_is_the_default_and_stays_empty() {
        let preferences = parse(json!({})).unwrap();
        assert_eq!(preferences, Preferences::default());
        assert_eq!(serde_json::to_value(&preferences).unwrap(), json!({}));
        let empty_lists = parse(json!({ "sidebar": { "hidden": [], "order": [] } })).unwrap();
        assert_eq!(
            serde_json::to_value(&empty_lists).unwrap(),
            json!({ "sidebar": {} })
        );
    }

    #[test]
    fn unknown_keys_and_values_are_refused() {
        assert!(parse(json!({ "colour": "blue" })).is_err());
        assert!(parse(json!({ "sidebar": { "width": 300 } })).is_err());
        assert!(parse(json!({ "theme": "sepia" })).is_err());
        assert!(parse(json!({ "language": "de" })).is_err());
        assert!(parse(json!({ "sidebar": { "collapsed": "yes" } })).is_err());
        assert!(parse(json!([])).is_err());
        assert!(parse(json!({ "sidebar": [] })).is_err());
        assert!(parse(json!({ "sidebar": [true] })).is_err());
        assert!(parse(json!(null)).is_err());
    }

    #[test]
    fn identifiers_are_checked() {
        for bad in ["", "1fleet", "fleet!", "on call", "-x"] {
            let error = parse(json!({ "sidebar": { "hidden": [bad] } })).unwrap_err();
            assert!(error.contains("sidebar.hidden"), "{bad}: {error}");
        }
        let long = "a".repeat(MAX_ID + 1);
        assert!(parse(json!({ "sidebar": { "order": [long] } })).is_err());
        assert!(parse(json!({ "sidebar": { "order": ["a".repeat(MAX_ID)] } })).is_ok());
        assert!(parse(json!({ "sidebar": { "order": ["onCall", "auto-update2"] } })).is_ok());
    }

    #[test]
    fn duplicates_and_long_lists_are_refused() {
        let error = parse(json!({ "sidebar": { "order": ["fleet", "fleet"] } })).unwrap_err();
        assert!(error.contains("twice"), "{error}");
        let many: Vec<String> = (0..=MAX_ITEMS).map(|i| format!("item{i}")).collect();
        assert!(parse(json!({ "sidebar": { "hidden": many } })).is_err());
        let enough: Vec<String> = (0..MAX_ITEMS).map(|i| format!("item{i}")).collect();
        assert!(parse(json!({ "sidebar": { "hidden": enough } })).is_ok());
    }

    #[test]
    fn the_largest_valid_document_fits_the_cap() {
        let ids = |p: &str| -> Vec<String> {
            (0..MAX_ITEMS)
                .map(|i| format!("{p}{i:0>width$}", width = MAX_ID - 1))
                .collect()
        };
        let preferences = parse(json!({
            "theme": "system", "language": "en",
            "sidebar": { "collapsed": true, "hidden": ids("h"), "order": ids("o") }
        }))
        .unwrap();
        assert!(serde_json::to_vec(&preferences).unwrap().len() <= MAX_BYTES);
    }
}
