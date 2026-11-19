//! The one capability of a plugin (ADR-0103): HTTP requests, made by the
//! host on its behalf, to the hosts its manifest lists. A request is
//! checked here before any connection; [`Http`] sends it.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, Url};
use serde::{Deserialize, Serialize};

use crate::limits;

/// Most headers of one request.
pub const MAX_HEADERS: usize = 32;

/// Headers a plugin may not set: the host decides where and how a request
/// goes, and the body's framing.
const REFUSED_HEADERS: [&str; 9] = [
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "keep-alive",
    "expect",
];

/// A host a plugin may call: its name and port, or ports 80 and 443 when
/// the port is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    pub host: String,
    pub port: Option<u16>,
}

fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl HostRule {
    /// `name` or `name:port`, lowercase. IPv6 literals are not supported.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (host, port) = match text.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0 && !port.starts_with('0'))
                    .ok_or_else(|| format!("{text}: bad port"))?;
                (host, Some(port))
            }
            None => (text, None),
        };
        if !valid_host(host) {
            return Err(format!(
                "{text}: a host name in lowercase, or an IPv4 address, with an optional port"
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// The host and explicit port of an `http` or `https` URL without
    /// credentials, as a setting of type `url` must be.
    pub fn of_url(url: &str) -> Option<Self> {
        let url = Url::parse(url).ok()?;
        let plain = matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none();
        let host = url.host_str().filter(|h| valid_host(h))?;
        plain.then(|| Self {
            host: host.to_owned(),
            port: url.port(),
        })
    }

    fn allows(&self, url: &Url) -> bool {
        let port = url.port_or_known_default();
        url.host_str() == Some(self.host.as_str())
            && match self.port {
                Some(rule) => port == Some(rule),
                None => matches!(port, Some(80 | 443)),
            }
    }
}

impl fmt::Display for HostRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.port {
            Some(port) => write!(f, "{}:{port}", self.host),
            None => f.write_str(&self.host),
        }
    }
}

/// A request as the plugin writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default = "get")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: String,
}

fn get() -> String {
    "GET".to_owned()
}

/// What the plugin reads back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The body as UTF-8, invalid sequences replaced.
    pub body: String,
    /// From sending the request to the end of the body: plugins have no
    /// clock of their own.
    pub elapsed_ms: u64,
}

/// A [`Request`] that passed [`check`].
#[derive(Debug, Clone)]
pub struct Checked {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Checks `request` against the host rules and the limits. The error is
/// for the plugin and its logs.
pub fn check(request: Request, rules: &[HostRule]) -> Result<Checked, String> {
    let method = match request.method.as_str() {
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" => {
            Method::from_bytes(request.method.as_bytes()).map_err(|e| e.to_string())?
        }
        other => return Err(format!("method {other} is not allowed")),
    };
    let url = Url::parse(&request.url).map_err(|e| format!("bad URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs may not hold a user name or password".into());
    }
    if !rules.iter().any(|rule| rule.allows(&url)) {
        let host = url.host_str().unwrap_or_default();
        return Err(format!("{host} is not a host this plugin may connect to"));
    }
    if request.headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} headers"));
    }
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        let token = !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
        if !token || REFUSED_HEADERS.contains(&lower.as_str()) || lower.starts_with("proxy-") {
            return Err(format!("header {name} is not allowed"));
        }
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(format!("header {name}: value too long or not on one line"));
        }
    }
    if request.body.len() > limits::HTTP_REQUEST_BYTES {
        return Err(format!(
            "the body is over {} KiB",
            limits::HTTP_REQUEST_BYTES / 1024
        ));
    }
    Ok(Checked {
        method,
        url,
        headers: request.headers,
        body: request.body,
    })
}

/// Sends checked requests. Called from the plugin's thread, which is a
/// blocking one: implementations may block.
pub trait Http: Send + Sync {
    fn send(&self, request: Checked, timeout: Duration) -> Result<Response, String>;
}

/// Where the host may connect on a plugin's behalf, beyond its host
/// rules: on an agent, what its `sensors.port_targets` allows (ADR-0111).
pub trait Destinations: Send + Sync {
    /// Whether `url`'s host and port may be asked at all; the reason when
    /// not.
    fn check(&self, url: &Url) -> Result<(), String>;
    /// Whether `addr`, which `url`'s host name resolved to, may be
    /// connected to.
    fn allows(&self, url: &Url, addr: SocketAddr) -> bool;
}

/// [`Http`] on a reqwest client, run on a tokio runtime. The client must
/// follow no redirects: a redirect would reach a host nobody checked.
#[derive(Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    destinations: Option<Arc<dyn Destinations>>,
}

impl fmt::Debug for ReqwestHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReqwestHttp")
            .field("client", &self.client)
            .field("bounded", &self.destinations.is_some())
            .finish()
    }
}

impl ReqwestHttp {
    pub fn new(client: reqwest::Client, runtime: tokio::runtime::Handle) -> Self {
        Self {
            client,
            runtime,
            destinations: None,
        }
    }

    /// On [`Self::default_client`]'s settings, connecting only where
    /// `destinations` allows: a host name is resolved first, and the
    /// request goes, without a proxy, to the allowed addresses alone, so
    /// neither DNS nor a proxy can take it elsewhere.
    pub fn bounded(destinations: Arc<dyn Destinations>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            destinations: Some(destinations),
            ..Self::new(Self::default_client(), runtime)
        }
    }

    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(limits::HTTP_TIMEOUT)
            .user_agent(concat!("MonitorFlow-plugin/", env!("CARGO_PKG_VERSION")))
    }

    /// A client following no redirects, for agents' checks.
    pub fn default_client() -> reqwest::Client {
        Self::builder().build().unwrap_or_default()
    }

    /// The client for `url`: this one, or, when bounded, one that reaches
    /// only the allowed addresses of its host.
    async fn client_for(&self, url: &Url, timeout: Duration) -> Result<reqwest::Client, String> {
        let Some(destinations) = &self.destinations else {
            return Ok(self.client.clone());
        };
        destinations.check(url)?;
        let port = url.port_or_known_default().ok_or("no port")?;
        let name = match url.host() {
            Some(url::Host::Domain(name)) => name,
            // An address: `check` decided.
            _ => return Self::builder().no_proxy().build().map_err(error_text),
        };
        let found = tokio::time::timeout(timeout, tokio::net::lookup_host((name, port)))
            .await
            .map_err(|_| "timed out".to_owned())?
            .map_err(|e| format!("could not resolve {name}: {e}"))?;
        let allowed: Vec<SocketAddr> = found.filter(|a| destinations.allows(url, *a)).collect();
        if allowed.is_empty() {
            return Err(format!(
                "{name} resolves only to addresses outside this agent's sensors.port_targets"
            ));
        }
        Self::builder()
            .no_proxy()
            .resolve_to_addrs(name, &allowed)
            .build()
            .map_err(error_text)
    }

    async fn send_async(&self, request: Checked, timeout: Duration) -> Result<Response, String> {
        let started = std::time::Instant::now();
        let client = self.client_for(&request.url, timeout).await?;
        let mut builder = client.request(request.method, request.url).timeout(timeout);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if !request.body.is_empty() {
            builder = builder.body(request.body);
        }
        let mut response = builder.send().await.map_err(error_text)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(error_text)? {
            if body.len() + chunk.len() > limits::HTTP_RESPONSE_BYTES {
                return Err(format!(
                    "the response is over {} MiB",
                    limits::HTTP_RESPONSE_BYTES / 1024 / 1024
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// reqwest's error and its causes, without the URL, which may hold a
/// token in its path.
fn error_text(error: reqwest::Error) -> String {
    let mut text = if error.is_timeout() {
        "timed out".to_owned()
    } else if error.is_connect() {
        "could not connect".to_owned()
    } else {
        "request failed".to_owned()
    };
    let error = error.without_url();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let line = cause.to_string();
        if !text.contains(&line) {
            text.push_str(": ");
            text.push_str(&line);
        }
        source = std::error::Error::source(cause);
    }
    text
}

impl Http for ReqwestHttp {
    fn send(&self, request: Checked, timeout: Duration) -> Result<Response, String> {
        self.runtime.block_on(self.send_async(request, timeout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(texts: &[&str]) -> Vec<HostRule> {
        texts.iter().map(|t| HostRule::parse(t).unwrap()).collect()
    }

    fn request(url: &str) -> Request {
        Request {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    #[test]
    fn host_rules_parse_names_and_ports() {
        assert_eq!(HostRule::parse("api.example.com").unwrap().port, None);
        assert_eq!(HostRule::parse("10.0.0.5:8080").unwrap().port, Some(8080));
        for bad in [
            "",
            "API.example.com",
            "a..b",
            "-a.b",
            "a.b:0",
            "a.b:080",
            "a.b:99999",
            "[::1]",
            "a b",
            "*.example.com",
        ] {
            assert!(HostRule::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            HostRule::of_url("https://ntfy.sh/x").unwrap().to_string(),
            "ntfy.sh"
        );
        assert_eq!(
            HostRule::of_url("http://Ntfy.Lan:8080")
                .unwrap()
                .to_string(),
            "ntfy.lan:8080"
        );
        for bad in [
            "ftp://x.y",
            "https://u@x.y",
            "https://u:p@x.y",
            "https://[::1]/",
            "not a url",
        ] {
            assert!(HostRule::of_url(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn only_listed_hosts_and_ports_pass() {
        let rules = rules(&["api.example.com", "svc.lan:8080"]);
        for ok in [
            "https://api.example.com/v1",
            "http://api.example.com:80/",
            "http://svc.lan:8080/health",
            "HTTPS://API.EXAMPLE.COM/",
        ] {
            assert!(check(request(ok), &rules).is_ok(), "{ok}");
        }
        for bad in [
            "https://api.example.com:8443/",
            "http://svc.lan/",
            "https://evil.example.com/",
            "https://api.example.com.evil.net/",
            "file:///etc/passwd",
            "https://user:pw@api.example.com/",
            "http://127.0.0.1/",
        ] {
            assert!(check(request(bad), &rules).is_err(), "{bad}");
        }
        assert!(check(request("https://api.example.com/"), &[]).is_err());
    }

    #[test]
    fn methods_headers_and_bodies_are_limited() {
        let rules = rules(&["api.example.com"]);
        let base = request("https://api.example.com/");
        let with = |change: fn(&mut Request)| {
            let mut r = base.clone();
            change(&mut r);
            check(r, &rules)
        };
        assert!(with(|r| r.method = "POST".into()).is_ok());
        assert!(with(|r| r.method = "CONNECT".into()).is_err());
        assert!(with(|r| r.headers.push(("Authorization".into(), "Bearer x".into()))).is_ok());
        for name in [
            "Host",
            "content-length",
            "Proxy-Authorization",
            "Transfer-Encoding",
            "bad name",
        ] {
            let mut r = base.clone();
            r.headers.push((name.into(), "x".into()));
            assert!(check(r, &rules).is_err(), "{name}");
        }
        assert!(with(|r| r.headers.push(("X-A".into(), "a\r\nHost: b".into()))).is_err());
        assert!(with(|r| r.body = "x".repeat(limits::HTTP_REQUEST_BYTES + 1)).is_err());
    }

    /// Allows one address, and any host name until it resolves.
    struct Only(std::net::IpAddr);

    impl Destinations for Only {
        fn check(&self, url: &Url) -> Result<(), String> {
            match url.host() {
                Some(url::Host::Domain(_)) => Ok(()),
                Some(url::Host::Ipv4(ip)) if std::net::IpAddr::V4(ip) == self.0 => Ok(()),
                _ => Err("not in sensors.port_targets".into()),
            }
        }

        fn allows(&self, _: &Url, addr: SocketAddr) -> bool {
            addr.ip() == self.0
        }
    }

    #[test]
    fn bounded_requests_reach_only_allowed_addresses() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        runtime.spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });
        let get = |http: &ReqwestHttp, url: &str| {
            let rules = rules(&[&format!("localhost:{port}"), &format!("127.0.0.1:{port}")]);
            http.send(check(request(url), &rules).unwrap(), Duration::from_secs(5))
        };
        let by_name = format!("http://localhost:{port}/");
        let by_address = format!("http://127.0.0.1:{port}/");
        let loopback = std::net::IpAddr::from([127, 0, 0, 1]);

        let allowed = ReqwestHttp::bounded(Arc::new(Only(loopback)), runtime.handle().clone());
        assert_eq!(get(&allowed, &by_name).unwrap().body, "ok");
        assert_eq!(get(&allowed, &by_address).unwrap().body, "ok");
        assert_eq!(served.load(Ordering::SeqCst), 2);

        let elsewhere = std::net::IpAddr::from([192, 0, 2, 1]);
        let refused = ReqwestHttp::bounded(Arc::new(Only(elsewhere)), runtime.handle().clone());
        let error = get(&refused, &by_name).unwrap_err();
        assert!(
            error.contains("outside this agent's sensors.port_targets"),
            "{error}"
        );
        let error = get(&refused, &by_address).unwrap_err();
        assert!(error.contains("not in sensors.port_targets"), "{error}");
        assert_eq!(served.load(Ordering::SeqCst), 2, "no connection was made");
    }
}
