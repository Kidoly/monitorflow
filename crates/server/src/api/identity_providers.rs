//! Settings › Sign-in (ADR-0145): sign-in providers, their test, and the
//! groups that give roles. Global administrators only. Secrets are sealed
//! and never returned; a provider is turned on only after its test passes.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::access::Scope;
use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::sensors::{body, parse_id};
use super::users::GrantScope;
use super::{ApiError, ApiState, Role, RoleGrant};
use crate::secrets::Column;
use crate::sso::accounts::{self, Provider};
use crate::sso::config::{self, Kind, Settings};
use crate::sso::{self, ldap, oidc, Mapping};

/// The most groups one provider maps.
pub const MAX_MAPPINGS: usize = 500;
/// Longest group name.
const MAX_GROUP: usize = 256;

/// One line of "Groups and roles".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MappingView {
    /// The group's name as the provider sends it, compared without regard
    /// to case.
    pub group: String,
    pub role: Role,
    /// The customer, with all its sites; absent or `null` with `site_id`
    /// for everything.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// One site; never with `customer_id`.
    #[serde(default)]
    pub site_id: Option<Uuid>,
}

impl MappingView {
    fn of(mapping: &Mapping) -> Self {
        let (customer_id, site_id) = mapping.scope.columns();
        Self {
            group: mapping.group.clone(),
            role: mapping.role,
            customer_id,
            site_id,
        }
    }
}

/// A sign-in provider, without its secret.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProviderView {
    pub id: Uuid,
    pub kind: Kind,
    /// What the login page's button says: "Sign in with <name>".
    pub name: String,
    /// Whether people sign in through it.
    pub enabled: bool,
    /// Whether a client secret or service account password is stored. It
    /// is never returned.
    pub has_secret: bool,
    pub settings: Settings,
    /// Which role each group gives.
    pub mappings: Vec<MappingView>,
    /// Accounts that sign in through it.
    pub accounts: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Body of `GET /api/identity-providers`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProviderList {
    pub providers: Vec<ProviderView>,
    /// The address to enter as the redirect URI in an OpenID Connect
    /// provider.
    pub redirect_uri: String,
    /// Enabled administrators of everything who sign in with a password
    /// kept by MonitorFlow. There is always one (ADR-0145).
    pub local_administrators: i64,
}

/// Body of `POST /api/identity-providers` and of `PUT` on one.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = IdentityProviderRequest)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequest {
    pub name: String,
    /// Turning it on tests it first; 400 with the reason when the test
    /// fails.
    #[serde(default)]
    pub enabled: bool,
    /// The client secret (OpenID Connect, absent for a public client) or
    /// the service account's password (LDAP). Absent, `null` or empty
    /// keeps the stored one while the issuer or the directory's address
    /// stays the same.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub secret: Option<Secret>,
    pub settings: Settings,
}

/// Body of `POST /api/identity-providers/test`: what the form holds.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = IdentityProviderTestRequest)]
#[serde(deny_unknown_fields)]
pub struct TestRequest {
    /// A stored provider whose secret to use when `secret` is empty, and
    /// whose groups to read the roles from.
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub secret: Option<Secret>,
    pub settings: Settings,
    /// LDAP only: a person to sign in as, to see their groups and roles.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub password: Option<Secret>,
}

/// Body of the test's answer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = IdentityProviderTestResult)]
pub struct TestResult {
    pub ok: bool,
    /// Why it failed, in plain words; `null` when `ok`.
    pub error: Option<String>,
    /// What an OpenID Connect test found.
    pub oidc: Option<oidc::Found>,
    /// What an LDAP test found.
    pub ldap: Option<ldap::Found>,
    /// The roles the person tried would get, from the stored groups of
    /// `id`.
    pub roles: Vec<RoleGrant>,
}

/// Body of `GET` and `PUT /api/identity-providers/{id}/groups`.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = IdentityProviderGroups)]
#[serde(deny_unknown_fields)]
pub struct Mappings {
    pub mappings: Vec<MappingView>,
}

fn require_global_admin(user: &CurrentUser) -> Result<(), ApiError> {
    if user.administered_customers() == Scope::All {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn invalid(error: config::Invalid) -> ApiError {
    ApiError::Invalid(error.0)
}

pub(super) fn redirect_uri(state: &ApiState) -> String {
    format!(
        "{}/api/auth/oidc/callback",
        state.settings.web_url.trim_end_matches('/')
    )
}

async fn view(
    db: &mut PgConnection,
    provider: Provider,
    accounts: i64,
) -> Result<ProviderView, ApiError> {
    let mappings = accounts::mappings(&mut *db, provider.id).await?;
    Ok(ProviderView {
        id: provider.id,
        kind: provider.settings.kind(),
        name: provider.name,
        enabled: provider.enabled,
        has_secret: provider.secret.is_some(),
        settings: provider.settings,
        mappings: mappings.iter().map(MappingView::of).collect(),
        accounts,
        created_at: provider.created_at,
        updated_at: provider.updated_at,
    })
}

async fn view_of(db: &mut PgConnection, id: Uuid) -> Result<ProviderView, ApiError> {
    let provider = Provider::get(&mut *db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let accounts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE identity_provider_id = $1")
            .bind(id)
            .fetch_one(&mut *db)
            .await?;
    view(db, provider, accounts).await
}

async fn local_administrators(db: &mut PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(DISTINCT u.id) FROM users u JOIN user_roles r ON r.user_id = u.id
         WHERE r.role = 'administrator' AND r.customer_id IS NULL AND r.site_id IS NULL
           AND u.disabled_at IS NULL AND u.password_hash IS NOT NULL",
    )
    .fetch_one(db)
    .await
}

/// `GET /api/identity-providers`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
) -> Result<Json<ProviderList>, ApiError> {
    require_global_admin(&user)?;
    let mut db = state.pool.acquire().await?;
    let counts: HashMap<Uuid, i64> = accounts::account_counts(&mut *db)
        .await?
        .into_iter()
        .collect();
    let mut providers = Vec::new();
    for provider in Provider::all(&mut *db).await? {
        let accounts = counts.get(&provider.id).copied().unwrap_or(0);
        providers.push(view(&mut db, provider, accounts).await?);
    }
    Ok(Json(ProviderList {
        providers,
        redirect_uri: redirect_uri(&state),
        local_administrators: local_administrators(&mut db).await?,
    }))
}

/// `GET /api/identity-providers/{id}`.
pub async fn detail(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ProviderView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut db = state.pool.acquire().await?;
    Ok(Json(view_of(&mut db, id).await?))
}

/// The secret a change or a test uses: the typed one, else the stored one
/// while the address it was entered for stays.
fn usable_secret(
    typed: Option<Secret>,
    stored: Option<&Provider>,
    settings: &Settings,
) -> Result<(Option<Secret>, bool), ApiError> {
    let typed = config::secret(typed.as_ref().map(Secret::expose)).map_err(invalid)?;
    if let Some(typed) = typed {
        return Ok((Some(Secret::new(typed)), true));
    }
    let Some(stored) = stored.filter(|p| p.secret.is_some()) else {
        return Ok((None, false));
    };
    if stored.settings.kind() != settings.kind() || stored.settings.address() != settings.address()
    {
        return Err(ApiError::Invalid(
            "Enter the secret again: the stored one was entered for another address.".into(),
        ));
    }
    let secret = stored
        .secret()
        .map_err(|reason| ApiError::Invalid(format!("{reason}. Enter the secret again.")))?;
    Ok((secret, false))
}

/// Runs the connection test of `settings`; `Err` is the reason in words.
async fn run_test(
    state: &ApiState,
    settings: &Settings,
    secret: Option<&Secret>,
    person: Option<(&str, &Secret)>,
) -> Result<(Option<oidc::Found>, Option<ldap::Found>), String> {
    match settings {
        Settings::Oidc(s) => oidc::test(s, secret, &redirect_uri(state))
            .await
            .map(|found| (Some(found), None))
            .map_err(|e| sentence(&e.to_string())),
        Settings::Ldap(s) => {
            let Some(secret) = secret else {
                return Err("Enter the service account's password.".into());
            };
            ldap::test(s, secret, person)
                .await
                .map(|found| (None, Some(found)))
                .map_err(|e| sentence(&e.to_string()))
        }
    }
}

fn sentence(text: &str) -> String {
    if text.ends_with('.') {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// `POST /api/identity-providers/test`: saves nothing.
pub async fn test(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<TestRequest>, JsonRejection>,
) -> Result<Json<TestResult>, ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let settings = request.settings.checked().map_err(invalid)?;
    let stored = match request.id {
        Some(id) => Some(
            Provider::get(&state.pool, id)
                .await?
                .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    let (secret, _) = usable_secret(request.secret, stored.as_ref(), &settings)?;
    let person = match (&request.username, &request.password) {
        (Some(name), Some(password))
            if !name.trim().is_empty() && matches!(settings, Settings::Ldap(_)) =>
        {
            Some((name.as_str(), password))
        }
        _ => None,
    };
    let outcome = run_test(&state, &settings, secret.as_ref(), person).await;
    tracing::info!(user = %user.id, kind = settings.kind().as_str(), ok = outcome.is_ok(), "tested a sign-in provider");
    Ok(Json(match outcome {
        Ok((oidc, ldap)) => {
            let mut roles = Vec::new();
            if let (Some(found), Some(stored)) = (&ldap, &stored) {
                let mappings = accounts::mappings(&state.pool, stored.id).await?;
                roles = sso::grants(&found.groups, &mappings)
                    .into_iter()
                    .map(|(role, scope)| {
                        let (customer_id, site_id) = scope.columns();
                        RoleGrant {
                            role,
                            customer_id,
                            site_id,
                        }
                    })
                    .collect();
            }
            TestResult {
                ok: true,
                error: None,
                oidc,
                ldap,
                roles,
            }
        }
        Err(error) => TestResult {
            ok: false,
            error: Some(error),
            oidc: None,
            ldap: None,
            roles: Vec::new(),
        },
    }))
}

fn audit_settings(provider: &Provider) -> serde_json::Value {
    json!({
        "name": provider.name,
        "enabled": provider.enabled,
        "settings": provider.settings,
        "has_secret": provider.secret.is_some(),
    })
}

async fn record(
    db: &mut PgConnection,
    user: &CurrentUser,
    action: &str,
    target: Uuid,
    details: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let actor = user.id.to_string();
    let target = target.to_string();
    audit::record(
        db,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action,
            target: Some(("identity_provider", &target)),
            details,
        },
    )
    .await
}

fn duplicate_name(error: sqlx::Error) -> ApiError {
    match &error {
        sqlx::Error::Database(e) if e.constraint() == Some("identity_providers_name_key") => {
            ApiError::Conflict("Another sign-in provider has this name.".into())
        }
        _ => error.into(),
    }
}

fn seal(secret: Option<&Secret>) -> Result<Option<String>, ApiError> {
    secret
        .map(|s| crate::secrets::seal(Column::IdentityProviders, s.expose()))
        .transpose()
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// `POST /api/identity-providers`: 201 with the provider.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ProviderRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ProviderView>), ApiError> {
    require_global_admin(&user)?;
    let request = body(request)?;
    let name = config::name(&request.name).map_err(invalid)?;
    let settings = request.settings.checked().map_err(invalid)?;
    let (secret, _) = usable_secret(request.secret, None, &settings)?;
    if request.enabled {
        run_test(&state, &settings, secret.as_ref(), None)
            .await
            .map_err(ApiError::Invalid)?;
    }
    let config = serde_json::to_value(&settings).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut tx = state.pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO identity_providers (kind, name, enabled, config, secret)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(settings.kind().as_str())
    .bind(&name)
    .bind(request.enabled)
    .bind(config)
    .bind(seal(secret.as_ref())?)
    .fetch_one(&mut *tx)
    .await
    .map_err(duplicate_name)?;
    let created = Provider::get(&mut *tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        &user,
        "identity_provider.create",
        id,
        audit_settings(&created),
    )
    .await?;
    let view = view_of(&mut tx, id).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, provider = %id, kind = settings.kind().as_str(), "added a sign-in provider");
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PUT /api/identity-providers/{id}`: replaces it.
pub async fn update(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<ProviderRequest>, JsonRejection>,
) -> Result<Json<ProviderView>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    let name = config::name(&request.name).map_err(invalid)?;
    let settings = request.settings.checked().map_err(invalid)?;
    let before = Provider::get(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if before.settings.kind() != settings.kind() {
        return Err(ApiError::Invalid(
            "A provider keeps its kind; add another one instead.".into(),
        ));
    }
    let (secret, secret_changed) = usable_secret(request.secret, Some(&before), &settings)?;
    let same = before.enabled && !secret_changed && before.settings == settings;
    if request.enabled && !same {
        run_test(&state, &settings, secret.as_ref(), None)
            .await
            .map_err(ApiError::Invalid)?;
    }
    let config = serde_json::to_value(&settings).map_err(|e| ApiError::Internal(e.to_string()))?;
    let mut tx = state.pool.begin().await?;
    let old = Provider::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // A concurrent change of the address since the check above drops the
    // stored secret rather than send it elsewhere.
    let keep_stored = !secret_changed && old.settings.address() == settings.address();
    sqlx::query(
        "UPDATE identity_providers
         SET name = $2, enabled = $3, config = $4,
             secret = CASE WHEN $6 THEN $5 WHEN $7 THEN secret END,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(request.enabled)
    .bind(config)
    .bind(seal(secret.as_ref().filter(|_| secret_changed))?)
    .bind(secret_changed)
    .bind(keep_stored)
    .execute(&mut *tx)
    .await
    .map_err(duplicate_name)?;
    let new = Provider::get(&mut *tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut ended = 0;
    if old.enabled && !new.enabled {
        ended = accounts::end_provider_sessions(&mut tx, id).await?;
    }
    record(
        &mut tx,
        &user,
        "identity_provider.update",
        id,
        json!({
            "old": audit_settings(&old),
            "new": audit_settings(&new),
            "secret_changed": secret_changed,
            "sessions_ended": ended,
        }),
    )
    .await?;
    let view = view_of(&mut tx, id).await?;
    tx.commit().await?;
    state.oidc.forget(id);
    tracing::info!(user = %user.id, provider = %id, enabled = new.enabled, "changed a sign-in provider");
    Ok(Json(view))
}

/// `DELETE /api/identity-providers/{id}`: its accounts stay, without
/// roles and without a way to sign in.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let mut tx = state.pool.begin().await?;
    let old = Provider::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let released = accounts::release(&mut tx, id).await?;
    sqlx::query("DELETE FROM identity_providers WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &user,
        "identity_provider.delete",
        id,
        json!({ "old": audit_settings(&old), "accounts": released }),
    )
    .await?;
    tx.commit().await?;
    state.oidc.forget(id);
    tracing::info!(user = %user.id, provider = %id, accounts = released, "removed a sign-in provider");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/identity-providers/{id}/groups`.
pub async fn groups(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<Mappings>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    Provider::get(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mappings = accounts::mappings(&state.pool, id).await?;
    Ok(Json(Mappings {
        mappings: mappings.iter().map(MappingView::of).collect(),
    }))
}

async fn checked_mapping(db: &mut PgConnection, view: MappingView) -> Result<Mapping, ApiError> {
    let group = view.group.trim();
    if group.is_empty() {
        return Err(ApiError::Invalid("Enter the name of each group.".into()));
    }
    if group.len() > MAX_GROUP || group.chars().any(char::is_control) {
        return Err(ApiError::Invalid(format!(
            "The group {group:.40} is not a valid name."
        )));
    }
    let scope = GrantScope::from_columns(view.customer_id, view.site_id).ok_or_else(|| {
        ApiError::Invalid("A role applies to a customer or a site, not both.".into())
    })?;
    let exists: bool = match scope {
        GrantScope::Global => true,
        GrantScope::Customer(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customers WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?
        }
        GrantScope::Site(id) => {
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?
        }
    };
    if !exists {
        return Err(ApiError::Invalid(
            "This customer or site does not exist.".into(),
        ));
    }
    Ok(Mapping {
        group: group.to_owned(),
        role: view.role,
        scope,
    })
}

/// `PUT /api/identity-providers/{id}/groups`: replaces every mapping and
/// applies them to the provider's accounts at once.
pub async fn set_groups(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
    request: Result<Json<Mappings>, JsonRejection>,
) -> Result<Json<Mappings>, ApiError> {
    require_global_admin(&user)?;
    let id = parse_id(&id)?;
    let request = body(request)?;
    if request.mappings.len() > MAX_MAPPINGS {
        return Err(ApiError::Invalid(format!(
            "A provider maps at most {MAX_MAPPINGS} groups."
        )));
    }
    let mut tx = state.pool.begin().await?;
    Provider::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut new: Vec<Mapping> = Vec::new();
    for view in request.mappings {
        let mapping = checked_mapping(&mut tx, view).await?;
        let duplicate = new.iter().any(|m| {
            m.group.eq_ignore_ascii_case(&mapping.group)
                && m.role == mapping.role
                && m.scope == mapping.scope
        });
        if !duplicate {
            new.push(mapping);
        }
    }
    let old = accounts::mappings(&mut *tx, id).await?;
    sqlx::query("DELETE FROM identity_provider_groups WHERE provider_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for mapping in &new {
        let (customer, site) = mapping.scope.columns();
        sqlx::query(
            "INSERT INTO identity_provider_groups (provider_id, group_name, role, customer_id, site_id)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(&mapping.group)
        .bind(mapping.role.as_str())
        .bind(customer)
        .bind(site)
        .execute(&mut *tx)
        .await?;
    }
    let changed = accounts::resync(&mut tx, id).await?;
    let views = |m: &[Mapping]| m.iter().map(MappingView::of).collect::<Vec<_>>();
    record(
        &mut tx,
        &user,
        "identity_provider.groups",
        id,
        json!({ "old": views(&old), "new": views(&new), "accounts_changed": changed }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, provider = %id, mappings = new.len(), accounts_changed = changed, "changed the groups of a sign-in provider");
    Ok(Json(Mappings {
        mappings: views(&new),
    }))
}
