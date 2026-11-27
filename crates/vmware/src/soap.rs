//! The few vim25 calls a check makes, over HTTPS with a kept session.
//! None of them changes anything on the vCenter (ADR-0156).

use std::sync::Arc;
use std::time::Duration;

use crate::tls::{self, Trust, Verifier};
use crate::xml::{self, escape, Element};

/// Most bytes of answers read for one call, or for all the pages of one
/// collection; a vCenter of thousands of machines stays below.
pub const MAX_ANSWER: usize = 64 << 20;
/// Objects per page of `RetrievePropertiesEx`.
pub const PAGE: usize = 1000;
/// Pages read at most in one collection.
const MAX_PAGES: usize = 200;
/// The vim25 version the requests are written for; every vCenter and
/// ESXi from 6.5 on answers it.
const ACTION: &str = "urn:vim25/6.5";

/// What the answers of one call, or of the pages of one collection, may
/// still take: bytes read and elements parsed.
struct Budget {
    bytes: usize,
    elements: usize,
}

impl Budget {
    fn full() -> Self {
        Self {
            bytes: MAX_ANSWER,
            elements: xml::MAX_ELEMENTS,
        }
    }
}

/// Why a call failed, as the `reason` of `up`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Connect(String),
    Tls(String),
    Certificate(String),
    Login(String),
    /// The session expired; log in again.
    NotAuthenticated,
    Api(String),
}

impl Failure {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Connect(_) => "connect",
            Self::Tls(_) => "tls",
            Self::Certificate(_) => "certificate",
            Self::Login(_) | Self::NotAuthenticated => "login",
            Self::Api(_) => "api",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Connect(m)
            | Self::Tls(m)
            | Self::Certificate(m)
            | Self::Login(m)
            | Self::Api(m) => m.clone(),
            Self::NotAuthenticated => "the session was refused".into(),
        }
    }
}

/// Text from the other side, bounded and without control characters.
fn bounded(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(256).collect()
}

/// The references a session works with, from `RetrieveServiceContent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceContent {
    pub root_folder: String,
    pub property_collector: String,
    pub view_manager: String,
    pub session_manager: String,
    /// `VirtualCenter` or `HostAgent` (a standalone ESXi host).
    pub api_type: String,
    pub full_name: String,
    pub version: String,
    pub instance_uuid: String,
}

impl ServiceContent {
    fn read(answer: &Element) -> Result<Self, Failure> {
        let value = answer
            .child("returnval")
            .ok_or_else(|| Failure::Api("no service content in the answer".into()))?;
        let text = |path: &[&str]| {
            value
                .path(path)
                .map(|e| e.text.trim().to_owned())
                .unwrap_or_default()
        };
        let content = Self {
            root_folder: text(&["rootFolder"]),
            property_collector: text(&["propertyCollector"]),
            view_manager: text(&["viewManager"]),
            session_manager: text(&["sessionManager"]),
            api_type: text(&["about", "apiType"]),
            full_name: text(&["about", "fullName"]),
            version: text(&["about", "version"]),
            instance_uuid: text(&["about", "instanceUuid"]),
        };
        if content.root_folder.is_empty()
            || content.property_collector.is_empty()
            || content.view_manager.is_empty()
            || content.session_manager.is_empty()
        {
            return Err(Failure::Api(
                "this does not look like a vCenter or ESXi API".into(),
            ));
        }
        Ok(content)
    }
}

/// One vCenter or ESXi host.
pub struct Client {
    http: reqwest::Client,
    url: String,
    trust: Trust,
    verifier: Arc<Verifier>,
    cookie: Option<String>,
    page: usize,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("url", &self.url)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client of `https://host:port/sdk` trusting as `trust` says.
    pub fn new(host: &str, port: u16, trust: Trust, timeout: Duration) -> Result<Self, Failure> {
        Self::with_roots(host, port, trust, timeout, tls::roots())
    }

    pub fn with_roots(
        host: &str,
        port: u16,
        trust: Trust,
        timeout: Duration,
        roots: Arc<rustls::RootCertStore>,
    ) -> Result<Self, Failure> {
        let verifier = Verifier::new(roots, trust).map_err(Failure::Tls)?;
        let config = verifier.config().map_err(Failure::Tls)?;
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(config)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .build()
            .map_err(|e| Failure::Tls(e.to_string()))?;
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        Ok(Self {
            http,
            url: format!("https://{host}:{port}/sdk"),
            trust,
            verifier,
            cookie: None,
            page: PAGE,
        })
    }

    /// Reads `page` objects a page instead of [`PAGE`].
    pub fn with_page(mut self, page: usize) -> Self {
        self.page = page.clamp(1, PAGE);
        self
    }

    /// The certificate of the last handshake.
    pub fn seen(&self) -> Option<tls::Seen> {
        self.verifier.seen()
    }

    pub fn logged_in(&self) -> bool {
        self.cookie.is_some()
    }

    /// Why the last handshake failed on its certificate, if it did.
    fn certificate_failure(&self) -> Option<Failure> {
        self.verifier.failure().map(Failure::Certificate)
    }

    /// Sends one call and returns the element inside the SOAP body.
    async fn call(&mut self, call: &str) -> Result<Element, Failure> {
        self.call_within(call, &mut Budget::full()).await
    }

    /// [`Self::call`] within what `budget` has left, taking what it used.
    async fn call_within(&mut self, call: &str, budget: &mut Budget) -> Result<Element, Failure> {
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Body>{call}</soapenv:Body></soapenv:Envelope>"#
        );
        let mut request = self
            .http
            .post(&self.url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", ACTION)
            .body(body);
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", format!("vmware_soap_session={cookie}"));
        }
        let mut answer = match request.send().await {
            Ok(answer) => answer,
            Err(error) => {
                if let Some(failure) = self.certificate_failure() {
                    return Err(failure);
                }
                let text = bounded(&error_chain(&error));
                return Err(if error.is_timeout() {
                    Failure::Connect(format!("no answer in time: {text}"))
                } else if text.contains("certificate") || text.contains("handshake") {
                    Failure::Tls(text)
                } else {
                    Failure::Connect(text)
                });
            }
        };
        for value in answer.headers().get_all("set-cookie") {
            if let Some(session) = value
                .to_str()
                .ok()
                .and_then(|v| v.split(';').next())
                .and_then(|v| v.trim().strip_prefix("vmware_soap_session="))
            {
                self.cookie = Some(session.trim_matches('"').to_owned());
            }
        }
        let status = answer.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = answer
            .chunk()
            .await
            .map_err(|e| Failure::Connect(bounded(&error_chain(&e))))?
        {
            if bytes.len() + chunk.len() > budget.bytes {
                return Err(Failure::Api("the answer is too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        budget.bytes -= bytes.len();
        // Parsing tens of MiB is work for a blocking thread.
        let elements = budget.elements;
        let parsed = tokio::task::spawn_blocking(move || {
            let mut left = elements;
            xml::parse_within(&String::from_utf8_lossy(&bytes), &mut left).map(|root| (root, left))
        })
        .await
        .map_err(|_| Failure::Api("the answer could not be read".into()))?;
        let root = parsed.map(|(root, left)| {
            budget.elements = left;
            root
        });
        let mut root = root.map_err(|e| {
            Failure::Api(if status.is_success() || status.as_u16() == 500 {
                format!("the answer does not read: {}", bounded(&e))
            } else {
                format!("the API answered {status}")
            })
        })?;
        let body = root
            .children
            .iter_mut()
            .find(|c| c.name == "Body")
            .filter(|b| !b.children.is_empty())
            .map(|b| b.children.swap_remove(0))
            .ok_or_else(|| Failure::Api("an empty answer".into()))?;
        if body.name == "Fault" {
            return Err(fault(&body));
        }
        Ok(body)
    }

    /// `RetrieveServiceContent`: needs no login, and tells what answers.
    pub async fn service_content(&mut self) -> Result<ServiceContent, Failure> {
        let answer = self
            .call(r#"<RetrieveServiceContent xmlns="urn:vim25"><_this type="ServiceInstance">ServiceInstance</_this></RetrieveServiceContent>"#)
            .await?;
        ServiceContent::read(&answer)
    }

    /// Logs in as `user`.
    pub async fn login(
        &mut self,
        content: &ServiceContent,
        user: &str,
        password: &str,
    ) -> Result<(), Failure> {
        self.cookie = None;
        let call = format!(
            r#"<Login xmlns="urn:vim25"><_this type="SessionManager">{}</_this><userName>{}</userName><password>{}</password></Login>"#,
            escape(&content.session_manager),
            escape(user),
            escape(password)
        );
        match self.call(&call).await {
            Ok(_) if self.cookie.is_some() => Ok(()),
            Ok(_) => Err(Failure::Api("the login gave no session".into())),
            Err(Failure::Api(message)) | Err(Failure::Login(message)) => {
                self.cookie = None;
                Err(Failure::Login(message))
            }
            Err(other) => {
                self.cookie = None;
                Err(other)
            }
        }
    }

    /// A view of every host, cluster, virtual machine and datastore.
    pub async fn create_view(&mut self, content: &ServiceContent) -> Result<String, Failure> {
        let call = format!(
            r#"<CreateContainerView xmlns="urn:vim25"><_this type="ViewManager">{}</_this><container type="Folder">{}</container><type>HostSystem</type><type>VirtualMachine</type><type>Datastore</type><type>ClusterComputeResource</type><recursive>true</recursive></CreateContainerView>"#,
            escape(&content.view_manager),
            escape(&content.root_folder)
        );
        let answer = self.call(&call).await?;
        answer
            .text_of("returnval")
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Failure::Api("no view in the answer".into()))
    }

    /// Removes a view; a failure only leaves it to the session's end.
    pub async fn destroy_view(&mut self, view: &str) {
        let call = format!(
            r#"<DestroyView xmlns="urn:vim25"><_this type="ContainerView">{}</_this></DestroyView>"#,
            escape(view)
        );
        if let Err(error) = self.call(&call).await {
            tracing::debug!(error = %error.message(), "could not remove a container view");
        }
    }

    /// Every object of `view` with the properties of [`PROPERTIES`].
    pub async fn retrieve(
        &mut self,
        content: &ServiceContent,
        view: &str,
    ) -> Result<Vec<Element>, Failure> {
        let collector = escape(&content.property_collector);
        let mut sets = String::new();
        for (kind, paths) in PROPERTIES {
            sets.push_str("<propSet><type>");
            sets.push_str(kind);
            sets.push_str("</type>");
            for path in *paths {
                sets.push_str("<pathSet>");
                sets.push_str(path);
                sets.push_str("</pathSet>");
            }
            sets.push_str("</propSet>");
        }
        let call = format!(
            r#"<RetrievePropertiesEx xmlns="urn:vim25"><_this type="PropertyCollector">{collector}</_this><specSet>{sets}<objectSet><obj type="ContainerView">{}</obj><skip>true</skip><selectSet xsi:type="TraversalSpec"><name>view</name><type>ContainerView</type><path>view</path><skip>false</skip></selectSet></objectSet></specSet><options><maxObjects>{}</maxObjects></options></RetrievePropertiesEx>"#,
            escape(view),
            self.page
        );
        // Every page together stays within one budget (ADR-0156).
        let mut budget = Budget::full();
        let mut answer = self.call_within(&call, &mut budget).await?;
        let mut objects = Vec::new();
        for _ in 0..MAX_PAGES {
            let Some(value) = answer.children.iter_mut().find(|c| c.name == "returnval") else {
                return Ok(objects);
            };
            let token = value
                .text_of("token")
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            objects.extend(
                std::mem::take(&mut value.children)
                    .into_iter()
                    .filter(|c| c.name == "objects"),
            );
            let Some(token) = token else {
                return Ok(objects);
            };
            let call = format!(
                r#"<ContinueRetrievePropertiesEx xmlns="urn:vim25"><_this type="PropertyCollector">{collector}</_this><token>{}</token></ContinueRetrievePropertiesEx>"#,
                escape(&token)
            );
            answer = self.call_within(&call, &mut budget).await?;
        }
        Err(Failure::Api("too many pages of objects".into()))
    }
}

/// Every message of an error and its sources.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let more = inner.to_string();
        if !text.contains(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        source = inner.source();
    }
    text
}

/// A SOAP fault as a [`Failure`].
fn fault(fault: &Element) -> Failure {
    let message = bounded(
        fault
            .text_of("faultstring")
            .unwrap_or("the API refused the call"),
    );
    let kind = fault
        .child("detail")
        .and_then(|d| d.children.first())
        .map(|d| d.attribute("type").unwrap_or(&d.name).to_owned())
        .unwrap_or_default();
    let kind = kind
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches("Fault");
    match kind {
        "NotAuthenticated" => Failure::NotAuthenticated,
        "InvalidLogin" | "NoPermission" | "InvalidPrivilege" => Failure::Login(message),
        _ => Failure::Api(message),
    }
}

/// What a collection reads of each kind of object.
pub const PROPERTIES: &[(&str, &[&str])] = &[
    (
        "HostSystem",
        &[
            "name",
            "parent",
            "runtime.connectionState",
            "runtime.powerState",
            "runtime.inMaintenanceMode",
            "summary.overallStatus",
            "summary.hardware",
            "summary.quickStats",
            "summary.config.product.fullName",
            "triggeredAlarmState",
            "runtime.healthSystemRuntime.systemHealthInfo.numericSensorInfo",
        ],
    ),
    (
        "VirtualMachine",
        &[
            "name",
            "config.uuid",
            "config.template",
            "runtime.powerState",
            "runtime.host",
            "summary.config.numCpu",
            "summary.config.memorySizeMB",
            "summary.config.guestFullName",
            "summary.quickStats.overallCpuUsage",
            "summary.quickStats.guestMemoryUsage",
            "summary.runtime.maxCpuUsage",
            "summary.storage.committed",
            "summary.storage.uncommitted",
            "summary.overallStatus",
            "guest.hostName",
            "guest.ipAddress",
            "guest.net",
            "snapshot.rootSnapshotList",
            "triggeredAlarmState",
        ],
    ),
    (
        "Datastore",
        &[
            "name",
            "summary.capacity",
            "summary.freeSpace",
            "summary.accessible",
            "summary.type",
            "overallStatus",
            "triggeredAlarmState",
        ],
    ),
    (
        "ClusterComputeResource",
        &[
            "name",
            "summary.numHosts",
            "summary.numEffectiveHosts",
            "overallStatus",
            "triggeredAlarmState",
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_are_typed() {
        let parse = |text: &str| fault(&xml::parse(text).unwrap());
        assert_eq!(
            parse(
                r#"<Fault><faultcode>ServerFaultCode</faultcode><faultstring>Login failure</faultstring><detail><InvalidLoginFault xsi:type="InvalidLogin"></InvalidLoginFault></detail></Fault>"#
            ),
            Failure::Login("Login failure".into())
        );
        assert_eq!(
            parse(
                r#"<Fault><faultstring>The session is not authenticated.</faultstring><detail><NotAuthenticatedFault xsi:type="NotAuthenticated"/></detail></Fault>"#
            ),
            Failure::NotAuthenticated
        );
        assert_eq!(
            parse("<Fault><faultstring>odd</faultstring></Fault>"),
            Failure::Api("odd".into())
        );
        assert_eq!(Failure::NotAuthenticated.reason(), "login");
    }

    #[test]
    fn service_content_needs_its_references() {
        let answer = xml::parse(include_str!("../tests/fixtures/service_content.xml")).unwrap();
        let body = answer.child("Body").unwrap().children[0].clone();
        let content = ServiceContent::read(&body).unwrap();
        assert_eq!(content.api_type, "VirtualCenter");
        assert_eq!(content.root_folder, "group-d1");
        assert_eq!(content.version, "6.5.0");
        assert!(ServiceContent::read(&xml::parse("<r><returnval/></r>").unwrap()).is_err());
    }
}
