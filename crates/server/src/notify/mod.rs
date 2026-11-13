//! Notifications (ADR-0060): email, webhook and chat channels (ADR-0077),
//! and the outbox triggers fill when incidents change, commands wait for approval or
//! updates fail, [`backup`] when a backup fails, and the release check when
//! a new version appears. The [`dispatcher`] delivers each event to every
//! channel whose [`Coverage`] includes it (ADR-0078), unless a [`silence`]
//! holds it back (ADR-0079), and sends related incidents as one [`group`]
//! (ADR-0080). Whoever is [`on_call`] for the place hears of it in person
//! too, grouped the same way (ADR-0081). When nobody acknowledges a
//! critical incident, the next step of its [`follow_up`] is told (ADR-0082).

pub mod backup;
pub mod chat;
pub mod dispatcher;
pub mod follow_up;
pub mod group;
pub mod message;
pub mod on_call;
pub mod send;
pub mod silence;

use lettre::message::Mailbox;
use monitorflow_common::redact::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgExecutor, Row};
use uuid::Uuid;

use crate::secrets::{self, Column};

pub use chat::Service;
pub use dispatcher::Dispatcher;
pub use message::Event;

/// Most recipients of one email channel.
pub const MAX_RECIPIENTS: usize = 20;
/// Longest name of a chat channel.
pub const MAX_CHAT_NAME: usize = 80;
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

/// A Slack, Teams or Discord channel as stored and shown. Its webhook URL
/// is a credential, so it is in [`Channel::secret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    /// Tells channels of one service apart; may be empty.
    #[serde(default)]
    pub name: String,
    /// The webhook URL's host, set from the URL when it is saved.
    #[serde(default)]
    pub host: String,
}

/// A channel of a plugin (ADR-0103): which one, and its settings. Its
/// secret settings are in [`Channel::secret`], as a JSON object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginChannel {
    pub plugin: String,
    /// Tells channels of one plugin apart; may be empty.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, String>,
}

/// The destination of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    Email(Email),
    Webhook(Webhook),
    Slack(Chat),
    Teams(Chat),
    Discord(Chat),
    Plugin(PluginChannel),
}

impl Destination {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Email(_) => "email",
            Self::Webhook(_) => "webhook",
            Self::Slack(_) => "slack",
            Self::Teams(_) => "teams",
            Self::Discord(_) => "discord",
            Self::Plugin(_) => "plugin",
        }
    }

    /// The chat service of a chat channel, with its settings.
    pub fn chat(&self) -> Option<(Service, &Chat)> {
        match self {
            Self::Slack(chat) => Some((Service::Slack, chat)),
            Self::Teams(chat) => Some((Service::Teams, chat)),
            Self::Discord(chat) => Some((Service::Discord, chat)),
            Self::Email(_) | Self::Webhook(_) | Self::Plugin(_) => None,
        }
    }

    /// Whether `other` sends to the same place, so a stored password or
    /// signing secret may stay with it. Recipients do not count: the
    /// secret goes to the mail server, not to them. A chat channel's URL is
    /// its secret, so only the service and the host are here to compare: a
    /// new URL always comes with the request that changes it.
    pub fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Email(a), Self::Email(b)) => {
                (&a.host, a.port, &a.username) == (&b.host, b.port, &b.username)
            }
            (Self::Webhook(a), Self::Webhook(b)) => a.url == b.url,
            (Self::Slack(a), Self::Slack(b))
            | (Self::Teams(a), Self::Teams(b))
            | (Self::Discord(a), Self::Discord(b)) => a.host == b.host,
            // A plugin's settings may name where it sends: any change of
            // them is another target.
            (Self::Plugin(a), Self::Plugin(b)) => a.plugin == b.plugin && a.values == b.values,
            _ => false,
        }
    }

    /// Trims the fields and checks them; the error is a sentence for the
    /// form.
    pub fn normalize(self) -> Result<Self, String> {
        match self {
            Self::Email(email) => normalize_email(email).map(Self::Email),
            Self::Webhook(webhook) => normalize_webhook(webhook).map(Self::Webhook),
            Self::Slack(chat) => normalize_chat(chat).map(Self::Slack),
            Self::Teams(chat) => normalize_chat(chat).map(Self::Teams),
            Self::Discord(chat) => normalize_chat(chat).map(Self::Discord),
            Self::Plugin(plugin) => normalize_plugin(plugin).map(Self::Plugin),
        }
    }

    /// The same destination with `host`, which comes from the checked URL.
    pub fn with_chat_host(mut self, host: &str) -> Self {
        if let Self::Slack(chat) | Self::Teams(chat) | Self::Discord(chat) = &mut self {
            host.clone_into(&mut chat.host);
        }
        self
    }

    /// The settings column: the fields without the kind.
    pub(crate) fn settings(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        if let Some(object) = value.as_object_mut() {
            object.remove("kind");
        }
        value
    }

    pub(crate) fn from_row(kind: &str, mut settings: Value) -> Result<Self, serde_json::Error> {
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

/// What a channel hears about: the `customer_id` and `site_id` columns,
/// which hold at most one ID, like a role grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every event, those about no customer included.
    Everything,
    /// One customer, all its sites included.
    Customer(Uuid),
    /// One site.
    Site(Uuid),
}

impl Coverage {
    /// The coverage of the two columns; `None` when both are set.
    pub fn from_columns(customer_id: Option<Uuid>, site_id: Option<Uuid>) -> Option<Self> {
        match (customer_id, site_id) {
            (None, None) => Some(Self::Everything),
            (Some(id), None) => Some(Self::Customer(id)),
            (None, Some(id)) => Some(Self::Site(id)),
            (Some(_), Some(_)) => None,
        }
    }

    /// `(customer_id, site_id)`.
    pub fn columns(self) -> (Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Everything => (None, None),
            Self::Customer(id) => (Some(id), None),
            Self::Site(id) => (None, Some(id)),
        }
    }

    /// Whether an event about `customer` and `site` falls under this. The
    /// tenancy boundary of notifications: an event without either reaches
    /// only [`Coverage::Everything`].
    pub fn includes(self, customer: Option<Uuid>, site: Option<Uuid>) -> bool {
        match self {
            Self::Everything => true,
            Self::Customer(id) => customer == Some(id),
            Self::Site(id) => site == Some(id),
        }
    }
}

fn normalize_plugin(plugin: PluginChannel) -> Result<PluginChannel, String> {
    if !monitorflow_plugin::manifest::valid_name(&plugin.plugin) {
        return Err("Choose a plugin.".into());
    }
    let name = plugin.name.trim().to_owned();
    if name.chars().count() > MAX_CHAT_NAME {
        return Err(format!("Keep the name under {MAX_CHAT_NAME} characters."));
    }
    let values = plugin
        .values
        .into_iter()
        .map(|(key, value)| (key, value.trim().to_owned()))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    Ok(PluginChannel {
        name,
        values,
        ..plugin
    })
}

fn normalize_chat(chat: Chat) -> Result<Chat, String> {
    let name = chat.name.trim().to_owned();
    if name.chars().count() > MAX_CHAT_NAME {
        return Err(format!("Keep the name under {MAX_CHAT_NAME} characters."));
    }
    Ok(Chat { name, ..chat })
}

/// A channel as the dispatcher uses it.
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: Uuid,
    pub destination: Destination,
    /// The SMTP password, the webhook signing secret or the chat webhook
    /// URL.
    pub secret: Option<Secret>,
    /// Why the stored secret does not open (ADR-0110): every delivery
    /// fails with it rather than going out without the secret.
    pub secret_error: Option<String>,
    pub coverage: Coverage,
    /// Delivered only to public addresses: someone other than a global
    /// administrator chose the destination (ADR-0078).
    pub public_only: bool,
    /// A person's own destination (ADR-0081): `id` is the user's, and the
    /// outcome is recorded on their contact.
    pub person: bool,
    /// Hears only the follow-up steps that name it, not every event it
    /// covers (ADR-0082).
    pub follow_up_only: bool,
}

const CHANNEL_COLUMNS: &str =
    "id, kind, settings, secret, customer_id, site_id, public_only, follow_up_only";

fn channel(row: &sqlx::postgres::PgRow) -> Result<Channel, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    let destination = Destination::from_row(&kind, row.try_get("settings")?)
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    let (secret, secret_error) = split(secrets::open_stored(
        Column::NotificationChannels,
        row.try_get("secret")?,
    ));
    Ok(Channel {
        id: row.try_get("id")?,
        destination,
        secret,
        secret_error,
        coverage: Coverage::from_columns(row.try_get("customer_id")?, row.try_get("site_id")?)
            .ok_or_else(|| sqlx::Error::Decode("a channel with two scopes".into()))?,
        public_only: row.try_get("public_only")?,
        person: false,
        follow_up_only: row.try_get("follow_up_only")?,
    })
}

/// A secret read with [`secrets::open_stored`] as a [`Channel`] holds it.
pub fn split(stored: Option<secrets::Opened>) -> (Option<Secret>, Option<String>) {
    match stored {
        None => (None, None),
        Some(Ok(secret)) => (Some(secret), None),
        Some(Err(error)) => (None, Some(error)),
    }
}

/// `secret` sealed for `column`, as a database error when it cannot be.
pub fn sealed(column: Column, secret: Option<&Secret>) -> Result<Option<String>, sqlx::Error> {
    secret
        .map(|s| secrets::seal(column, s.expose()))
        .transpose()
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

/// Every channel.
pub async fn channels(db: impl PgExecutor<'_>) -> Result<Vec<Channel>, sqlx::Error> {
    // SQL-safe: interpolates only the constant CHANNEL_COLUMNS.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CHANNEL_COLUMNS} FROM notification_channels ORDER BY created_at, id"
    )))
    .fetch_all(db)
    .await?
    .iter()
    .map(channel)
    .collect()
}

/// One channel, `None` when it does not exist.
pub async fn load(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Channel>, sqlx::Error> {
    // SQL-safe: interpolates only the constant CHANNEL_COLUMNS.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await?
    .as_ref()
    .map(channel)
    .transpose()
}

/// One channel, locked until the transaction ends, so that what a change
/// was authorized against is still what it changes.
pub async fn lock(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Channel>, sqlx::Error> {
    // SQL-safe: interpolates only the constant CHANNEL_COLUMNS.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {CHANNEL_COLUMNS} FROM notification_channels WHERE id = $1 FOR UPDATE"
    )))
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
    coverage: Coverage,
    public_only: bool,
) -> Result<Uuid, sqlx::Error> {
    let (customer_id, site_id) = coverage.columns();
    let secret = sealed(Column::NotificationChannels, secret)?;
    sqlx::query_scalar(
        "INSERT INTO notification_channels (kind, settings, secret, customer_id, site_id, public_only)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(destination.kind())
    .bind(destination.settings())
    .bind(secret)
    .bind(customer_id)
    .bind(site_id)
    .bind(public_only)
    .fetch_one(db)
    .await
}

/// Replaces a channel's destination and coverage; `secret` `None` keeps
/// the stored one. `false` when it does not exist.
pub async fn update(
    db: impl PgExecutor<'_>,
    id: Uuid,
    destination: &Destination,
    secret: Option<Option<&Secret>>,
    coverage: Coverage,
    public_only: bool,
) -> Result<bool, sqlx::Error> {
    let (customer_id, site_id) = coverage.columns();
    let new_secret = sealed(Column::NotificationChannels, secret.flatten())?;
    let done = sqlx::query(
        "UPDATE notification_channels
         SET kind = $2, settings = $3, customer_id = $4, site_id = $5, public_only = $6,
             secret = CASE WHEN $7 THEN $8 ELSE secret END
         WHERE id = $1",
    )
    .bind(id)
    .bind(destination.kind())
    .bind(destination.settings())
    .bind(customer_id)
    .bind(site_id)
    .bind(public_only)
    .bind(secret.is_some())
    .bind(new_secret)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Makes channel `id` hear only the follow-up steps that name it, or every
/// event it covers again.
pub async fn set_follow_up_only(
    db: impl PgExecutor<'_>,
    id: Uuid,
    follow_up_only: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE notification_channels SET follow_up_only = $2 WHERE id = $1")
        .bind(id)
        .bind(follow_up_only)
        .execute(db)
        .await?;
    Ok(())
}

/// Records the outcome of a delivery or test to `channel`: `None` for
/// success.
pub async fn record_outcome(
    db: impl PgExecutor<'_>,
    channel: &Channel,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    let id = channel.id;
    let table = if channel.person {
        "user_contacts"
    } else {
        "notification_channels"
    };
    let key = if channel.person { "user_id" } else { "id" };
    // SQL-safe: interpolates only the constant table and column names above.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {table}
         SET last_sent_at = CASE WHEN $2::text IS NULL THEN now() ELSE last_sent_at END,
             last_error = $2,
             last_error_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END
         WHERE {key} = $1"
    )))
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
    fn coverage_goes_no_further_than_its_customer_or_site() {
        let (customer, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let (site, sibling) = (Uuid::from_u128(3), Uuid::from_u128(4));
        let at_site = (Some(customer), Some(site));
        let at_sibling = (Some(customer), Some(sibling));
        let at_customer = (Some(customer), None);
        let elsewhere = (Some(other), Some(Uuid::from_u128(5)));
        let nowhere = (None, None);
        let includes =
            |coverage: Coverage, (c, s): (Option<Uuid>, Option<Uuid>)| coverage.includes(c, s);
        for place in [at_site, at_sibling, at_customer, elsewhere, nowhere] {
            assert!(includes(Coverage::Everything, place));
        }
        let by_customer = Coverage::Customer(customer);
        assert!(includes(by_customer, at_site) && includes(by_customer, at_customer));
        assert!(!includes(by_customer, elsewhere) && !includes(by_customer, nowhere));
        let by_site = Coverage::Site(site);
        assert!(includes(by_site, at_site));
        assert!(!includes(by_site, at_sibling) && !includes(by_site, at_customer));
        assert!(!includes(by_site, elsewhere) && !includes(by_site, nowhere));
    }

    #[test]
    fn coverage_is_one_column_at_most() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            Coverage::from_columns(None, None),
            Some(Coverage::Everything)
        );
        assert_eq!(Coverage::from_columns(Some(id), Some(id)), None);
        for coverage in [Coverage::Customer(id), Coverage::Site(id)] {
            let (customer, site) = coverage.columns();
            assert_eq!(Coverage::from_columns(customer, site), Some(coverage));
        }
    }

    #[test]
    fn the_target_is_where_a_secret_would_go() {
        let base = email("smtp.x.io", "ops", "a@x.io", &["b@x.io"]);
        assert!(base.same_target(&email("smtp.x.io", "ops", "c@x.io", &["d@x.io"])));
        assert!(!base.same_target(&email("evil.example", "ops", "a@x.io", &["b@x.io"])));
        assert!(!base.same_target(&email("smtp.x.io", "root", "a@x.io", &["b@x.io"])));
        let hook = |url: &str| Destination::Webhook(Webhook { url: url.into() });
        assert!(hook("https://a/").same_target(&hook("https://a/")));
        assert!(!hook("https://a/").same_target(&hook("https://b/")));
        assert!(!hook("https://a/").same_target(&base));
        let chat = |name: &str, host: &str| Chat {
            name: name.into(),
            host: host.into(),
        };
        let slack = Destination::Slack(chat("#ops", "hooks.slack.com"));
        assert!(slack.same_target(&Destination::Slack(chat("#alerts", "hooks.slack.com"))));
        assert!(!slack.same_target(&Destination::Slack(chat("#ops", "hooks.slack-gov.com"))));
        assert!(!slack.same_target(&Destination::Discord(chat("#ops", "hooks.slack.com"))));
        assert!(!slack.same_target(&hook("https://hooks.slack.com/")));
        assert!(!hook("https://hooks.slack.com/").same_target(&slack));
    }
}
