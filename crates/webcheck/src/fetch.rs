//! One run of a website check: resolve, connect, TLS when https, the
//! request, redirects when followed, and the page (ADR-0150).
//!
//! HTTP/1.1 on a connection of our own, closed after each answer, so the
//! check sees the certificate of the very connection that served the page.
//! Nothing is cached between runs: each one is what a new visitor gets.

use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::Request;
use hyper_util::rt::TokioIo;
use monitorflow_sensors::port::PortTargets;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::{timeout_at, Instant};
use url::{Host, Url};

use crate::certificate::{self, Recorder, Report};
use crate::spec::{IpFamily, Method, Spec};

/// Redirects followed before the check gives up.
pub const MAX_REDIRECTS: u32 = 10;
/// Most bytes of a page read; the text is looked for in them.
pub const MAX_BODY: usize = 1024 * 1024;
const USER_AGENT: &str = concat!("MonitorFlow-website-check/", env!("CARGO_PKG_VERSION"));

/// Why a check failed, as the `reason` label of `up 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The host name has no address (of the family asked).
    Dns,
    /// No address accepted a connection.
    Connect,
    /// Out of time before the page was read.
    Timeout,
    /// The TLS handshake failed.
    Tls,
    /// The certificate is not good, and the check carries a password,
    /// which it does not send over such a connection.
    Certificate,
    /// The status code is not one expected.
    Status,
    /// The text expected is missing, or the text not expected is there.
    Content,
    /// Too many redirects, or one to something not http(s).
    Redirects,
    /// The agent's `sensors.port_targets` does not allow the address.
    Refused,
    /// The server did not speak HTTP/1.1 correctly.
    Protocol,
}

impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Tls => "tls",
            Self::Certificate => "certificate",
            Self::Status => "status",
            Self::Content => "content",
            Self::Redirects => "redirects",
            Self::Refused => "refused",
            Self::Protocol => "protocol",
        }
    }
}

/// What one run found.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub failure: Option<Failure>,
    /// The status of the last answer.
    pub status: Option<u16>,
    /// From the start to the end of the page, when it was read.
    pub seconds: Option<f64>,
    pub redirects: u32,
    /// The URL of the last request.
    pub final_url: Url,
    /// Whether the page passed the text conditions, when there are some
    /// and it was read.
    pub content_ok: Option<bool>,
    /// The certificate of the last https connection.
    pub certificate: Option<Report>,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Stream = Pin<Box<dyn Io>>;

/// Aborts the connection task when the run ends.
struct Abort(tokio::task::JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Where a run may connect: everywhere, or what the agent's
/// `sensors.port_targets` allows (ADR-0111); and which certificate
/// authorities it trusts, the machine's by default.
#[derive(Debug, Clone, Default)]
pub struct Limits {
    pub targets: Option<PortTargets>,
    /// `None`: [`certificate::roots`].
    pub roots: Option<Arc<rustls::RootCertStore>>,
}

impl Limits {
    /// Whether `url`'s host may be asked at all: `Err` with the reason when
    /// not. A name is allowed when listed, or when some range is listed
    /// for its port; its addresses are then checked one by one.
    pub fn check(&self, url: &Url) -> Result<(), String> {
        let Some(targets) = &self.targets else {
            return Ok(());
        };
        let port = url.port_or_known_default().unwrap_or(0);
        let allowed = match url.host() {
            _ if targets.is_empty() => false,
            Some(Host::Ipv4(ip)) => targets.allows_ip(ip.into(), port),
            Some(Host::Ipv6(ip)) => targets.allows_ip(ip.into(), port),
            Some(Host::Domain(name)) => {
                targets.allows_name(name, port) || targets.has_range_for(port)
            }
            None => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "{}:{port} is not in this agent's sensors.port_targets",
                url.host_str().unwrap_or_default()
            ))
        }
    }

    /// Whether `addr`, which `url`'s host resolved to, may be connected to.
    pub fn allows(&self, url: &Url, addr: SocketAddr) -> bool {
        let Some(targets) = &self.targets else {
            return true;
        };
        match url.host() {
            Some(Host::Domain(name)) if targets.allows_name(name, addr.port()) => true,
            _ => targets.allows_ip(addr.ip(), addr.port()),
        }
    }
}

fn family_ok(ip: IpAddr, family: IpFamily) -> bool {
    match family {
        IpFamily::Any => true,
        IpFamily::V4 => ip.is_ipv4(),
        IpFamily::V6 => ip.is_ipv6(),
    }
}

/// The addresses to try for `url`, or why there are none.
async fn addresses(
    spec: &Spec,
    url: &Url,
    limits: &Limits,
    cutoff: Instant,
) -> Result<Vec<SocketAddr>, Failure> {
    let port = url.port_or_known_default().ok_or(Failure::Dns)?;
    let found: Vec<SocketAddr> = match url.host() {
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(Host::Domain(name)) => {
            match timeout_at(cutoff, tokio::net::lookup_host((name, port))).await {
                Ok(Ok(addrs)) => addrs.collect(),
                Ok(Err(error)) => {
                    tracing::debug!(host = name, %error, "website check: lookup failed");
                    return Err(Failure::Dns);
                }
                Err(_) => return Err(Failure::Timeout),
            }
        }
        None => return Err(Failure::Dns),
    };
    let found: Vec<SocketAddr> = found
        .into_iter()
        .filter(|a| family_ok(a.ip(), spec.ip))
        .collect();
    if found.is_empty() {
        return Err(Failure::Dns);
    }
    let allowed: Vec<SocketAddr> = found
        .into_iter()
        .filter(|a| limits.allows(url, *a))
        .collect();
    if allowed.is_empty() {
        tracing::warn!(
            host = url.host_str().unwrap_or_default(),
            "website check: the host resolves only to addresses outside sensors.port_targets"
        );
        return Err(Failure::Refused);
    }
    Ok(allowed)
}

async fn connect(addrs: &[SocketAddr], cutoff: Instant) -> Result<TcpStream, Failure> {
    for addr in addrs {
        match timeout_at(cutoff, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => tracing::debug!(%addr, %error, "website check: connect failed"),
            Err(_) => return Err(Failure::Timeout),
        }
    }
    Err(Failure::Connect)
}

fn server_name(url: &Url) -> Option<ServerName<'static>> {
    match url.host()? {
        Host::Domain(name) => ServerName::try_from(name.to_owned()).ok(),
        Host::Ipv4(ip) => Some(ServerName::IpAddress(IpAddr::from(ip).into())),
        Host::Ipv6(ip) => Some(ServerName::IpAddress(IpAddr::from(ip).into())),
    }
}

/// TLS over `tcp`, accepting any certificate but recording what it is.
async fn tls(
    tcp: TcpStream,
    url: &Url,
    limits: &Limits,
    cutoff: Instant,
) -> (Result<Stream, Failure>, Option<Report>) {
    let Some(name) = server_name(url) else {
        return (Err(Failure::Tls), None);
    };
    let roots = limits.roots.clone().unwrap_or_else(certificate::roots);
    let recorder = match Recorder::new(roots) {
        Ok(recorder) => recorder,
        Err(error) => {
            tracing::warn!(%error, "website check: no usable certificate authorities");
            return (Err(Failure::Tls), None);
        }
    };
    let config = rustls::ClientConfig::builder_with_provider(certificate::provider())
        .with_safe_default_protocol_versions()
        .map(|b| {
            b.dangerous()
                .with_custom_certificate_verifier(recorder.clone())
                .with_no_client_auth()
        });
    let mut config = match config {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(%error, "website check: cannot set up TLS");
            return (Err(Failure::Tls), None);
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let result = match timeout_at(cutoff, connector.connect(name, tcp)).await {
        Ok(Ok(stream)) => Ok(Box::pin(stream) as Stream),
        Ok(Err(error)) => {
            tracing::debug!(%error, "website check: TLS handshake failed");
            Err(Failure::Tls)
        }
        Err(_) => Err(Failure::Timeout),
    };
    (result, recorder.take())
}

/// Scheme, host and port: credentials go only where the check points.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

fn authorization(spec: &Spec) -> Option<String> {
    let password = spec.password.as_deref()?;
    Some(match &spec.username {
        Some(user) => format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        ),
        None => format!("Bearer {password}"),
    })
}

fn request(
    spec: &Spec,
    url: &Url,
    method: Method,
    credentials: bool,
) -> Result<Request<Empty<Bytes>>, Failure> {
    let mut target = url.path().to_owned();
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    let mut host = url.host_str().unwrap_or_default().to_owned();
    if let Some(port) = url.port() {
        host = format!("{host}:{port}");
    }
    let mut builder = Request::builder()
        .method(method.as_str())
        .uri(target)
        .header("host", host)
        .header("user-agent", USER_AGENT)
        .header("accept", "*/*")
        .header("connection", "close");
    for (name, value) in &spec.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if credentials {
        if let Some(value) = authorization(spec) {
            builder = builder.header("authorization", value);
        }
    }
    builder.body(Empty::new()).map_err(|_| Failure::Protocol)
}

/// Reads at most [`MAX_BODY`] bytes of the page.
async fn read_body(mut body: hyper::body::Incoming, cutoff: Instant) -> Result<Vec<u8>, Failure> {
    let mut page = Vec::new();
    while page.len() < MAX_BODY {
        match timeout_at(cutoff, body.frame()).await {
            Err(_) => return Err(Failure::Timeout),
            Ok(None) => break,
            Ok(Some(Err(error))) => {
                tracing::debug!(%error, "website check: page cut short");
                return Err(Failure::Protocol);
            }
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    let room = MAX_BODY - page.len();
                    page.extend_from_slice(&data[..data.len().min(room)]);
                }
            }
        }
    }
    Ok(page)
}

fn content_ok(spec: &Spec, page: &[u8]) -> Option<bool> {
    if spec.contains.is_none() && spec.not_contains.is_none() {
        return None;
    }
    let text = String::from_utf8_lossy(page);
    let has = spec.contains.as_deref().is_none_or(|t| text.contains(t));
    let lacks = spec
        .not_contains
        .as_deref()
        .is_none_or(|t| !text.contains(t));
    Some(has && lacks)
}

/// Runs the check once, stopping at `cutoff`.
pub async fn run(spec: &Spec, limits: &Limits, cutoff: Instant) -> Outcome {
    let started = Instant::now();
    let mut outcome = Outcome {
        failure: None,
        status: None,
        seconds: None,
        redirects: 0,
        final_url: spec.url.clone(),
        content_ok: None,
        certificate: None,
    };
    loop {
        let url = outcome.final_url.clone();
        if limits.check(&url).is_err() {
            outcome.failure = Some(Failure::Refused);
            return outcome;
        }
        match hop(spec, &url, limits, cutoff, &mut outcome).await {
            // GET and HEAD stay what they are after any redirect.
            Ok(Some(next)) => {
                outcome.redirects += 1;
                if outcome.redirects > MAX_REDIRECTS {
                    outcome.failure = Some(Failure::Redirects);
                    return outcome;
                }
                outcome.final_url = next;
            }
            Ok(None) => break,
            Err(failure) => {
                outcome.failure = Some(failure);
                return outcome;
            }
        }
    }
    outcome.seconds = Some(started.elapsed().as_secs_f64());
    let status = outcome.status.unwrap_or(0);
    outcome.failure = if !spec.status_ok(status) {
        Some(Failure::Status)
    } else if outcome.content_ok == Some(false) {
        Some(Failure::Content)
    } else {
        None
    };
    outcome
}

/// One request: `Ok(Some(next))` for a redirect to follow, `Ok(None)` once
/// the answer is read into `outcome`.
async fn hop(
    spec: &Spec,
    url: &Url,
    limits: &Limits,
    cutoff: Instant,
    outcome: &mut Outcome,
) -> Result<Option<Url>, Failure> {
    let addrs = addresses(spec, url, limits, cutoff).await?;
    let tcp = connect(&addrs, cutoff).await?;
    let stream: Stream = if url.scheme() == "https" {
        let (stream, report) = tls(tcp, url, limits, cutoff).await;
        outcome.certificate = report;
        stream?
    } else {
        Box::pin(tcp)
    };
    let credentials = spec.password.is_some() && same_origin(url, &spec.url);
    if credentials && !outcome.certificate.as_ref().is_some_and(Report::ok) {
        // Never hand a secret to a server whose certificate failed.
        return Err(Failure::Certificate);
    }
    let handshake = hyper::client::conn::http1::handshake(TokioIo::new(stream));
    let (mut sender, connection) = match timeout_at(cutoff, handshake).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(_)) => return Err(Failure::Protocol),
        Err(_) => return Err(Failure::Timeout),
    };
    let _connection = Abort(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let request = request(spec, url, spec.method, credentials)?;
    let response = match timeout_at(cutoff, sender.send_request(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::debug!(%error, "website check: no answer");
            return Err(Failure::Protocol);
        }
        Err(_) => return Err(Failure::Timeout),
    };
    let status = response.status().as_u16();
    outcome.status = Some(status);
    if spec.follow_redirects && matches!(status, 301 | 302 | 303 | 307 | 308) {
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or(Failure::Redirects)?;
        let next = url.join(location).map_err(|_| Failure::Redirects)?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(Failure::Redirects);
        }
        return Ok(Some(next));
    }
    if spec.method == Method::Get {
        let page = read_body(response.into_body(), cutoff).await?;
        outcome.content_ok = content_ok(spec, &page);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec(pairs: &[(&str, &str)]) -> Spec {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Spec::parse(&map).unwrap()
    }

    #[test]
    fn origins_compare_scheme_host_and_port() {
        let a = Url::parse("https://x.test/a").unwrap();
        assert!(same_origin(
            &a,
            &Url::parse("https://x.test:443/b").unwrap()
        ));
        assert!(!same_origin(&a, &Url::parse("http://x.test/a").unwrap()));
        assert!(!same_origin(&a, &Url::parse("https://y.test/a").unwrap()));
        assert!(!same_origin(
            &a,
            &Url::parse("https://x.test:8443/a").unwrap()
        ));
    }

    #[test]
    fn credentials_are_basic_or_bearer() {
        let basic = spec(&[("url", "https://x"), ("username", "u"), ("password", "p")]);
        assert_eq!(authorization(&basic).unwrap(), "Basic dTpw");
        let bearer = spec(&[("url", "https://x"), ("password", "t")]);
        assert_eq!(authorization(&bearer).unwrap(), "Bearer t");
        assert!(authorization(&spec(&[("url", "https://x")])).is_none());
    }

    #[test]
    fn requests_carry_the_host_and_headers() {
        let s = spec(&[
            ("url", "https://x.test:8443/p?q=1"),
            ("headers", "X-A: b"),
            ("username", "u"),
            ("password", "p"),
        ]);
        let r = request(&s, &s.url, Method::Get, true).unwrap();
        assert_eq!(r.uri(), "/p?q=1");
        assert_eq!(r.headers()["host"], "x.test:8443");
        assert_eq!(r.headers()["x-a"], "b");
        assert!(r.headers().contains_key("authorization"));
        let r = request(&s, &s.url, Method::Head, false).unwrap();
        assert_eq!(r.method(), "HEAD");
        assert!(!r.headers().contains_key("authorization"));
        let v6 = spec(&[("url", "http://[::1]/")]);
        let r = request(&v6, &v6.url, Method::Get, false).unwrap();
        assert_eq!(r.headers()["host"], "[::1]");
    }

    #[test]
    fn text_conditions() {
        let s = spec(&[
            ("url", "http://x"),
            ("contains", "Welcome"),
            ("not_contains", "Error"),
        ]);
        assert_eq!(content_ok(&s, b"<h1>Welcome</h1>"), Some(true));
        assert_eq!(content_ok(&s, b"Welcome, Error"), Some(false));
        assert_eq!(content_ok(&s, b"nothing"), Some(false));
        assert_eq!(content_ok(&spec(&[("url", "http://x")]), b"x"), None);
    }

    #[test]
    fn limits_follow_port_targets() {
        let limits = Limits {
            targets: Some(PortTargets::parse(&["10.0.0.0/8:443", "web.test:80"]).unwrap()),
            roots: None,
        };
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(limits.check(&url("https://10.1.2.3/")).is_ok());
        assert!(limits.check(&url("http://10.1.2.3/")).is_err());
        assert!(limits.check(&url("http://web.test/")).is_ok());
        // A name resolved at run time against the ranges for its port.
        assert!(limits.check(&url("https://other.test/")).is_ok());
        assert!(limits.check(&url("http://other.test/")).is_err());
        let a: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let b: SocketAddr = "192.0.2.1:443".parse().unwrap();
        assert!(limits.allows(&url("https://other.test/"), a));
        assert!(!limits.allows(&url("https://other.test/"), b));
        assert!(Limits::default().check(&url("http://anything/")).is_ok());
        let none = Limits {
            targets: Some(PortTargets::parse::<&str>(&[]).unwrap()),
            roots: None,
        };
        assert!(none.check(&url("http://10.0.0.1/")).is_err());
    }
}
