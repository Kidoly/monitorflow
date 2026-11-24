//! The language the AI writes its diagnosis, reasoning and reasons in
//! (ADR-0181), so people read them in the language of their interface.
//!
//! A global administrator may choose it (`server_settings.ai_language`).
//! Without a choice it follows the people: the language most users chose
//! for the web interface (ADR-0137), English when none chose or on a tie.
//! The server keeps no language per organization, and the AI works for no
//! one person: an incident is read by everyone who covers its server.

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

/// A language the web interface speaks (ADR-0136).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Fr,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Fr => "fr",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::En, Self::Fr]
            .into_iter()
            .find(|l| l.as_str() == text)
    }

    /// Its English name, for the prompt.
    pub fn english_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Fr => "French",
        }
    }
}

/// The administrators' choice, `None` to follow the people.
pub async fn chosen(db: impl PgExecutor<'_>) -> Result<Option<Language>, sqlx::Error> {
    let word: Option<Option<String>> =
        sqlx::query_scalar("SELECT ai_language FROM server_settings")
            .fetch_optional(db)
            .await?;
    Ok(word.flatten().as_deref().and_then(Language::parse))
}

/// The language most users chose for the web interface; English first on
/// a tie, `None` when nobody chose.
pub async fn most_chosen(db: impl PgExecutor<'_>) -> Result<Option<Language>, sqlx::Error> {
    let word: Option<String> = sqlx::query_scalar(
        "SELECT p.document->>'language' AS language
         FROM user_preferences p JOIN users u ON u.id = p.user_id
         WHERE p.document->>'language' IN ('en', 'fr') AND u.disabled_at IS NULL
         GROUP BY 1
         ORDER BY count(*) DESC, 1
         LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(word.as_deref().and_then(Language::parse))
}

/// The language of the next AI request.
pub async fn effective(pool: &sqlx::PgPool) -> Result<Language, sqlx::Error> {
    Ok(match chosen(pool).await? {
        Some(language) => language,
        None => most_chosen(pool).await?.unwrap_or_default(),
    })
}

/// Sets the choice; `None` follows the people again.
pub async fn set(db: impl PgExecutor<'_>, language: Option<Language>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO server_settings (singleton, ai_language) VALUES (true, $1)
         ON CONFLICT (singleton) DO UPDATE SET ai_language = EXCLUDED.ai_language",
    )
    .bind(language.map(Language::as_str))
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_its_words() {
        for language in [Language::En, Language::Fr] {
            assert_eq!(Language::parse(language.as_str()), Some(language));
        }
        assert_eq!(Language::parse("de"), None);
        assert_eq!(Language::default(), Language::En);
        assert_eq!(serde_json::to_value(Language::Fr).unwrap(), "fr");
    }
}
