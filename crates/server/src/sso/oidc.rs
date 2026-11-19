//! OpenID Connect (ADR-0145): discovery, the provider's keys kept for an
//! hour and fetched again for a key it rotated in, the authorization code
//! flow with PKCE, the ID token's checks and the connection test.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use monitorflow_common::redact::Secret;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::{StatusCode, Url};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::config::OidcSettings;
use super::jws::{self, JwkSet, JwsError};

/// How long a request to the provider may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Largest answer read from the provider.
const MAX_BODY: usize = 512 * 1024;
/// How long discovery and keys are kept.
const KEEP: Duration = Duration::from_secs(60 * 60);
/// The least time between two fetches of the keys for an unknown `kid`.
const REFETCH_EVERY: Duration = Duration::from_secs(30);
/// Leeway on `exp` and `iat`.
pub const LEEWAY_S: i64 = 60;
/// The code the connection test trades, which no provider issued.
const TEST_CODE: &str = "monitorflow-connection-test";

/// `application/x-www-form-urlencoded`: everything but `*-._` and
/// alphanumerics is encoded.
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'*')
    .remove(b'-')
    .remove(b'.')
    .remove(b'_');

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OidcError {
    #[error("Could not reach the provider: {0}")]
    Unreachable(String),
    #[error("The provider answered {0} at {1}")]
    Status(u16, String),
    #[error("The provider's answer at {0} is not what OpenID Connect expects")]
    Unreadable(String),
    #[error("The discovery document names the issuer {found}, not {expected}: enter the issuer exactly as the provider names it")]
    Issuer { expected: String, found: String },
    #[error("The provider's {0} is not an https address")]
    NotHttps(&'static str),
    #[error("The provider does not offer PKCE with S256, which MonitorFlow requires")]
    NoPkce,
    #[error("The provider publishes no signing key MonitorFlow can use (RS256, PS256, ES256, ES384 or EdDSA)")]
    NoKey,
    #[error("The provider signs ID tokens only with {0}; choose an RS256 or ES256 signing key for this client in the provider")]
    SigningAlgorithm(String),
    #[error("The provider refused the client ID or the client secret")]
    Client,
    #[error("The provider refused the sign-in: {0}")]
    Refused(String),
    #[error("The ID token was refused: {0}")]
    Token(String),
}

/// The fields of the discovery document MonitorFlow reads.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub id_token_signing_alg_values_supported: Option<Vec<String>>,
}

impl Discovery {
    fn check(&self, typed_issuer: &str) -> Result<(), OidcError> {
        if self.issuer.trim_end_matches('/') != typed_issuer.trim_end_matches('/') {
            return Err(OidcError::Issuer {
                expected: typed_issuer.to_owned(),
                found: self.issuer.clone(),
            });
        }
        for (what, url) in [
            ("issuer", Some(&self.issuer)),
            ("authorization endpoint", Some(&self.authorization_endpoint)),
            ("token endpoint", Some(&self.token_endpoint)),
            ("key set", Some(&self.jwks_uri)),
            ("userinfo endpoint", self.userinfo_endpoint.as_ref()),
        ] {
            if let Some(url) = url {
                // Visible ASCII only: the authorization endpoint goes into
                // a Location header as it is.
                let header_safe = url.bytes().all(|b| b.is_ascii_graphic());
                if !header_safe || Url::parse(url).map_or(true, |u| u.scheme() != "https") {
                    return Err(OidcError::NotHttps(what));
                }
            }
        }
        if let Some(methods) = &self.code_challenge_methods_supported {
            if !methods.iter().any(|m| m == "S256") {
                return Err(OidcError::NoPkce);
            }
        }
        if let Some(algs) = &self.id_token_signing_alg_values_supported {
            if !algs.is_empty() && !algs.iter().any(|a| jws::Alg::parse(a).is_some()) {
                return Err(OidcError::SigningAlgorithm(algs.join(", ")));
            }
        }
        Ok(())
    }

    /// The scopes of `settings` the provider lists, all of them when it
    /// lists none; `openid` always.
    pub fn scopes(&self, settings: &OidcSettings) -> String {
        settings
            .scopes
            .split_whitespace()
            .filter(|s| {
                *s == "openid"
                    || self
                        .scopes_supported
                        .as_ref()
                        .is_none_or(|known| known.iter().any(|k| k == s))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether the client secret goes in the body rather than in Basic.
    fn secret_in_body(&self) -> bool {
        self.token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|m| {
                !m.iter().any(|m| m == "client_secret_basic")
                    && m.iter().any(|m| m == "client_secret_post")
            })
    }
}

/// An HTTP client of the provider: its certificate authority, no
/// redirects, a timeout.
pub fn http(settings: &OidcSettings) -> Result<reqwest::Client, OidcError> {
    let tls = super::tls::client_config(settings.ca_pem.as_deref())
        .map_err(|e| OidcError::Unreachable(e.to_string()))?;
    reqwest::Client::builder()
        .use_preconfigured_tls((*tls).clone())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| OidcError::Unreachable(e.to_string()))
}

fn reach(error: reqwest::Error) -> OidcError {
    OidcError::Unreachable(monitorflow_common::tls::error_chain(&error))
}

async fn read(mut response: reqwest::Response) -> Result<Vec<u8>, OidcError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(reach)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(OidcError::Unreadable(response.url().to_string()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<T, OidcError> {
    let mut request = http.get(url).header("accept", "application/json");
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(reach)?;
    if !response.status().is_success() {
        return Err(OidcError::Status(
            response.status().as_u16(),
            url.to_owned(),
        ));
    }
    let body = read(response).await?;
    serde_json::from_slice(&body).map_err(|_| OidcError::Unreadable(url.to_owned()))
}

/// The provider's discovery document, checked against the typed issuer.
pub async fn discover(
    http: &reqwest::Client,
    settings: &OidcSettings,
) -> Result<Discovery, OidcError> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        settings.issuer.trim_end_matches('/')
    );
    let discovery: Discovery = get_json(http, &url, None).await?;
    discovery.check(&settings.issuer)?;
    Ok(discovery)
}

async fn keys(http: &reqwest::Client, discovery: &Discovery) -> Result<JwkSet, OidcError> {
    let response = http.get(&discovery.jwks_uri).send().await.map_err(reach)?;
    if !response.status().is_success() {
        return Err(OidcError::Status(
            response.status().as_u16(),
            discovery.jwks_uri.clone(),
        ));
    }
    let body = read(response).await?;
    JwkSet::parse(&body).map_err(|_| OidcError::Unreadable(discovery.jwks_uri.clone()))
}

/// Discovery and keys of each provider, kept for [`KEEP`].
#[derive(Default, Clone)]
pub struct Cache {
    entries: Arc<Mutex<HashMap<Uuid, Arc<EntryView>>>>,
    forced: Arc<Mutex<HashMap<Uuid, Instant>>>,
}

/// What [`Cache::get`] hands out.
pub struct EntryView {
    pub discovery: Discovery,
    pub keys: JwkSet,
    version: String,
    fetched: Instant,
}

fn version(settings: &OidcSettings) -> String {
    let ca = settings.ca_pem.as_deref().unwrap_or_default();
    format!(
        "{}\n{}",
        settings.issuer,
        URL_SAFE_NO_PAD.encode(digest(&SHA256, ca.as_bytes()))
    )
}

impl Cache {
    /// The provider's discovery and keys, fetched when absent, older than
    /// an hour or for other settings.
    pub async fn get(
        &self,
        id: Uuid,
        http: &reqwest::Client,
        settings: &OidcSettings,
    ) -> Result<Arc<EntryView>, OidcError> {
        let version = version(settings);
        let cached = self.entries.lock().expect("oidc cache").get(&id).cloned();
        if let Some(entry) = cached {
            if entry.version == version && entry.fetched.elapsed() < KEEP {
                return Ok(entry);
            }
        }
        self.fetch(id, http, settings, version).await
    }

    async fn fetch(
        &self,
        id: Uuid,
        http: &reqwest::Client,
        settings: &OidcSettings,
        version: String,
    ) -> Result<Arc<EntryView>, OidcError> {
        let discovery = discover(http, settings).await?;
        let keys = keys(http, &discovery).await?;
        let entry = Arc::new(EntryView {
            discovery,
            keys,
            version,
            fetched: Instant::now(),
        });
        self.entries
            .lock()
            .expect("oidc cache")
            .insert(id, entry.clone());
        Ok(entry)
    }

    /// The keys fetched again for a token whose `kid` they lack, at most
    /// once per [`REFETCH_EVERY`]; `None` when it is too soon.
    pub async fn refetch(
        &self,
        id: Uuid,
        http: &reqwest::Client,
        settings: &OidcSettings,
    ) -> Result<Option<Arc<EntryView>>, OidcError> {
        {
            let mut forced = self.forced.lock().expect("oidc cache");
            if forced
                .get(&id)
                .is_some_and(|at| at.elapsed() < REFETCH_EVERY)
            {
                return Ok(None);
            }
            forced.insert(id, Instant::now());
        }
        self.fetch(id, http, settings, version(settings))
            .await
            .map(Some)
    }

    /// Forgets a provider, after a change of its settings.
    pub fn forget(&self, id: Uuid) {
        self.entries.lock().expect("oidc cache").remove(&id);
        self.forced.lock().expect("oidc cache").remove(&id);
    }
}

/// A random value of 256 bits, base64url.
pub fn random() -> String {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system random generator failed");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 challenge of a PKCE verifier (RFC 7636).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                utf8_percent_encode(k, FORM),
                utf8_percent_encode(v, FORM)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Where the browser goes to sign in.
pub fn authorize_url(
    discovery: &Discovery,
    settings: &OidcSettings,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    verifier: &str,
) -> String {
    let query = form(&[
        ("response_type", "code"),
        ("client_id", &settings.client_id),
        ("redirect_uri", redirect_uri),
        ("scope", &discovery.scopes(settings)),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
    ]);
    let separator = if discovery.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{}{separator}{query}", discovery.authorization_endpoint)
}

/// What the token endpoint answers.
#[derive(Debug, Deserialize)]
pub struct Tokens {
    pub id_token: Option<String>,
    pub access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// A request to the token endpoint, with the client's authentication.
async fn token_request(
    http: &reqwest::Client,
    discovery: &Discovery,
    settings: &OidcSettings,
    secret: Option<&Secret>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<(StatusCode, Vec<u8>), OidcError> {
    let mut pairs = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    let mut request = http
        .post(&discovery.token_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json");
    match secret {
        Some(secret) if discovery.secret_in_body() => {
            pairs.push(("client_id", &settings.client_id));
            pairs.push(("client_secret", secret.expose()));
        }
        Some(secret) => {
            // RFC 6749 §2.3.1: both are form-encoded before Basic.
            let user = form(&[("", &settings.client_id)]);
            let pass = form(&[("", secret.expose())]);
            let basic = STANDARD.encode(format!("{}:{}", &user[1..], &pass[1..]));
            request = request.header("authorization", format!("Basic {basic}"));
        }
        None => pairs.push(("client_id", &settings.client_id)),
    }
    let response = request.body(form(&pairs)).send().await.map_err(reach)?;
    let status = response.status();
    Ok((status, read(response).await?))
}

/// Trades `code` for the tokens.
pub async fn exchange(
    http: &reqwest::Client,
    discovery: &Discovery,
    settings: &OidcSettings,
    secret: Option<&Secret>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Tokens, OidcError> {
    let (status, body) = token_request(
        http,
        discovery,
        settings,
        secret,
        code,
        verifier,
        redirect_uri,
    )
    .await?;
    if !status.is_success() {
        return Err(match serde_json::from_slice::<TokenError>(&body) {
            Ok(e) if e.error == "invalid_client" || e.error == "unauthorized_client" => {
                OidcError::Client
            }
            _ if status == StatusCode::UNAUTHORIZED => OidcError::Client,
            Ok(e) => OidcError::Refused(match e.error_description {
                Some(d) => format!("{}: {}", e.error, short(&d)),
                None => e.error,
            }),
            Err(_) => OidcError::Status(status.as_u16(), discovery.token_endpoint.clone()),
        });
    }
    serde_json::from_slice(&body)
        .map_err(|_| OidcError::Unreadable(discovery.token_endpoint.clone()))
}

/// A provider's text, cut short and without control characters, for
/// logs and the audit log.
fn short(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// What an ID token must say.
pub struct Expected<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    /// Seconds since the epoch.
    pub now: i64,
}

fn token(reason: &str) -> OidcError {
    OidcError::Token(reason.to_owned())
}

/// Checks the claims of a verified ID token (OpenID Connect Core 3.1.3.7).
pub fn check_claims(claims: &Map<String, Value>, expected: &Expected<'_>) -> Result<(), OidcError> {
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err(token("it was issued by another issuer"));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(aud)) => vec![aud.as_str()],
        Some(Value::Array(auds)) => auds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(token("it is meant for another client"));
    }
    let azp = claims.get("azp").and_then(Value::as_str);
    if (audiences.len() > 1 || azp.is_some()) && azp != Some(expected.client_id) {
        return Err(token("it was issued to another client"));
    }
    let Some(exp) = claims.get("exp").and_then(Value::as_i64) else {
        return Err(token("it has no expiry"));
    };
    if exp + LEEWAY_S < expected.now {
        return Err(token("it has expired; check the central server's clock"));
    }
    match claims.get("iat").and_then(Value::as_i64) {
        Some(iat) if iat - LEEWAY_S <= expected.now => {}
        Some(_) => return Err(token("it was issued in the future; check the clocks")),
        None => return Err(token("it has no issue time")),
    }
    let nonce = claims.get("nonce").and_then(Value::as_str);
    if nonce != Some(expected.nonce) {
        return Err(token("its nonce does not match this sign-in"));
    }
    if claims
        .get("sub")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(token("it names nobody"));
    }
    Ok(())
}

/// Verifies `id_token` with the provider's keys, fetching them again once
/// for a key it does not know yet (rotation).
pub async fn verify(
    cache: &Cache,
    id: Uuid,
    http: &reqwest::Client,
    settings: &OidcSettings,
    entry: &EntryView,
    id_token: &str,
) -> Result<jws::Verified, OidcError> {
    match jws::verify(id_token, &entry.keys) {
        Err(JwsError::UnknownKey) => {}
        other => return other.map_err(|e| token(&e.to_string())),
    }
    let Some(fresh) = cache.refetch(id, http, settings).await? else {
        return Err(token(&JwsError::UnknownKey.to_string()));
    };
    jws::verify(id_token, &fresh.keys).map_err(|e| token(&e.to_string()))
}

/// Who signed in: the account's identity at the provider, its name and
/// its groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub username: String,
    pub groups: Vec<String>,
}

/// The groups of `claim` in `claims`: an array of names, or one name.
pub fn groups_of(claims: &Map<String, Value>, claim: &str) -> Option<Vec<String>> {
    match claims.get(claim)? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        Value::String(one) => Some(vec![one.clone()]),
        _ => None,
    }
}

/// The username of the claims: `preferred_username`, else `email`, else
/// `sub`, the first that is a valid username.
pub fn username_of(claims: &Map<String, Value>) -> Option<String> {
    ["preferred_username", "email", "sub"]
        .iter()
        .filter_map(|c| claims.get(*c).and_then(Value::as_str))
        .map(str::trim)
        .find(|name| crate::api::users::validate_username(name).is_ok())
        .map(str::to_owned)
}

/// The identity of verified claims, with groups from userinfo when the ID
/// token does not carry them.
pub async fn identity(
    http: &reqwest::Client,
    discovery: &Discovery,
    settings: &OidcSettings,
    claims: &Map<String, Value>,
    access_token: Option<&str>,
) -> Result<Identity, OidcError> {
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut groups = groups_of(claims, &settings.groups_claim);
    let mut username = username_of(claims);
    if groups.is_none() {
        if let (Some(url), Some(token)) = (&discovery.userinfo_endpoint, access_token) {
            let info: Map<String, Value> = get_json(http, url, Some(token)).await?;
            // OpenID Connect Core 5.3.2: userinfo of another sub is not used.
            if info.get("sub").and_then(Value::as_str) == Some(subject.as_str()) {
                groups = groups_of(&info, &settings.groups_claim);
                username = username.or_else(|| username_of(&info));
            }
        }
    }
    let username = username.ok_or_else(|| token("it has no name MonitorFlow can use"))?;
    Ok(Identity {
        subject,
        username,
        groups: groups.unwrap_or_default(),
    })
}

/// What a passed connection test found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Found {
    /// The issuer the provider names.
    pub issuer: String,
    /// Signing keys MonitorFlow can use.
    pub keys: usize,
    /// Whether the client was checked with its secret (false for a public
    /// client).
    pub client_checked: bool,
}

/// Checks a provider without signing anyone in: discovery, keys, and the
/// client by trading a code that does not exist.
pub async fn test(
    settings: &OidcSettings,
    secret: Option<&Secret>,
    redirect_uri: &str,
) -> Result<Found, OidcError> {
    let http = http(settings)?;
    let discovery = discover(&http, settings).await?;
    let keys = keys(&http, &discovery).await?;
    if keys.usable() == 0 {
        return Err(OidcError::NoKey);
    }
    let (status, body) = token_request(
        &http,
        &discovery,
        settings,
        secret,
        TEST_CODE,
        &random(),
        redirect_uri,
    )
    .await?;
    let error = serde_json::from_slice::<TokenError>(&body)
        .map(|e| e.error)
        .unwrap_or_default();
    if status == StatusCode::UNAUTHORIZED
        || error == "invalid_client"
        || error == "unauthorized_client"
    {
        return Err(OidcError::Client);
    }
    Ok(Found {
        issuer: discovery.issuer,
        keys: keys.usable(),
        client_checked: secret.is_some(),
    })
}

#[cfg(test)]
mod tests;
