//! Delivery to one channel, with failures in plain words: the error the
//! "Send test" button shows and the channel list keeps.

use std::error::Error as StdError;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use monitorflow_common::redact::Secret;
use ring::hmac;

use super::message::Message;
use super::{Channel, Destination, Email, Webhook};

/// How long one delivery may take.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// Port of SMTP over TLS from the first byte; others use STARTTLS.
pub const SMTPS_PORT: u16 = 465;

/// Headers of webhook requests (ADR-0060).
pub const EVENT_HEADER: &str = "x-monitorflow-event";
pub const DELIVERY_HEADER: &str = "x-monitorflow-delivery";
pub const TIMESTAMP_HEADER: &str = "x-monitorflow-timestamp";
pub const SIGNATURE_HEADER: &str = "x-monitorflow-signature";

/// Why a delivery failed, as a sentence for the person who set it up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SendError(pub String);

/// `error` and its sources, each once.
fn chain(error: &dyn StdError) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let line = cause.to_string();
        if !text.contains(&line) {
            text.push_str(": ");
            text.push_str(&line);
        }
        source = cause.source();
    }
    text
}

/// The client for webhooks: no redirects, so a request never lands
/// somewhere the administrator did not type.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("MonitorFlow/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// Delivers `message` to `channel`.
pub async fn deliver(
    channel: &Channel,
    message: &Message,
    http: &reqwest::Client,
) -> Result<(), SendError> {
    match &channel.destination {
        Destination::Email(email) => send_email(email, channel.secret.as_ref(), message).await,
        Destination::Webhook(webhook) => {
            post_webhook(http, webhook, channel.secret.as_ref(), message).await
        }
    }
}

/// `sha256=` and the hex HMAC-SHA256 of `{timestamp}.{body}` under
/// `secret`: what the signature header carries.
pub fn signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut context = hmac::Context::with_key(&key);
    context.update(timestamp.to_string().as_bytes());
    context.update(b".");
    context.update(body);
    let tag = context.sign();
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

async fn post_webhook(
    http: &reqwest::Client,
    webhook: &Webhook,
    secret: Option<&Secret>,
    message: &Message,
) -> Result<(), SendError> {
    let body = serde_json::to_vec(&message.payload).unwrap_or_default();
    let timestamp = chrono::Utc::now().timestamp();
    let mut request = http
        .post(&webhook.url)
        .header("content-type", "application/json")
        .header(TIMESTAMP_HEADER, timestamp.to_string());
    for (header, field) in [(EVENT_HEADER, "event"), (DELIVERY_HEADER, "id")] {
        if let Some(value) = message.payload.get(field).and_then(|v| v.as_str()) {
            request = request.header(header, value);
        }
    }
    if let Some(secret) = secret {
        request = request.header(
            SIGNATURE_HEADER,
            signature(secret.expose(), timestamp, &body),
        );
    }
    let host = reqwest::Url::parse(&webhook.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| webhook.url.clone());
    let response = request.body(body).send().await.map_err(|error| {
        SendError(if error.is_timeout() {
            format!(
                "No answer from {host} within {} seconds. Check the URL, and that this server can reach it.",
                TIMEOUT.as_secs()
            )
        } else if error.is_connect() {
            format!("Could not connect to {host}: {}", chain(&error))
        } else {
            format!("The request to {host} failed: {}", chain(&error))
        })
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    let text: String = text.trim().chars().take(200).collect();
    Err(SendError(if status.is_redirection() {
        format!("{host} answered {status}, a redirect, which is not followed: enter the final URL.")
    } else if text.is_empty() {
        format!("{host} answered {status}.")
    } else {
        format!("{host} answered {status}: {text}")
    }))
}

/// The SMTP transport for `email`: TLS from the start on port 465,
/// STARTTLS required when there is a password to protect, opportunistic
/// otherwise (a local relay on port 25).
fn transport(
    email: &Email,
    password: Option<&Secret>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
    let tls = TlsParameters::new(email.host.clone()).map_err(|error| {
        SendError(format!(
            "Cannot use TLS with {}: {}",
            email.host,
            chain(&error)
        ))
    })?;
    let tls = if email.port == SMTPS_PORT {
        Tls::Wrapper(tls)
    } else if password.is_some() {
        Tls::Required(tls)
    } else {
        Tls::Opportunistic(tls)
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(email.host.clone())
        .port(email.port)
        .tls(tls)
        .timeout(Some(TIMEOUT));
    if let (false, Some(password)) = (email.username.is_empty(), password) {
        builder = builder.credentials(Credentials::new(
            email.username.clone(),
            password.expose().to_owned(),
        ));
    }
    Ok(builder.build())
}

async fn send_email(
    email: &Email,
    password: Option<&Secret>,
    message: &Message,
) -> Result<(), SendError> {
    let from = if email.from.is_empty() {
        &email.username
    } else {
        &email.from
    };
    let mailbox = |address: &str| {
        address
            .parse::<Mailbox>()
            .map_err(|_| SendError(format!("`{address}` is not an email address.")))
    };
    let mut builder = lettre::Message::builder()
        .from(mailbox(from)?)
        .subject(message.subject.clone())
        .header(ContentType::TEXT_PLAIN);
    for to in &email.to {
        builder = builder.to(mailbox(to)?);
    }
    let mail = builder
        .body(message.body.clone())
        .map_err(|error| SendError(format!("The email could not be built: {error}")))?;
    transport(email, password)?
        .send(mail)
        .await
        .map(drop)
        .map_err(|error| SendError(smtp_error(email, &error)))
}

/// An SMTP failure in plain words.
fn smtp_error(email: &Email, error: &lettre::transport::smtp::Error) -> String {
    let server = format!("{}:{}", email.host, email.port);
    let detail = chain(error);
    let code = error.status().map(|c| c.to_string()).unwrap_or_default();
    if error.is_timeout() {
        format!(
            "No answer from {server} within {} seconds. Check the server and port, and that this server can reach it.",
            TIMEOUT.as_secs()
        )
    } else if error.is_tls() {
        format!("The secure connection to {server} failed ({detail}). Port 465 expects TLS from the start, 587 STARTTLS.")
    } else if ["530", "534", "535"].contains(&code.as_str()) {
        format!("{server} refused the username or password ({detail}).")
    } else if error.is_permanent() || error.is_transient() {
        format!("{server} refused the email ({detail}).")
    } else if detail.to_ascii_lowercase().contains("starttls") {
        format!("{server} does not offer STARTTLS, which a password needs; use port 465 if it speaks TLS ({detail}).")
    } else {
        format!("Could not send through {server}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_hmac_sha256_of_the_timestamp_and_body() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"s3cret");
        let tag = hmac::sign(&key, b"1700000000.{\"a\":1}");
        let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let signed = signature("s3cret", 1_700_000_000, b"{\"a\":1}");
        assert_eq!(signed, format!("sha256={hex}"));
        assert_eq!(signed.len(), "sha256=".len() + 64);
        assert_ne!(signature("s3cret", 1_700_000_001, b"{\"a\":1}"), signed);
        assert_ne!(signature("other", 1_700_000_000, b"{\"a\":1}"), signed);
    }

    #[test]
    fn error_chains_keep_each_cause_once() {
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let outer = std::io::Error::other(inner);
        assert_eq!(chain(&outer), "refused");
    }
}
