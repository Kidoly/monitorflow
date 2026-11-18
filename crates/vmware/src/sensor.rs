//! The `vmware` sensor: one session kept between collections.

use std::time::Duration;

use async_trait::async_trait;
use monitorflow_sensors::{virt, Sample, Sensor, SensorError};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::inventory::{self, Inventory};
use crate::soap::{Client, Failure, ServiceContent};
use crate::tls::{self, Trust};
use crate::{Target, What};

/// Kept before the deadline so a slow answer still reports `up 0`.
const MARGIN: Duration = Duration::from_millis(300);
/// Longest single request.
const REQUEST: Duration = Duration::from_secs(120);

/// The session of one check.
#[derive(Default)]
struct Session {
    client: Option<Client>,
    content: Option<ServiceContent>,
}

pub struct VmwareSensor {
    target: Target,
    session: Mutex<Session>,
    roots: Option<std::sync::Arc<rustls::RootCertStore>>,
}

impl std::fmt::Debug for VmwareSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmwareSensor")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl VmwareSensor {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            session: Mutex::default(),
            roots: None,
        }
    }

    /// Trusts `roots` instead of the machine's, for tests.
    pub fn with_roots(mut self, roots: std::sync::Arc<rustls::RootCertStore>) -> Self {
        self.roots = Some(roots);
        self
    }

    fn client(&self, trust: Trust) -> Result<Client, Failure> {
        let roots = self.roots.clone().unwrap_or_else(tls::roots);
        Client::with_roots(&self.target.host, self.target.port, trust, REQUEST, roots)
    }

    /// Reads the inventory, logging in when the session is new or gone.
    async fn inventory(
        &self,
        session: &mut Session,
    ) -> Result<(ServiceContent, Inventory), Failure> {
        if session.client.is_none() {
            session.client = Some(self.client(Trust::Verify(self.target.fingerprint))?);
            session.content = None;
        }
        let client = session.client.as_mut().expect("set above");
        let content = match &session.content {
            Some(content) => content.clone(),
            None => {
                let content = client.service_content().await?;
                session.content = Some(content.clone());
                content
            }
        };
        let user = self.target.user.as_deref().unwrap_or_default();
        let password = self.target.password.as_deref().unwrap_or_default();
        if !client.logged_in() {
            client.login(&content, user, password).await?;
        }
        let view = match client.create_view(&content).await {
            Err(Failure::NotAuthenticated) => {
                client.login(&content, user, password).await?;
                client.create_view(&content).await?
            }
            other => other?,
        };
        let objects = client.retrieve(&content, &view).await;
        client.destroy_view(&view).await;
        Ok((content, inventory::read(&objects?)))
    }

    async fn collect_inventory(&self, deadline: Instant) -> Vec<Sample> {
        let mut session = self.session.lock().await;
        let outcome = tokio::time::timeout_at(
            deadline.checked_sub(MARGIN).unwrap_or(deadline),
            self.inventory(&mut session),
        )
        .await
        .unwrap_or_else(|_| Err(Failure::Connect("no answer in time".into())));
        let now = chrono::Utc::now().timestamp_millis();
        match outcome {
            Ok((content, inventory)) => {
                let mut samples = virt::samples(
                    &inventory.vms,
                    &inventory.hosts,
                    &inventory.clusters,
                    &inventory.datastores,
                    now,
                );
                samples.push(up(1.0, now, &content));
                samples
            }
            Err(failure) => {
                // A refused login or a broken connection starts again; an
                // API error keeps the session for the next collection.
                if !matches!(failure, Failure::Api(_)) {
                    *session = Session::default();
                }
                tracing::debug!(host = %self.target.host, reason = failure.reason(), error = %failure.message(), "vCenter collection failed");
                let mut sample = Sample::metric("up", 0.0)
                    .with_label("reason", failure.reason())
                    .with_label("error", virt::cut(&failure.message()));
                sample.timestamp_unix_ms = now;
                vec![sample]
            }
        }
    }

    /// Looks at what answers without trusting it, and logs in only when
    /// its certificate is trusted or pinned.
    async fn test(&self) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut look = match self.client(Trust::Look) {
            Ok(client) => client,
            Err(failure) => return vec![failed("answers", &failure)],
        };
        let content = match look.service_content().await {
            Ok(content) => content,
            Err(failure) => return vec![failed("answers", &failure)],
        };
        let mut answers = up(1.0, chrono::Utc::now().timestamp_millis(), &content);
        answers.sensor = "answers".into();
        out.push(answers);
        let Some(seen) = look.seen() else {
            return out;
        };
        let trusted = seen.trusted || self.target.fingerprint == Some(seen.fingerprint);
        out.push(
            Sample::metric("certificate_trusted", f64::from(u8::from(trusted)))
                .with_label("fingerprint", tls::show_fingerprint(&seen.fingerprint))
                .with_label("by_roots", if seen.trusted { "true" } else { "false" })
                .with_label("subject", virt::cut(&seen.subject))
                .with_label("not_after", seen.not_after.to_string()),
        );
        let (Some(user), Some(_)) = (&self.target.user, &self.target.password) else {
            return out;
        };
        if !trusted {
            return out;
        }
        let mut session = Session::default();
        match self.inventory(&mut session).await {
            Ok((_, inventory)) => {
                out.push(Sample::metric("login", 1.0).with_label("user", virt::cut(user)));
                for (kind, count) in [
                    ("vm", inventory.vms.len()),
                    ("host", inventory.hosts.len()),
                    ("cluster", inventory.clusters.len()),
                    ("datastore", inventory.datastores.len()),
                ] {
                    out.push(Sample::metric("objects", count as f64).with_instance(kind));
                }
            }
            Err(failure) => out.push(failed("login", &failure)),
        }
        out
    }
}

/// `up` with what answers.
fn up(value: f64, now: i64, content: &ServiceContent) -> Sample {
    let mut sample = Sample::metric("up", value)
        .with_label("product", virt::cut(&content.full_name))
        .with_label("version", virt::cut(&content.version))
        .with_label("api_type", virt::cut(&content.api_type));
    sample.timestamp_unix_ms = now;
    sample
}

fn failed(metric: &str, failure: &Failure) -> Sample {
    Sample::metric(metric, 0.0)
        .with_label("reason", failure.reason())
        .with_label("error", virt::cut(&failure.message()))
}

#[async_trait]
impl Sensor for VmwareSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        Ok(match self.target.what {
            What::Inventory => self.collect_inventory(deadline).await,
            What::Test => tokio::time::timeout_at(
                deadline.checked_sub(MARGIN).unwrap_or(deadline),
                self.test(),
            )
            .await
            .unwrap_or_else(|_| {
                vec![failed(
                    "answers",
                    &Failure::Connect("no answer in time".into()),
                )]
            }),
        })
    }
}
