//! The one capability of a plugin (ADR-0103): HTTP requests, made by the
//! host on its behalf, to the hosts its manifest lists. A request is
//! checked here before any connection; [`Http`] sends it.

use std::fmt;
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

/// [`Http`] on a reqwest client, run on a tokio runtime. The client must
/// follow no redirects: a redirect would reach a host nobody checked.
#[derive(Debug, Clone)]
pub struct ReqwestHttp {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
}

impl ReqwestHttp {
    pub fn new(client: reqwest::Client, runtime: tokio::runtime::Handle) -> Self {
        Self { client, runtime }
    }

    /// A client following no redirects, for agents' checks.
    pub fn default_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(limits::HTTP_TIMEOUT)
            .user_agent(concat!("MonitorFlow-plugin/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    }

    async fn send_async(&self, request: Checked, timeout: Duration) -> Result<Response, String> {
        let started = std::time::Instant::now();
        let mut builder = self
            .client
            .request(request.method, request.url)
            .timeout(timeout);
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
}
