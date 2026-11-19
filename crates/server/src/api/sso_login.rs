//! Signing in through a provider, for people in a browser (ADR-0145): the
//! login page's buttons, the OpenID Connect redirect and its return, and
//! LDAP directories behind the password form. Never in `/api/v1`.

use axum::extract::{Path, Query, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::audit::{self, Event};
use super::client_addr::Peer;
use super::{session, ApiError, ApiSettings, ApiState};
use crate::secrets::Column;
use crate::sso::accounts::{self, Identity, Outcome, Provider, Refusal};
use crate::sso::config::{Kind, Settings};
use crate::sso::{ldap, oidc};

/// How long a sign-in may take at the provider.
const PENDING_S: i64 = 600;
const COOKIE_SECURE: &str = "__Host-mf_sso";
const COOKIE_PLAIN: &str = "mf_sso";

/// A provider on the login page.
#[derive(Debug, Serialize)]
pub struct LoginProvider {
    pub id: Uuid,
    pub name: String,
    pub kind: Kind,
}

/// Body of `GET /api/auth/providers`.
#[derive(Debug, Serialize)]
pub struct LoginProviders {
    /// Providers turned on, oldest first. OpenID Connect ones are buttons;
    /// directories sign in through the password form.
    pub providers: Vec<LoginProvider>,
}

/// `GET /api/auth/providers`: what the login page offers; no session.
pub async fn providers(State(state): State<ApiState>) -> Result<Json<LoginProviders>, ApiError> {
    let providers = Provider::all(&state.pool)
        .await?
        .into_iter()
        .filter(|p| p.enabled)
        .map(|p| LoginProvider {
            id: p.id,
            kind: p.settings.kind(),
            name: p.name,
        })
        .collect();
    Ok(Json(LoginProviders { providers }))
}

/// A sign-in in progress, sealed in a cookie: nothing is stored.
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    /// The provider.
    p: Uuid,
    /// `state`.
    s: String,
    /// `nonce`.
    n: String,
    /// The PKCE verifier.
    v: String,
    /// Expiry, seconds since the epoch.
    e: i64,
}

fn cookie_name(settings: &ApiSettings) -> &'static str {
    if settings.cookie_secure {
        COOKIE_SECURE
    } else {
        COOKIE_PLAIN
    }
}

fn pending_cookie(settings: &ApiSettings, value: &str, max_age: i64) -> HeaderValue {
    let secure = if settings.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    // Lax: the browser sends it back when the provider redirects to us.
    let cookie = format!(
        "{}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}",
        cookie_name(settings)
    );
    HeaderValue::from_str(&cookie).expect("cookie is ASCII")
}

fn read_pending(headers: &HeaderMap, settings: &ApiSettings) -> Option<Pending> {
    let name = cookie_name(settings);
    let sealed = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)?
        .1;
    let opened = crate::secrets::open(Column::SignInCookie, sealed).ok()?;
    let pending: Pending = serde_json::from_str(opened.expose()).ok()?;
    (pending.e > Utc::now().timestamp()).then_some(pending)
}

/// A 303 to `location` with `cookies`.
fn redirect(location: &str, cookies: Vec<HeaderValue>) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    headers.insert(
        LOCATION,
        HeaderValue::from_str(location).expect("ASCII location"),
    );
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    response
}

/// Back to the login page with a reason it puts in words.
fn to_login(settings: &ApiSettings, reason: &str) -> Response {
    redirect(
        &format!("/login/?sso={reason}"),
        vec![pending_cookie(settings, "", 0)],
    )
}

/// `GET /api/auth/oidc/{id}/start`: off to the provider.
pub async fn start(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let Ok(id) = id.parse::<Uuid>() else {
        return to_login(&state.settings, "unavailable");
    };
    let provider = match Provider::get(&state.pool, id).await {
        Ok(Some(p)) if p.enabled => p,
        Ok(_) => return to_login(&state.settings, "unavailable"),
        Err(error) => {
            tracing::error!(%error, "reading a sign-in provider");
            return to_login(&state.settings, "provider_error");
        }
    };
    let Settings::Oidc(settings) = &provider.settings else {
        return to_login(&state.settings, "unavailable");
    };
    let entry = match oidc::http(settings) {
        Ok(http) => state.oidc.get(id, &http, settings).await,
        Err(error) => Err(error),
    };
    let entry = match entry {
        Ok(entry) => entry,
        Err(error) => {
            tracing::warn!(provider = %id, %error, "sign-in provider unavailable");
            return to_login(&state.settings, "provider_error");
        }
    };
    let pending = Pending {
        p: id,
        s: oidc::random(),
        n: oidc::random(),
        v: oidc::random(),
        e: Utc::now().timestamp() + PENDING_S,
    };
    let url = oidc::authorize_url(
        &entry.discovery,
        settings,
        &super::identity_providers::redirect_uri(&state),
        &pending.s,
        &pending.n,
        &pending.v,
    );
    let sealed = match serde_json::to_string(&pending)
        .map_err(|e| e.to_string())
        .and_then(|json| {
            crate::secrets::seal(Column::SignInCookie, &json).map_err(|e| e.to_string())
        }) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::error!(%error, "sealing a sign-in");
            return to_login(&state.settings, "provider_error");
        }
    };
    redirect(
        &url,
        vec![pending_cookie(&state.settings, &sealed, PENDING_S)],
    )
}

/// The query of the provider's return.
#[derive(Debug, Deserialize)]
pub struct Callback {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

async fn audit_failure(
    state: &ApiState,
    user: Option<Uuid>,
    provider: &Provider,
    reason: &str,
    error: Option<String>,
) {
    let actor = user.map_or_else(|| "unknown".to_owned(), |u| u.to_string());
    let mut details = json!({
        "reason": reason,
        "via": provider.settings.kind().as_str(),
        "provider": provider.id,
    });
    if let Some(error) = error {
        details["error"] = json!(error);
    }
    let recorded = audit::record(
        &state.pool,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "auth.login_failed",
            target: None,
            details,
        },
    )
    .await;
    if let Err(error) = recorded {
        tracing::error!(%error, "recording a refused sign-in");
    }
}

/// Starts a session for `user` signed in through `provider`.
pub(super) async fn open_session(
    state: &ApiState,
    user: Uuid,
    provider: &Provider,
) -> Result<session::SessionToken, ApiError> {
    let mut tx = state.pool.begin().await?;
    session::prune(&mut *tx, &state.settings).await?;
    let token = session::create(&mut *tx, user, &state.settings).await?;
    let actor = user.to_string();
    audit::record(
        &mut *tx,
        Event {
            actor_kind: "user",
            actor_id: &actor,
            action: "auth.login",
            target: None,
            details: json!({ "via": provider.settings.kind().as_str(), "provider": provider.id }),
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(%user, provider = %provider.id, "signed in through a provider");
    Ok(token)
}

/// `GET /api/auth/oidc/callback`: the provider sends the browser back.
///
/// The cookie of `start` can be sent again for ten minutes, so trading
/// its code at the provider is limited per source address like a
/// password (ADR-0106): each refused one costs the provider a request and
/// the audit log a row.
pub async fn callback(
    State(state): State<ApiState>,
    peer: Peer,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    let settings = &state.settings;
    // No sign-in started in this browser, or another one: a forged return.
    let Some(pending) = read_pending(&headers, settings) else {
        return to_login(settings, "expired");
    };
    if query.state.as_deref() != Some(pending.s.as_str()) {
        return to_login(settings, "expired");
    }
    let provider = match Provider::get(&state.pool, pending.p).await {
        Ok(Some(p)) if p.enabled => p,
        Ok(_) => return to_login(settings, "unavailable"),
        Err(error) => {
            tracing::error!(%error, "reading a sign-in provider");
            return to_login(settings, "provider_error");
        }
    };
    if let Some(error) = query.error {
        let reason = if error == "access_denied" {
            "cancelled"
        } else {
            "provider_error"
        };
        tracing::info!(provider = %provider.id, %error, "the provider ended the sign-in");
        return to_login(settings, reason);
    }
    let Some(code) = query.code.filter(|c| !c.is_empty()) else {
        return to_login(settings, "provider_error");
    };
    let source = settings.trusted_proxies.source(&peer, &headers).await;
    if !state.login_limits.begin_provider(source) {
        tracing::debug!(%source, "provider return refused: too many attempts");
        return to_login(settings, "provider_error");
    }
    let identity = match oidc_identity(&state, &provider, &pending, &code).await {
        Ok(identity) => {
            state.login_limits.provider_succeeded(source);
            identity
        }
        Err(error) => {
            tracing::warn!(provider = %provider.id, %error, "OpenID Connect sign-in failed");
            audit_failure(
                &state,
                None,
                &provider,
                "provider_error",
                Some(error.to_string()),
            )
            .await;
            return to_login(settings, "provider_error");
        }
    };
    finish(&state, &provider, &identity).await
}

async fn oidc_identity(
    state: &ApiState,
    provider: &Provider,
    pending: &Pending,
    code: &str,
) -> Result<Identity, oidc::OidcError> {
    let Settings::Oidc(settings) = &provider.settings else {
        return Err(oidc::OidcError::Refused(
            "not an OpenID Connect provider".into(),
        ));
    };
    let secret = provider.secret().map_err(|reason| {
        oidc::OidcError::Refused(format!("the client secret does not open: {reason}"))
    })?;
    let http = oidc::http(settings)?;
    let entry = state.oidc.get(provider.id, &http, settings).await?;
    let tokens = oidc::exchange(
        &http,
        &entry.discovery,
        settings,
        secret.as_ref(),
        code,
        &pending.v,
        &super::identity_providers::redirect_uri(state),
    )
    .await?;
    let id_token = tokens
        .id_token
        .ok_or_else(|| oidc::OidcError::Token("the provider sent none".into()))?;
    let verified =
        oidc::verify(&state.oidc, provider.id, &http, settings, &entry, &id_token).await?;
    oidc::check_claims(
        &verified.claims,
        &oidc::Expected {
            issuer: &entry.discovery.issuer,
            client_id: &settings.client_id,
            nonce: &pending.n,
            now: Utc::now().timestamp(),
        },
    )?;
    let found = oidc::identity(
        &http,
        &entry.discovery,
        settings,
        &verified.claims,
        tokens.access_token.as_deref(),
    )
    .await?;
    Ok(Identity {
        subject: found.subject,
        username: found.username,
        groups: found.groups,
    })
}

async fn finish(state: &ApiState, provider: &Provider, identity: &Identity) -> Response {
    let settings = &state.settings;
    let outcome = match accounts::sign_in(&state.pool, provider, identity).await {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!(%error, "signing in through a provider");
            return to_login(settings, "provider_error");
        }
    };
    match outcome {
        Outcome::SignedIn { user, .. } => match open_session(state, user, provider).await {
            Ok(token) => redirect(
                "/",
                vec![
                    pending_cookie(settings, "", 0),
                    session::set_cookie(settings, &token),
                ],
            ),
            Err(error) => {
                tracing::error!(%error, "starting a session");
                to_login(settings, "provider_error")
            }
        },
        Outcome::Refused { reason, user } => {
            audit_failure(state, user, provider, reason.as_str(), None).await;
            tracing::info!(provider = %provider.id, reason = reason.as_str(), "sign-in through a provider refused");
            to_login(settings, reason.as_str())
        }
    }
}

/// What an LDAP attempt at the login form came to.
pub(super) enum LdapAttempt {
    SignedIn {
        user: Uuid,
        provider: Box<Provider>,
    },
    Refused {
        reason: Refusal,
        user: Option<Uuid>,
    },
    /// No directory knows this name with this password, or one failed.
    Failed {
        reason: &'static str,
    },
}

/// Signs `username` in through `provider`, or through each directory
/// turned on when `None` (a name no account has yet).
pub(super) async fn ldap_login(
    state: &ApiState,
    provider: Option<Uuid>,
    username: &str,
    password: &Secret,
) -> Result<LdapAttempt, ApiError> {
    let candidates: Vec<Provider> = Provider::all(&state.pool)
        .await?
        .into_iter()
        .filter(|p| p.enabled && p.settings.kind() == Kind::Ldap)
        .filter(|p| provider.is_none_or(|id| id == p.id))
        .collect();
    if candidates.is_empty() {
        return Ok(LdapAttempt::Failed {
            reason: if provider.is_some() {
                "provider_off"
            } else {
                "unknown_user"
            },
        });
    }
    let mut reason = "unknown_user";
    for candidate in candidates {
        let Settings::Ldap(settings) = &candidate.settings else {
            continue;
        };
        let service = match candidate.secret() {
            Ok(Some(secret)) => secret,
            Ok(None) | Err(_) => {
                tracing::warn!(provider = %candidate.id, "directory without a usable service account password");
                reason = "provider_error";
                continue;
            }
        };
        let person = match ldap::authenticate(settings, &service, username, password).await {
            Ok(person) => person,
            Err(error) if error.is_credentials() => {
                if reason != "provider_error" {
                    reason = if matches!(error, ldap::LdapError::BadPassword) {
                        "bad_password"
                    } else {
                        "unknown_user"
                    };
                }
                continue;
            }
            Err(error) => {
                tracing::warn!(provider = %candidate.id, %error, "directory sign-in failed");
                audit_failure(
                    state,
                    None,
                    &candidate,
                    "provider_error",
                    Some(error.to_string()),
                )
                .await;
                reason = "provider_error";
                continue;
            }
        };
        let identity = Identity {
            subject: username.trim().to_lowercase(),
            username: username.trim().to_owned(),
            groups: person.groups,
        };
        return Ok(
            match accounts::sign_in(&state.pool, &candidate, &identity).await? {
                Outcome::SignedIn { user, .. } => LdapAttempt::SignedIn {
                    user,
                    provider: Box::new(candidate),
                },
                Outcome::Refused { reason, user } => LdapAttempt::Refused { reason, user },
            },
        );
    }
    Ok(LdapAttempt::Failed { reason })
}
