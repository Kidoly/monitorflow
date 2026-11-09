//! Delivery to one channel, with failures in plain words: the error the
//! "Send test" button shows and the channel list keeps.

use std::error::Error as StdError;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use monitorflow_common::redact::{Secret, REDACTED};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use ring::hmac;

use super::chat::Service;
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

/// How long a chat service that asks to wait is waited for, at most.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Query values shorter than this are not treated as secrets.
const MIN_SECRET_VALUE: usize = 8;
/// The wait when a chat service limits messages without saying for how long.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);

/// Why a delivery failed, as a sentence for the person who set it up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SendError {
    pub message: String,
    /// A rate limit: how long the service asked to wait.
    pub retry_after: Option<Duration>,
}

impl SendError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry_after: None,
        }
    }
}

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

/// A request that got no answer, in plain words. The URL is taken out of
/// the error first: it can hold a token, and the text is shown and logged.
pub(crate) fn request_error(host: &str, error: reqwest::Error) -> SendError {
    let error = error.without_url();
    SendError::new(if error.is_timeout() {
        format!(
            "No answer from {host} within {} seconds. Check the URL, and that this server can reach it.",
            TIMEOUT.as_secs()
        )
    } else if error.is_connect() {
        format!("Could not connect to {host}: {}", chain(&error))
    } else {
        format!("The request to {host} failed: {}", chain(&error))
    })
}

/// A refusal in plain words, from the first characters of the answer.
pub(crate) fn response_error(host: &str, status: reqwest::StatusCode, text: &str) -> SendError {
    let text: String = text.trim().chars().take(200).collect();
    SendError::new(if status.is_redirection() {
        format!("{host} answered {status}, a redirect, which is not followed: enter the final URL.")
    } else if text.is_empty() {
        format!("{host} answered {status}.")
    } else {
        format!("{host} answered {status}: {text}")
    })
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

/// Whether `ip` is on the public Internet. Everything else is the central
/// server's own side: loopback, private and link-local ranges, cloud
/// metadata, shared address space, and the IPv6 forms that wrap an IPv4
/// address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 88 && c == 99))
        }
        IpAddr::V6(ip) => {
            let [first, second, ..] = ip.segments();
            // Global unicast is 2000::/3; 2001::/32 (Teredo), 2001:db8::/32
            // (documentation) and 2002::/16 (6to4) are not destinations.
            first & 0xe000 == 0x2000
                && first != 0x2002
                && !(first == 0x2001 && (second == 0 || second == 0xdb8))
        }
    }
}

/// The sentence for a destination that is not public.
fn private_address(host: &str) -> SendError {
    SendError::new(format!(
        "{host} is a private address, which only a global administrator can allow."
    ))
}

/// Resolves names to their public addresses only. The connection uses
/// what this returns, so a name that changes after a check cannot slip
/// through.
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses = public_addresses(name.as_str(), 0).await?;
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// The public addresses of `host`; an error when it has none.
async fn public_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, SendError> {
    let found = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| SendError::new(format!("Could not find {host}: {error}")))?;
    let public: Vec<SocketAddr> = found.filter(|a| is_public(a.ip())).collect();
    if public.is_empty() {
        return Err(private_address(host));
    }
    Ok(public)
}

/// [`http_client`] for destinations chosen by someone who does not control
/// the server: public addresses only, and no proxy, which would resolve
/// the name itself.
pub fn public_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("MonitorFlow/", env!("CARGO_PKG_VERSION")))
        .no_proxy()
        .dns_resolver(Arc::new(PublicResolver))
        .build()
        .unwrap_or_default()
}

/// Delivers `message` to `channel` with `http`, the client for its
/// [`Channel::public_only`].
pub async fn deliver(
    channel: &Channel,
    message: &Message,
    http: &reqwest::Client,
) -> Result<(), SendError> {
    let secret = channel.secret.as_ref();
    match &channel.destination {
        Destination::Email(email) => {
            let connect_to = if channel.public_only {
                let addresses = public_addresses(&email.host, email.port).await?;
                addresses.first().map(|a| a.ip().to_string())
            } else {
                None
            };
            send_email(email, connect_to, secret, message).await
        }
        Destination::Webhook(webhook) => {
            refuse_private_literal(channel, &webhook.url)?;
            post_webhook(http, webhook, secret, message).await
        }
        Destination::Slack(_) | Destination::Teams(_) | Destination::Discord(_) => {
            // Saving refuses addresses in chat URLs; a row written another
            // way still meets the rule of its channel.
            if let Some(url) = secret {
                refuse_private_literal(channel, url.expose())?;
            }
            post_chat(http, channel, message).await
        }
        // They need the plugin from the database: the dispatcher sends
        // them through `plugins::channel`.
        Destination::Plugin(_) => Err(SendError::new("a plugin channel is sent by its plugin")),
    }
}

/// The resolver never sees an address written in a URL, so a channel
/// limited to public addresses checks it here.
fn refuse_private_literal(channel: &Channel, url: &str) -> Result<(), SendError> {
    match channel.public_only {
        true => refuse_private_address_in(url),
        false => Ok(()),
    }
}

/// Refuses a URL whose host is a private address written out, which
/// [`public_http_client`]'s resolver never sees.
pub(crate) fn refuse_private_address_in(url: &str) -> Result<(), SendError> {
    let literal = reqwest::Url::parse(url).ok().and_then(|url| {
        url.host_str()?
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .ok()
    });
    match literal.filter(|ip| !is_public(*ip)) {
        Some(ip) => Err(private_address(&ip.to_string())),
        None => Ok(()),
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
    let response = request
        .body(body)
        .send()
        .await
        .map_err(|error| request_error(&host, error))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    Err(response_error(&host, status, &text))
}

/// How long a rate limited answer asks to wait, within
/// [`MAX_RETRY_AFTER`]: the `Retry-After` header, or Discord's `retry_after`
/// in the body.
fn retry_after(header: Option<&str>, body: &str) -> Duration {
    header
        .and_then(|seconds| seconds.trim().parse::<f64>().ok())
        .or_else(|| {
            let body: serde_json::Value = serde_json::from_str(body).ok()?;
            body.get("retry_after")?.as_f64()
        })
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .unwrap_or(DEFAULT_RETRY_AFTER)
        .min(MAX_RETRY_AFTER)
}

/// Posts to a chat service. The channel's secret is the URL, so only its
/// host appears in errors, and an answer that repeats it is cut out.
async fn post_chat(
    http: &reqwest::Client,
    channel: &Channel,
    message: &Message,
) -> Result<(), SendError> {
    let Some((service, _)) = channel.destination.chat() else {
        return Err(SendError::new("This is not a chat channel."));
    };
    let url = channel
        .secret
        .as_ref()
        .and_then(|url| service.request_url(url.expose()))
        .ok_or_else(|| SendError::new("This channel has no webhook URL: edit it and paste one."))?;
    let host = url.host_str().unwrap_or_default().to_owned();
    let response = http
        .post(url.clone())
        .header("content-type", "application/json")
        .body(service.payload(message).to_string())
        .send()
        .await
        .map_err(|error| request_error(&host, error))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let header = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let text = response.text().await.unwrap_or_default();
    Err(chat_error(service, &url, status, header.as_deref(), &text))
}

/// A chat service's refusal in plain words.
fn chat_error(
    service: Service,
    url: &reqwest::Url,
    status: reqwest::StatusCode,
    retry_header: Option<&str>,
    text: &str,
) -> SendError {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait = retry_after(retry_header, text);
        return SendError {
            message: format!(
                "{} is limiting messages from this webhook; it asked to wait {} seconds.",
                service.name(),
                wait.as_secs_f64().ceil()
            ),
            retry_after: Some(wait),
        };
    }
    match service.explain(status.as_u16(), text) {
        Some(reason) => SendError::new(reason),
        None => {
            let host = url.host_str().unwrap_or_default();
            response_error(host, status, &without_url_secrets(text, url))
        }
    }
}

/// `text` without what makes `url` a credential, should an answer repeat
/// it: the path of Slack and Discord, the query values of Teams (`sig`).
/// Short values like `api-version=1` are not worth hiding.
fn without_url_secrets(text: &str, url: &reqwest::Url) -> String {
    let raw = url.query().unwrap_or_default();
    let raw = raw.split('&').filter_map(|pair| pair.split_once('='));
    let decoded = url.query_pairs();
    let values = raw
        .map(|(_, value)| value.to_owned())
        .chain(decoded.map(|(_, value)| value.into_owned()))
        .filter(|value| value.len() >= MIN_SECRET_VALUE);
    let mut text = text.replace(url.path(), REDACTED);
    for value in values {
        text = text.replace(&value, REDACTED);
    }
    text
}

/// The SMTP transport for `email`: TLS from the start on port 465,
/// STARTTLS required when there is a password to protect, opportunistic
/// otherwise (a local relay on port 25). Connects to `connect_to` when
/// set, still checking the certificate against the host's name.
fn transport(
    email: &Email,
    connect_to: Option<String>,
    password: Option<&Secret>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
    let tls = TlsParameters::new(email.host.clone()).map_err(|error| {
        SendError::new(format!(
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
    let server = connect_to.unwrap_or_else(|| email.host.clone());
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(server)
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
    connect_to: Option<String>,
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
            .map_err(|_| SendError::new(format!("`{address}` is not an email address.")))
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
        .map_err(|error| SendError::new(format!("The email could not be built: {error}")))?;
    transport(email, connect_to, password)?
        .send(mail)
        .await
        .map(drop)
        .map_err(|error| SendError::new(smtp_error(email, &error)))
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
    fn only_addresses_on_the_internet_are_public() {
        for ip in [
            "93.184.216.34",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:2800:220:1:248:1893:25c8:1946",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "192.0.0.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "2001:0:7f00:1::",
            "64:ff9b::7f00:1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_rate_limit_says_how_long_within_bounds() {
        assert_eq!(retry_after(Some("3"), ""), Duration::from_secs(3));
        assert_eq!(
            retry_after(None, r#"{"message":"limited","retry_after":0.5}"#),
            Duration::from_millis(500)
        );
        assert_eq!(retry_after(Some("86400"), ""), MAX_RETRY_AFTER);
        assert_eq!(retry_after(Some("soon"), "nope"), DEFAULT_RETRY_AFTER);
        assert_eq!(retry_after(Some("-1"), ""), DEFAULT_RETRY_AFTER);
    }

    #[test]
    fn chat_refusals_are_explained_and_keep_the_url_out() {
        let status = reqwest::StatusCode::from_u16;
        let slack = reqwest::Url::parse("https://hooks.slack.com/services/T0/B0/TOKEN").unwrap();
        let gone = chat_error(
            Service::Slack,
            &slack,
            status(404).unwrap(),
            None,
            "no_service",
        );
        assert!(gone.message.contains("no longer knows"), "{gone}");
        let echoed = chat_error(
            Service::Slack,
            &slack,
            status(500).unwrap(),
            None,
            "failed for /services/T0/B0/TOKEN",
        );
        assert!(!echoed.message.contains("TOKEN"), "{echoed}");
        assert!(echoed.message.starts_with("hooks.slack.com answered 500"));
        let teams = reqwest::Url::parse(
            "https://a.logic.azure.com/workflows/w/invoke?api-version=1&sp=%2Ftriggers%2Fmanual%2Frun&sig=SIGNATURE",
        )
        .unwrap();
        let echoed = chat_error(
            Service::Teams,
            &teams,
            status(500).unwrap(),
            None,
            "bad call to /x?api-version=1&sp=/triggers/manual/run&sp2=%2Ftriggers%2Fmanual%2Frun&sig=SIGNATURE",
        );
        assert!(!echoed.message.contains("SIGNATURE"), "{echoed}");
        assert!(!echoed.message.contains("manual"), "{echoed}");
        assert!(echoed.message.contains("api-version=1"), "{echoed}");
        let discord = reqwest::Url::parse("https://discord.com/api/webhooks/1/t").unwrap();
        let limited = chat_error(
            Service::Discord,
            &discord,
            status(429).unwrap(),
            Some("2"),
            "",
        );
        assert_eq!(limited.retry_after, Some(Duration::from_secs(2)));
        assert!(limited.message.contains("Discord is limiting"));
    }

    #[test]
    fn error_chains_keep_each_cause_once() {
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let outer = std::io::Error::other(inner);
        assert_eq!(chain(&outer), "refused");
    }
}
