//! Risk classifier administration (ADR-0041): administrator allow, deny
//! and cache directory rules, the per-site "low-impact runs automatically"
//! policy, and a dry run of the classifier.
//!
//! Rules and policies belong to a customer, optionally narrowed to one of
//! its sites. As for campaigns, reading needs a role on the customer as a
//! whole (global or customer grant) and changing an administrator role on
//! it; another customer is 404, one the user reads without administering
//! it 403. Every change is audited.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::auth::CurrentUser;
use super::campaigns::authorize_customer;
use super::sensors::{body, parse_id, UNIQUE_VIOLATION};
use super::{ApiError, ApiState};
use crate::remediation::risk::store::{self, Kind, NewRule, Policy, StoredRule};
use crate::remediation::risk::{self, Decision, Shell, Tier};

/// Body of `POST /api/remediation/rules`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = RemediationRuleRequest)]
#[serde(deny_unknown_fields)]
pub struct RuleRequest {
    /// The customer the rule belongs to.
    pub customer_id: Uuid,
    /// One of the customer's sites to narrow the rule to; `null` or absent
    /// for every site of the customer.
    pub site_id: Option<Uuid>,
    pub kind: Kind,
    /// For `allow` and `deny`, words where `*` and `?` match within a word
    /// and `**` spans words; an `allow` pattern starts with a program name
    /// without wildcards. For `cache_dir`, an absolute directory of two
    /// components or more, outside system directories. Spaces are
    /// normalized and a trailing `/` dropped before the rule is stored.
    pub pattern: String,
    /// `read_only` or `low_impact` for an `allow` rule; `null` or absent
    /// for the other kinds.
    pub tier: Option<Tier>,
    /// Up to 256 characters, no control characters; empty by default.
    #[serde(default)]
    pub description: String,
}

/// Body of `GET /api/remediation/rules`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = RemediationRuleList)]
pub struct RuleList {
    /// By customer, then site (customer-wide rules first), kind and
    /// pattern.
    pub rules: Vec<StoredRule>,
}

/// Query string of `GET /api/remediation/rules`. Both filters are
/// optional.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    /// Only this customer's rules.
    pub customer_id: Option<Uuid>,
    /// Only the rules narrowed to this site (customer-wide rules are left
    /// out).
    pub site_id: Option<Uuid>,
}

/// Body of `PUT /api/remediation/policies`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = RemediationPolicyRequest)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    pub customer_id: Uuid,
    /// One of the customer's sites; `null` or absent sets the customer's
    /// own row, which applies to sites without a row of their own.
    pub site_id: Option<Uuid>,
    /// `true`: low-impact commands run without a person's approval.
    pub low_impact_auto: bool,
}

/// Body of `GET /api/remediation/policies`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = RemediationPolicyList)]
pub struct PolicyList {
    pub policies: Vec<Policy>,
}

/// Query string of `GET /api/remediation/policies`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PolicyParams {
    /// Only this customer's policies.
    pub customer_id: Option<Uuid>,
}

/// Body of `POST /api/remediation/classify`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[schema(as = ClassifyRequest)]
#[serde(deny_unknown_fields)]
pub struct ClassifyRequest {
    pub customer_id: Uuid,
    /// The site whose rules apply as well; `null` or absent for the
    /// customer-wide rules only.
    pub site_id: Option<Uuid>,
    /// The command line, as the AI would propose it. Neither stored nor
    /// logged.
    pub command: String,
    /// `posix` (the default) for Linux servers, `powershell` for Windows
    /// ones (ADR-0084).
    #[serde(default)]
    pub shell: Shell,
}

/// 400 unless `site` is absent or a site of `customer`.
async fn check_site(state: &ApiState, customer: Uuid, site: Option<Uuid>) -> Result<(), ApiError> {
    let Some(site) = site else {
        return Ok(());
    };
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_optional(&state.pool)
        .await?;
    if owner == Some(customer) {
        Ok(())
    } else {
        Err(ApiError::Invalid("unknown site".into()))
    }
}

/// `GET /api/remediation/rules`.
pub async fn list(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<RuleList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let rules = store::list(
        &state.pool,
        &user.visible_customers().bind(),
        params.customer_id,
        params.site_id,
    )
    .await?;
    Ok(Json(RuleList { rules }))
}

/// `POST /api/remediation/rules`.
pub async fn create(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<RuleRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<StoredRule>), ApiError> {
    let request = body(request)?;
    authorize_customer(&user, request.customer_id, true)?;
    let new = NewRule {
        customer_id: request.customer_id,
        site_id: request.site_id,
        kind: request.kind,
        pattern: request.pattern,
        tier: request.tier,
        description: request.description,
    };
    let pattern = store::validate(&new).map_err(|e| ApiError::Invalid(e.to_string()))?;
    check_site(&state, new.customer_id, new.site_id).await?;
    let mut tx = state.pool.begin().await?;
    let rule = match store::insert(&mut *tx, &new, &pattern, Some(user.id)).await {
        Ok(rule) => rule,
        Err(e)
            if e.as_database_error().and_then(|d| d.code()).as_deref()
                == Some(UNIQUE_VIOLATION) =>
        {
            return Err(ApiError::Conflict("this rule already exists".into()))
        }
        Err(e) => return Err(e.into()),
    };
    record(&mut tx, &user, "remediation_rule.create", &rule).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, rule = %rule.id, kind = %rule.kind, "remediation rule created");
    Ok((StatusCode::CREATED, Json(rule)))
}

/// `DELETE /api/remediation/rules/{id}`.
pub async fn delete(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = parse_id(&id)?;
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let mut tx = state.pool.begin().await?;
    let rule = store::get(&mut *tx, id).await?.ok_or(ApiError::NotFound)?;
    authorize_customer(&user, rule.customer_id, true)?;
    if !store::delete(&mut *tx, id).await? {
        return Err(ApiError::NotFound);
    }
    record(&mut tx, &user, "remediation_rule.delete", &rule).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, rule = %id, "remediation rule deleted");
    Ok(StatusCode::NO_CONTENT)
}

async fn record(
    tx: &mut sqlx::PgConnection,
    user: &CurrentUser,
    action: &str,
    rule: &StoredRule,
) -> Result<(), sqlx::Error> {
    let actor_id = user.id.to_string();
    let target = rule.id.to_string();
    audit::record(
        tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action,
            target: Some(("remediation_rule", &target)),
            details: json!({
                "customer_id": rule.customer_id,
                "site_id": rule.site_id,
                "kind": rule.kind,
                "pattern": rule.pattern,
                "tier": rule.tier,
            }),
        },
    )
    .await
}

/// `GET /api/remediation/policies`.
pub async fn list_policies(
    State(state): State<ApiState>,
    user: CurrentUser,
    params: Result<Query<PolicyParams>, QueryRejection>,
) -> Result<Json<PolicyList>, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let Query(params) = params.map_err(|e| ApiError::Invalid(e.body_text()))?;
    let policies = store::policies(
        &state.pool,
        &user.visible_customers().bind(),
        params.customer_id,
    )
    .await?;
    Ok(Json(PolicyList { policies }))
}

/// `PUT /api/remediation/policies`.
pub async fn set_policy(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<PolicyRequest>, JsonRejection>,
) -> Result<Json<Policy>, ApiError> {
    let request = body(request)?;
    authorize_customer(&user, request.customer_id, true)?;
    check_site(&state, request.customer_id, request.site_id).await?;
    let mut tx = state.pool.begin().await?;
    let policy = store::set_policy(
        &mut *tx,
        request.customer_id,
        request.site_id,
        request.low_impact_auto,
        Some(user.id),
    )
    .await?;
    let actor_id = user.id.to_string();
    let target = request.site_id.unwrap_or(request.customer_id).to_string();
    let kind = if request.site_id.is_some() {
        "site"
    } else {
        "customer"
    };
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor_id,
            action: "remediation_policy.update",
            target: Some((kind, &target)),
            details: json!({
                "customer_id": request.customer_id,
                "site_id": request.site_id,
                "low_impact_auto": request.low_impact_auto,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(policy))
}

/// `POST /api/remediation/classify`: what the classifier would decide for
/// `command` on servers of the site (or customer-wide rules only). The
/// command is neither stored nor logged.
pub async fn classify(
    State(state): State<ApiState>,
    user: CurrentUser,
    request: Result<Json<ClassifyRequest>, JsonRejection>,
) -> Result<Json<Decision>, ApiError> {
    let request = body(request)?;
    authorize_customer(&user, request.customer_id, false)?;
    check_site(&state, request.customer_id, request.site_id).await?;
    let rules = store::rule_set(&state.pool, request.customer_id, request.site_id).await?;
    Ok(Json(risk::classify_for(
        request.shell,
        &request.command,
        &rules,
    )))
}
