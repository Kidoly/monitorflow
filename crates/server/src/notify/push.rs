//! Web Push to people's phones (ADR-0186): the standard protocol every
//! browser speaks (RFC 8030), with messages encrypted to the phone
//! (RFC 8291) and signed with this server's own VAPID key (RFC 8292). No
//! service of ours sits in between: the server posts to the push service
//! the phone's browser chose (Apple's, Google's, Mozilla's), which needs
//! outbound HTTPS to it.
//!
//! What a message carries is little: a line of text, where to open, and
//! the incident to act on. No link that acknowledges by itself and no
//! secret: actions from the notification go through the phone's own
//! session.

use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use ring::{aead, agreement, hkdf};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::message::Message;
use super::send::{error_body, request_error, response_error, SendError};
use crate::secrets::{self, Column};

/// The record size written in the header: one record holds any message.
const RECORD_SIZE: u32 = 4096;
/// Longest text of a notification, in characters; the rest is on its page.
pub const MAX_BODY: usize = 240;
/// Most notifications one person's phone gets in a pass; the rest become
/// one "and N more" notification.
pub const MAX_PER_PASS: usize = 3;
/// How long a push service keeps a message for a phone that is off. An
/// alert older than this is better read in the app.
pub const TTL: Duration = Duration::from_secs(4 * 3600);
/// How long a VAPID token is valid; push services take at most 24 hours.
const TOKEN_LIFETIME: i64 = 12 * 3600;
/// Longest endpoint a browser may give.
pub const MAX_ENDPOINT: usize = 2048;

/// Why a push could not be sent or set up.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("the phone's keys are not valid")]
    BadKeys,
    #[error("could not encrypt or sign: {0}")]
    Crypto(&'static str),
    #[error(transparent)]
    Secrets(#[from] secrets::SecretsError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl From<ring::error::Unspecified> for PushError {
    fn from(_: ring::error::Unspecified) -> Self {
        Self::Crypto("unspecified")
    }
}

/// A browser's push subscription, as `PushSubscription.toJSON()` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: String,
    /// The phone's P-256 public key, uncompressed (65 bytes).
    pub p256dh: Vec<u8>,
    /// The phone's authentication secret (16 bytes).
    pub auth: Vec<u8>,
}

impl Subscription {
    /// Checks a subscription from a browser: an `https` endpoint that is
    /// not an address of a private network, and keys of the right sizes,
    /// in base64url. The error is a sentence for the form.
    pub fn parse(endpoint: &str, p256dh: &str, auth: &str) -> Result<Self, String> {
        let endpoint = endpoint.trim();
        if endpoint.len() > MAX_ENDPOINT {
            return Err("That push address is too long.".into());
        }
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| "The browser gave a push address that is not a URL.".to_owned())?;
        if url.scheme() != "https" {
            return Err("A push address must use https.".into());
        }
        let host = url.host_str().unwrap_or_default();
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        let public = match literal.parse::<std::net::IpAddr>() {
            Ok(ip) => super::send::is_public(ip),
            Err(_) => !host.is_empty() && host != "localhost" && host.contains('.'),
        };
        if !public {
            return Err("A push address must be on the internet.".into());
        }
        let decode = |text: &str| URL_SAFE_NO_PAD.decode(text.trim().trim_end_matches('='));
        let p256dh = decode(p256dh).map_err(|_| "The phone's key is not base64url.".to_owned())?;
        let auth = decode(auth).map_err(|_| "The phone's secret is not base64url.".to_owned())?;
        if p256dh.len() != 65 || p256dh[0] != 4 {
            return Err("The phone's key is not a P-256 public key.".into());
        }
        if auth.len() != 16 {
            return Err("The phone's secret is not 16 bytes.".into());
        }
        Ok(Self {
            endpoint: endpoint.to_owned(),
            p256dh,
            auth,
        })
    }

    /// The push service's host, for messages and logs: the endpoint's path
    /// names the phone.
    pub fn host(&self) -> String {
        reqwest::Url::parse(&self.endpoint)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// HKDF output of `len` bytes.
struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Result<Vec<u8>, PushError> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let mut out = vec![0; len];
    prk.expand(&[info], Len(len))?.fill(&mut out)?;
    Ok(out)
}

/// The content encryption key and nonce of RFC 8291 (section 3.4) from
/// the shared ECDH secret.
fn keys(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), PushError> {
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = hkdf(auth, ecdh_secret, &key_info, 32)?;
    let cek = hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16)?;
    let nonce = hkdf(salt, &ikm, b"Content-Encoding: nonce\0", 12)?;
    Ok((cek, nonce))
}

/// The `aes128gcm` body (RFC 8188) of one record for `plaintext`, from
/// the shared secret and the keys of both ends.
fn seal(
    ecdh_secret: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, PushError> {
    let (cek, nonce) = keys(ecdh_secret, auth, ua_public, as_public, salt)?;
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek)?);
    let nonce = aead::Nonce::try_assume_unique_for_key(&nonce)?;
    let mut record = plaintext.to_vec();
    // The last record's padding delimiter.
    record.push(2);
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record)?;
    let mut body = Vec::with_capacity(86 + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(u8::try_from(as_public.len()).map_err(|_| PushError::BadKeys)?);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// `plaintext` encrypted to `subscription` with a new key pair and salt.
pub fn encrypt(subscription: &Subscription, plaintext: &[u8]) -> Result<Vec<u8>, PushError> {
    let rng = SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)?;
    let public = private.compute_public_key()?;
    let salt: [u8; 16] = ring::rand::generate(&rng)?.expose();
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &subscription.p256dh);
    let secret = agreement::agree_ephemeral(private, &peer, |s| s.to_vec())
        .map_err(|_| PushError::BadKeys)?;
    seal(
        &secret,
        &subscription.auth,
        &subscription.p256dh,
        public.as_ref(),
        &salt,
        plaintext,
    )
}

/// The server's VAPID key pair: push services tie each subscription to
/// its public key, so only this server can send to it.
pub struct Vapid {
    pair: EcdsaKeyPair,
}

impl std::fmt::Debug for Vapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid")
            .field("public", &self.public_base64())
            .finish()
    }
}

impl Vapid {
    fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, PushError> {
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|_| PushError::Crypto("the stored VAPID key does not load"))?;
        Ok(Self { pair })
    }

    /// A new key pair, with its PKCS#8 document to store.
    fn generate() -> Result<(Self, Vec<u8>), PushError> {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())?;
        let pkcs8 = pkcs8.as_ref().to_vec();
        Ok((Self::from_pkcs8(&pkcs8)?, pkcs8))
    }

    /// The public key, uncompressed (65 bytes).
    pub fn public(&self) -> &[u8] {
        self.pair.public_key().as_ref()
    }

    /// The public key as a browser takes it (`applicationServerKey`).
    pub fn public_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public())
    }

    /// The `Authorization` header for a push to `endpoint` (RFC 8292):
    /// a token for the endpoint's origin, signed with ES256, valid from
    /// `now` (unix seconds) for [`TOKEN_LIFETIME`]. `subject` says who
    /// sends: the web interface's https address, or a mailto: URL.
    pub fn authorization(
        &self,
        endpoint: &str,
        subject: &str,
        now: i64,
    ) -> Result<String, PushError> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| PushError::Crypto("bad endpoint"))?;
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = json!({
            "aud": url.origin().ascii_serialization(),
            "exp": now + TOKEN_LIFETIME,
            "sub": subject,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing = format!("{header}.{claims}");
        let signature = self.pair.sign(&SystemRandom::new(), signing.as_bytes())?;
        Ok(format!(
            "vapid t={signing}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signature.as_ref()),
            self.public_base64()
        ))
    }
}

/// The server's VAPID key pair, made and stored on first use. Every
/// central server of a cluster reads the same row.
pub async fn vapid(pool: &PgPool) -> Result<Vapid, PushError> {
    if let Some(vapid) = stored_vapid(pool).await? {
        return Ok(vapid);
    }
    let (vapid, pkcs8) = Vapid::generate()?;
    let sealed = secrets::seal(Column::WebPushKeys, &STANDARD.encode(&pkcs8))?;
    sqlx::query(
        "INSERT INTO web_push_keys (id, public_key, private_key) VALUES (1, $1, $2)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(vapid.public())
    .bind(sealed)
    .execute(pool)
    .await?;
    // Another central server may have stored its own first.
    Ok(stored_vapid(pool).await?.unwrap_or(vapid))
}

async fn stored_vapid(pool: &PgPool) -> Result<Option<Vapid>, PushError> {
    let Some(sealed) =
        sqlx::query_scalar::<_, String>("SELECT private_key FROM web_push_keys WHERE id = 1")
            .fetch_optional(pool)
            .await?
    else {
        return Ok(None);
    };
    let opened = secrets::open(Column::WebPushKeys, &sealed)?;
    let pkcs8 = STANDARD
        .decode(opened.expose())
        .map_err(|_| PushError::Crypto("the stored VAPID key is not base64"))?;
    Vapid::from_pkcs8(&pkcs8).map(Some)
}

/// What one notification says, before encryption.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub body: String,
    /// The page of the web app it opens, a path.
    pub path: String,
    /// Replaces an earlier notification with the same tag on the phone.
    pub tag: String,
    /// The one incident it is about, if only one: the actions act on it.
    pub incident: Option<Uuid>,
    /// Whether the person may acknowledge that incident.
    pub can_acknowledge: bool,
    /// An alert still open: the push service delivers it at once.
    pub urgent: bool,
}

impl Notice {
    /// The notification for `message`, told to a person who may
    /// acknowledge `acknowledgeable` of `incidents`, its incidents still a
    /// problem.
    pub fn from_message(message: &Message, incidents: &[Uuid], acknowledgeable: &[Uuid]) -> Self {
        let payload = &message.payload;
        let group = payload["group"]["id"].as_str();
        let incident = match incidents {
            [one] if group.is_none() => Some(*one),
            _ => None,
        };
        let about = payload["incident"]["id"]
            .as_str()
            .and_then(|i| i.parse().ok());
        let (path, tag) = match (group, incident.or(about)) {
            (Some(group), _) => (
                format!("{}?group={group}", MOBILE),
                format!("group-{group}"),
            ),
            (None, Some(id)) => (
                format!("{MOBILE}incident/?id={id}"),
                format!("incident-{id}"),
            ),
            (None, None) => (
                MOBILE.to_owned(),
                format!("event-{}", payload["id"].as_str().unwrap_or_default()),
            ),
        };
        Self {
            body: shorten(&message.text),
            path,
            tag,
            incident,
            can_acknowledge: incident.is_some_and(|i| acknowledgeable.contains(&i)),
            urgent: !incidents.is_empty(),
        }
    }

    /// The one notification for messages beyond [`MAX_PER_PASS`].
    pub fn more(count: usize) -> Self {
        Self {
            body: if count == 1 {
                "1 more notification. Open MonitorFlow to see it.".into()
            } else {
                format!("{count} more notifications. Open MonitorFlow to see them.")
            },
            path: MOBILE.into(),
            tag: "more".into(),
            incident: None,
            can_acknowledge: false,
            urgent: false,
        }
    }

    /// What "Send test" shows.
    pub fn test() -> Self {
        Self {
            body: "Test notification from MonitorFlow. Alerts will appear like this.".into(),
            path: format!("{MOBILE}phone/"),
            tag: "test".into(),
            incident: None,
            can_acknowledge: false,
            urgent: false,
        }
    }

    /// The JSON the service worker reads.
    pub fn json(&self) -> Value {
        let mut actions = Vec::new();
        if self.incident.is_some() {
            if self.can_acknowledge {
                actions.push("acknowledge");
            }
            actions.push("silence");
        }
        json!({
            "v": 1,
            "title": "MonitorFlow",
            "body": self.body,
            "path": self.path,
            "tag": self.tag,
            "incident": self.incident,
            "actions": actions,
        })
    }
}

/// The web app's pages for phones.
pub const MOBILE: &str = "/mobile/";

fn shorten(text: &str) -> String {
    if text.chars().count() <= MAX_BODY {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_BODY - 1).collect();
    out.push('…');
    out
}

/// The notices for one person's messages of a pass: at most
/// [`MAX_PER_PASS`], the last one counting the rest.
pub fn limit(mut notices: Vec<Notice>) -> Vec<Notice> {
    if notices.len() > MAX_PER_PASS {
        let rest = notices.len() - (MAX_PER_PASS - 1);
        notices.truncate(MAX_PER_PASS - 1);
        notices.push(Notice::more(rest));
    }
    notices
}

/// How a push ended.
#[derive(Debug)]
pub enum Outcome {
    Sent,
    /// The push service no longer knows the subscription: the person
    /// unsubscribed, reinstalled or cleared the app's data.
    Gone,
    Failed(SendError),
}

/// Sends `notice` to `subscription` once, with `client`.
pub async fn send(
    client: &reqwest::Client,
    vapid: &Vapid,
    subject: &str,
    subscription: &Subscription,
    notice: &Notice,
) -> Outcome {
    let host = subscription.host();
    let body = match encrypt(subscription, notice.json().to_string().as_bytes()) {
        Ok(body) => body,
        Err(error) => return Outcome::Failed(SendError::new(error.to_string())),
    };
    let now = chrono::Utc::now().timestamp();
    let authorization = match vapid.authorization(&subscription.endpoint, subject, now) {
        Ok(header) => header,
        Err(error) => return Outcome::Failed(SendError::new(error.to_string())),
    };
    let response = client
        .post(&subscription.endpoint)
        .header("TTL", TTL.as_secs())
        .header("Urgency", if notice.urgent { "high" } else { "normal" })
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("Authorization", authorization)
        .body(body)
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => return Outcome::Failed(request_error(&host, error)),
    };
    let status = response.status();
    if status.is_success() {
        return Outcome::Sent;
    }
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Outcome::Gone;
    }
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let text = error_body(response).await;
    let mut error = response_error(&host, status, &text);
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        error.retry_after = Some(Duration::from_secs(retry_after.unwrap_or(2).min(30)));
    }
    Outcome::Failed(error)
}

/// A phone with push turned on.
#[derive(Debug, Clone)]
pub struct Phone {
    pub id: Uuid,
    pub subscription: Subscription,
}

/// The phones of `user` that take push.
pub async fn phones(pool: &PgPool, user: Uuid) -> Result<Vec<Phone>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, push_endpoint, push_p256dh, push_auth FROM phones
         WHERE user_id = $1 AND push_endpoint IS NOT NULL ORDER BY created_at, id",
    )
    .bind(user)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Phone {
                id: row.try_get("id")?,
                subscription: Subscription {
                    endpoint: row.try_get("push_endpoint")?,
                    p256dh: row.try_get("push_p256dh")?,
                    auth: row.try_get("push_auth")?,
                },
            })
        })
        .collect()
}

/// What happened to one phone, kept on its row and shown in its list.
pub async fn record(pool: &PgPool, phone: Uuid, outcome: &Outcome) -> Result<(), sqlx::Error> {
    match outcome {
        Outcome::Sent => {
            sqlx::query("UPDATE phones SET push_sent_at = now(), push_error = NULL WHERE id = $1")
                .bind(phone)
                .execute(pool)
                .await?;
        }
        Outcome::Gone => {
            sqlx::query(
                "UPDATE phones SET push_endpoint = NULL, push_p256dh = NULL, push_auth = NULL,
                        push_error = $2
                 WHERE id = $1",
            )
            .bind(phone)
            .bind(GONE)
            .execute(pool)
            .await?;
        }
        Outcome::Failed(error) => {
            sqlx::query("UPDATE phones SET push_error = $2 WHERE id = $1")
                .bind(phone)
                .bind(&error.message)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

/// What a phone's list says once its push service forgot it.
pub const GONE: &str =
    "The push service no longer knows this phone. Open MonitorFlow on it and turn notifications on again.";

/// Sends `notices` to every phone of `user`, retrying a busy push service
/// after `retries`. Returns how many phones got them all.
pub async fn deliver(
    pool: &PgPool,
    client: &reqwest::Client,
    subject: &str,
    user: Uuid,
    notices: &[Notice],
    retries: &[Duration],
) -> Result<usize, PushError> {
    let phones = phones(pool, user).await?;
    if phones.is_empty() || notices.is_empty() {
        return Ok(0);
    }
    let vapid = vapid(pool).await?;
    let mut reached = 0;
    for phone in &phones {
        let mut last = Outcome::Sent;
        for notice in notices {
            last = send(client, &vapid, subject, &phone.subscription, notice).await;
            for wait in retries {
                match &last {
                    Outcome::Failed(SendError {
                        retry_after: Some(asked),
                        ..
                    }) => tokio::time::sleep((*asked).max(*wait)).await,
                    _ => break,
                }
                last = send(client, &vapid, subject, &phone.subscription, notice).await;
            }
            if !matches!(last, Outcome::Sent) {
                break;
            }
        }
        match &last {
            Outcome::Sent => reached += 1,
            Outcome::Gone => {
                tracing::info!(phone = %phone.id, host = %phone.subscription.host(), "push subscription ended")
            }
            Outcome::Failed(error) => {
                tracing::warn!(phone = %phone.id, host = %phone.subscription.host(), %error, "push not delivered")
            }
        }
        if let Err(error) = record(pool, phone.id, &last).await {
            tracing::warn!(phone = %phone.id, %error, "could not record a push outcome");
        }
    }
    Ok(reached)
}

/// The VAPID subject for `web_url`: the web interface's address when it
/// is https, as push services ask, else a placeholder they accept.
pub fn subject(web_url: &str) -> String {
    match reqwest::Url::parse(web_url) {
        Ok(url) if url.scheme() == "https" => url.origin().ascii_serialization(),
        _ => "mailto:push@monitorflow.invalid".into(),
    }
}

#[cfg(test)]
mod tests;
