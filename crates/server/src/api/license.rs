//! Administration › License (ADR-0100): the Enterprise license key, pasted
//! or uploaded, and where it stands in plain words. Global administrators
//! only. A single central server never needs one.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::CurrentUser;
use super::server_update::require_global_admin;
use super::{ApiError, ApiState};
#[cfg(feature = "enterprise")]
use crate::edition::{store, Status};

#[derive(Debug, Serialize)]
pub struct LicenseView {
    /// Whether this build includes the Enterprise edition at all.
    pub available: bool,
    /// Absent without the Enterprise edition.
    #[cfg(feature = "enterprise")]
    #[serde(flatten)]
    pub status: Status,
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    /// The license key's text, both blocks.
    pub text: String,
}

/// `GET /api/license`.
pub async fn get(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<LicenseView>, ApiError> {
    require_global_admin(&user)?;
    view(&state).await.map(Json)
}

#[cfg(feature = "enterprise")]
async fn view(state: &ApiState) -> Result<LicenseView, ApiError> {
    let installed = store::read(&state.pool).await?;
    Ok(LicenseView {
        available: true,
        status: Status::of(installed, &state.settings.license_keys, store::today()),
    })
}

#[cfg(not(feature = "enterprise"))]
async fn view(_: &ApiState) -> Result<LicenseView, ApiError> {
    Ok(LicenseView { available: false })
}

/// `PUT /api/license`: installs a license key in place of any other.
/// Refuses one that does not verify or has expired; audited.
#[cfg(feature = "enterprise")]
pub async fn install(
    State(state): State<ApiState>,
    user: CurrentUser,
    Json(request): Json<InstallRequest>,
) -> Result<Json<LicenseView>, ApiError> {
    use super::audit::{self, Event};
    use serde_json::json;

    require_global_admin(&user)?;
    let mut tx = state.pool.begin().await?;
    let license = store::install(
        &mut *tx,
        &state.settings.license_keys,
        &request.text,
        &user.username,
    )
    .await?
    .map_err(|e| ApiError::Invalid(capitalised(&e.to_string())))?;
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "license.install",
            target: Some(("license", &license.id.to_string())),
            details: json!({
                "customer": license.customer,
                "server_instances": license.server_instances,
                "expires": license.expires,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(license = %license.id, customer = %license.customer, "license key installed");
    view(&state).await.map(Json)
}

#[cfg(not(feature = "enterprise"))]
pub async fn install(
    State(_): State<ApiState>,
    user: CurrentUser,
    Json(_): Json<InstallRequest>,
) -> Result<Json<LicenseView>, ApiError> {
    require_global_admin(&user)?;
    Err(ApiError::Conflict(capitalised(
        crate::edition::NOT_IN_THIS_BUILD,
    )))
}

/// `DELETE /api/license`: 204, or 404 without one; audited. Central
/// servers already added keep running.
#[cfg(feature = "enterprise")]
pub async fn remove(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<StatusCode, ApiError> {
    use super::audit::{self, Event};
    use serde_json::json;

    require_global_admin(&user)?;
    let mut tx = state.pool.begin().await?;
    let before = Status::of(
        store::read(&state.pool).await?,
        &state.settings.license_keys,
        store::today(),
    );
    if !store::remove(&mut *tx).await? {
        return Err(ApiError::NotFound);
    }
    let id = before.license.as_ref().map(|l| l.id.to_string());
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &user.id.to_string(),
            action: "license.remove",
            target: id.as_deref().map(|id| ("license", id)),
            details: json!({ "customer": before.license.map(|l| l.customer) }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!("license key removed");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(not(feature = "enterprise"))]
pub async fn remove(State(_): State<ApiState>, user: CurrentUser) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    Err(ApiError::NotFound)
}

/// "This license…": errors are written lower case to be embedded.
fn capitalised(message: &str) -> String {
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
