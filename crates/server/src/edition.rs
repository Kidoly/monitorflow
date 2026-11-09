//! Where the core meets the Enterprise edition (ADR-0100), whose code is
//! in `ee/` under its own license. Built with the `enterprise` feature,
//! on by default; without it the server is AGPL code only and refuses
//! what needs the Enterprise edition in plain words.

#[cfg(feature = "enterprise")]
pub use monitorflow_server_ee::{license, store, License, Status, Verifier};

/// Whether this build includes the Enterprise edition.
pub const ENTERPRISE: bool = cfg!(feature = "enterprise");

/// Said when something needs the Enterprise edition and this build has none.
pub const NOT_IN_THIS_BUILD: &str =
    "this build of MonitorFlow does not include the Enterprise edition";

#[cfg(feature = "enterprise")]
pub use monitorflow_server_ee::{admission, Added};

/// Why an additional central server may not start or be added.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// In plain words, for the person adding it.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Lets this process start as the additional central server `name`
/// (ADR-0100), recording it the first time, with its audit entry. True
/// when it was added now, false when it had been before.
pub async fn admit_additional(pool: &sqlx::PgPool, name: &str) -> Result<bool, JoinError> {
    #[cfg(feature = "enterprise")]
    return admit_additional_with(pool, &Verifier::built_in(), name).await;
    #[cfg(not(feature = "enterprise"))]
    {
        let _ = (pool, name);
        Err(JoinError::Refused(format!(
            "{NOT_IN_THIS_BUILD}, which adds central servers. Run the release's images, or build \
             the server with its default features"
        )))
    }
}

/// [`admit_additional`] with the keys of `verifier`, for tests.
#[cfg(feature = "enterprise")]
pub async fn admit_additional_with(
    pool: &sqlx::PgPool,
    verifier: &Verifier,
    name: &str,
) -> Result<bool, JoinError> {
    use crate::api::audit::{self, Event};
    use admission::Admission;

    let mut tx = pool.begin().await?;
    let admission = admission::admit(&mut tx, verifier, name, store::today())
        .await?
        .map_err(|refusal| JoinError::Refused(capitalised(&refusal.to_string())))?;
    let Admission::New(license) = admission else {
        tx.commit().await?;
        return Ok(false);
    };
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "system",
            actor_id: name,
            action: "server_instance.add",
            target: Some(("server_instance", name)),
            details: serde_json::json!({
                "license_id": license.id,
                "server_instances": license.server_instances,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Whether one more central server may be added now, in plain words
/// either way; for the installer, before it changes anything.
pub async fn check_add(pool: &sqlx::PgPool) -> Result<String, JoinError> {
    #[cfg(feature = "enterprise")]
    {
        let mut db = pool.acquire().await?;
        let license = admission::check(&mut db, &Verifier::built_in(), store::today())
            .await?
            .map_err(|refusal| JoinError::Refused(capitalised(&refusal.to_string())))?;
        Ok(format!(
            "A central server can be added: the license for {} allows {}, until {}.",
            license.customer, license.server_instances, license.expires
        ))
    }
    #[cfg(not(feature = "enterprise"))]
    {
        let _ = pool;
        Err(JoinError::Refused(capitalised(NOT_IN_THIS_BUILD)))
    }
}

/// Refusals are written lower case to be embedded; alone they start a
/// sentence.
pub fn capitalised(message: &str) -> String {
    let mut chars = message.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalises() {
        assert_eq!(capitalised("this license"), "This license");
        assert_eq!(capitalised(""), "");
    }
}
