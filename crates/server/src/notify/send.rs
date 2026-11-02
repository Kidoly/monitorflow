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
use monitorflow_common::redact::Secret;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
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
    SendError(format!(
        "{host} is a private address. Only a global administrator can set a channel to send there."
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
        .map_err(|error| SendError(format!("Could not find {host}: {error}")))?;
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
            // The resolver never sees an address written in the URL.
            let literal = reqwest::Url::parse(&webhook.url).ok().and_then(|url| {
                url.host_str()?
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .ok()
            });
            if let Some(ip) = literal.filter(|ip| channel.public_only && !is_public(*ip)) {
                return Err(private_address(&ip.to_string()));
            }
            post_webhook(http, webhook, secret, message).await
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
/// otherwise (a local relay on port 25). Connects to `connect_to` when
/// set, still checking the certificate against the host's name.
fn transport(
    email: &Email,
    connect_to: Option<String>,
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
    transport(email, connect_to, password)?
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
    fn error_chains_keep_each_cause_once() {
        let inner = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let outer = std::io::Error::other(inner);
        assert_eq!(chain(&outer), "refused");
    }
}
