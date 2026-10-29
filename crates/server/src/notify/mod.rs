//! Notifications (ADR-0058): email and webhook channels, and the outbox
//! triggers fill when incidents change, commands wait for approval or
//! updates fail. The [`dispatcher`] delivers each event to every channel
//! whose customer matches.

pub mod dispatcher;
pub mod message;
pub mod send;

use lettre::message::Mailbox;
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgExecutor, Row};
use uuid::Uuid;

pub use dispatcher::Dispatcher;
pub use message::Event;

/// Most recipients of one email channel.
pub const MAX_RECIPIENTS: usize = 20;
/// The usual submission port, with STARTTLS.
pub const DEFAULT_SMTP_PORT: u16 = 587;

/// Where an email channel sends, as stored and shown; the password is
/// apart, in [`Channel::secret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Empty without authentication.
    #[serde(default)]
    pub username: String,
    /// The sender; the username when empty and it is an address.
    #[serde(default)]
    pub from: String,
    pub to: Vec<String>,
}

fn default_port() -> u16 {
    DEFAULT_SMTP_PORT
}

/// Where a webhook channel posts; the signing secret is in
/// [`Channel::secret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    pub url: String,
}

/// The destination of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    Email(Email),
    Webhook(Webhook),
}

impl Destination {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Email(_) => "email",
            Self::Webhook(_) => "webhook",
        }
    }

    /// Trims the fields and checks them; the error is a sentence for the
    /// form.
    pub fn normalize(self) -> Result<Self, String> {
        match self {
            Self::Email(email) => normalize_email(email).map(Self::Email),
            Self::Webhook(webhook) => normalize_webhook(webhook).map(Self::Webhook),
        }
    }

    /// The settings column: the fields without the kind.
    fn settings(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.remove("kind");
        }
        value
    }

    fn from_row(kind: &str, mut settings: Value) -> Result<Self, serde_json::Error> {
        if let Some(object) = settings.as_object_mut() {
            object.insert("kind".into(), kind.into());
        }
        serde_json::from_value(settings)
    }
}

fn normalize_email(email: Email) -> Result<Email, String> {
    let host = email.host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("Enter the mail server, like smtp.example.com.".into());
    }
    if host.contains(|c: char| c.is_whitespace() || c == '/' || c == ':') {
        return Err(format!(
            "`{host}` is not a mail server name; enter the name alone and the port in its own field."
        ));
    }
    if email.port == 0 {
        return Err("Enter the mail server's port, usually 587.".into());
    }
    let username = email.username.trim().to_owned();
    let mut to = Vec::new();
    for address in email.to.iter().flat_map(|a| a.split([',', ';', '\n'])) {
        let address = address.trim();
        if address.is_empty() {
            continue;
        }
        address
            .parse::<Mailbox>()
            .map_err(|_| format!("`{address}` is not an email address."))?;
        if !to.iter().any(|a: &String| a.eq_ignore_ascii_case(address)) {
            to.push(address.to_owned());
        }
    }
    if to.is_empty() {
        return Err("Enter at least one address to send to.".into());
    }
    if to.len() > MAX_RECIPIENTS {
        return Err(format!(
            "At most {MAX_RECIPIENTS} addresses; use a mailing list for more."
        ));
    }
    let from = match email.from.trim() {
        "" if username.parse::<Mailbox>().is_ok() => String::new(),
        "" => {
            return Err(
                "Enter the sender address: the mail server's username is not an address.".into(),
            )
        }
        from => {
            from.parse::<Mailbox>()
                .map_err(|_| format!("`{from}` is not an email address."))?;
            from.to_owned()
        }
    };
    Ok(Email {
        host,
        port: email.port,
        username,
        from,
        to,
    })
}

fn normalize_webhook(webhook: Webhook) -> Result<Webhook, String> {
    let url = webhook.url.trim();
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| format!("`{url}` is not a URL; it starts with https://."))?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err(format!("`{url}` is not an http or https URL."));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Put no password in the URL; use the signing secret instead.".into());
    }
    Ok(Webhook {
        url: parsed.to_string(),
    })
}

/// A channel as the dispatcher uses it.
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: Uuid,
    pub destination: Destination,
    /// The SMTP password or the webhook signing secret.
    pub secret: Option<Secret>,
    /// Only this customer's events when set.
    pub customer_id: Option<Uuid>,
}

impl Channel {
    /// Whether an event about `customer` goes to this channel.
    pub fn wants(&self, customer: Option<Uuid>) -> bool {
        self.customer_id.is_none() || self.customer_id == customer
    }
}

const CHANNEL_COLUMNS: &str = "id, kind, settings, secret, customer_id";

fn channel(row: &sqlx::postgres::PgRow) -> Result<Channel, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    let destination = Destination::from_row(&kind, row.try_get("settings")?)
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(Channel {
        id: row.try_get("id")?,
        destination,
        secret: row.try_get::<Option<String>, _>("secret")?.map(Secret::new),
        customer_id: row.try_get("customer_id")?,
    })
}

/// Every channel.
pub async fn channels(db: impl PgExecutor<'_>) -> Result<Vec<Channel>, sqlx::Error> {
    sqlx::query(&format!(
        "SELECT {CHANNEL_COLUMNS} FROM notification_channels ORDER BY created_at, id"
    ))
    .fetch_all(db)
    .await?
    .iter()
    .map(channel)
    .collect()
}

/// One channel, `None` when it does not exist.
pub async fn load(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Channel>, sqlx::Error> {
    sqlx::query(&format!(
        "SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await?
    .as_ref()
    .map(channel)
    .transpose()
}

/// Inserts a channel and returns its ID.
pub async fn insert(
    db: impl PgExecutor<'_>,
    destination: &Destination,
    secret: Option<&Secret>,
    customer_id: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO notification_channels (kind, settings, secret, customer_id)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(destination.kind())
    .bind(destination.settings())
    .bind(secret.map(Secret::expose))
    .bind(customer_id)
    .fetch_one(db)
    .await
}

/// Replaces a channel's destination and customer; `secret` `None` keeps
/// the stored one. `false` when it does not exist.
pub async fn update(
    db: impl PgExecutor<'_>,
    id: Uuid,
    destination: &Destination,
    secret: Option<Option<&Secret>>,
    customer_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE notification_channels
         SET kind = $2, settings = $3, customer_id = $4,
             secret = CASE WHEN $5 THEN $6 ELSE secret END
         WHERE id = $1",
    )
    .bind(id)
    .bind(destination.kind())
    .bind(destination.settings())
    .bind(customer_id)
    .bind(secret.is_some())
    .bind(secret.flatten().map(Secret::expose))
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Records the outcome of a delivery or test: `None` for success.
pub async fn record_outcome(
    db: impl PgExecutor<'_>,
    id: Uuid,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE notification_channels
         SET last_sent_at = CASE WHEN $2::text IS NULL THEN now() ELSE last_sent_at END,
             last_error = $2,
             last_error_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(host: &str, username: &str, from: &str, to: &[&str]) -> Destination {
        Destination::Email(Email {
            host: host.into(),
            port: 587,
            username: username.into(),
            from: from.into(),
            to: to.iter().map(|&a| a.into()).collect(),
        })
    }

    #[test]
    fn emails_are_trimmed_and_split() {
        let normalized = email(
            " SMTP.Example.com. ",
            " ops@example.com ",
            "",
            &["a@example.com, b@example.com", "A@example.com"],
        )
        .normalize()
        .unwrap();
        assert_eq!(
            normalized,
            Destination::Email(Email {
                host: "smtp.example.com".into(),
                port: 587,
                username: "ops@example.com".into(),
                from: String::new(),
                to: vec!["a@example.com".into(), "b@example.com".into()],
            })
        );
    }

    #[test]
    fn email_mistakes_are_explained() {
        let cases = [
            (email("", "", "a@x.io", &["a@x.io"]), "mail server"),
            (email("smtp.x.io:587", "", "a@x.io", &["a@x.io"]), "port"),
            (email("smtp.x.io", "", "a@x.io", &[]), "at least one"),
            (email("smtp.x.io", "", "a@x.io", &["nope"]), "`nope`"),
            (email("smtp.x.io", "apikey", "", &["a@x.io"]), "sender"),
            (email("smtp.x.io", "", "bad", &["a@x.io"]), "`bad`"),
        ];
        for (destination, words) in cases {
            let error = destination.normalize().unwrap_err();
            assert!(error.contains(words), "{error}");
        }
        let many: Vec<String> = (0..=MAX_RECIPIENTS).map(|i| format!("u{i}@x.io")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(email("smtp.x.io", "", "a@x.io", &many).normalize().is_err());
    }

    #[test]
    fn webhooks_need_an_http_url_without_credentials() {
        let hook = |url: &str| Destination::Webhook(Webhook { url: url.into() }).normalize();
        assert_eq!(
            hook(" https://hooks.example/x?y=1 ").unwrap(),
            Destination::Webhook(Webhook {
                url: "https://hooks.example/x?y=1".into()
            })
        );
        assert!(hook("http://n8n.lan:5678/webhook/a").is_ok());
        assert!(hook("ftp://x").is_err());
        assert!(hook("hooks.example").is_err());
        assert!(hook("https://u:p@hooks.example/").is_err());
    }

    #[test]
    fn settings_round_trip_without_the_kind() {
        let destination = email("smtp.x.io", "", "a@x.io", &["b@x.io"]);
        let settings = destination.settings();
        assert!(settings.get("kind").is_none());
        assert_eq!(
            Destination::from_row("email", settings).unwrap(),
            destination
        );
    }

    #[test]
    fn unscoped_channels_take_every_customer() {
        let customer = Uuid::new_v4();
        let mut channel = Channel {
            id: Uuid::nil(),
            destination: Destination::Webhook(Webhook {
                url: "https://x/".into(),
            }),
            secret: None,
            customer_id: None,
        };
        assert!(channel.wants(Some(customer)) && channel.wants(None));
        channel.customer_id = Some(customer);
        assert!(channel.wants(Some(customer)));
        assert!(!channel.wants(Some(Uuid::new_v4())) && !channel.wants(None));
    }
}
