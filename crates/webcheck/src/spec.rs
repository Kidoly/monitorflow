//! The parameters of a website check, checked once when it is built
//! (ADR-0150). Every value is a string, as the agent receives it.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use monitorflow_sensors::BuildError;
use url::Url;

use crate::KIND;

/// Every parameter [`Spec::parse`] knows.
pub const PARAMS: &[&str] = &[
    "url",
    "method",
    "status",
    "contains",
    "not_contains",
    "follow_redirects",
    "headers",
    "username",
    "password",
    "ip",
];

/// The parameter holding the secret: sealed by the server, never shown
/// back, sent only to whoever runs the check (ADR-0150).
pub const SECRET_PARAM: &str = "password";

/// Longest URL, in bytes.
const MAX_URL: usize = 1024;
/// Longest text looked for in a page.
const MAX_TEXT: usize = 256;
/// Most extra headers.
const MAX_HEADERS: usize = 16;
/// Longest user name.
const MAX_USERNAME: usize = 128;

/// GET reads the page; HEAD only its status and headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        }
    }
}

/// Which addresses of a host name to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpFamily {
    Any,
    V4,
    V6,
}

/// What the check asks for and expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub url: Url,
    pub method: Method,
    /// Status codes that pass.
    pub status: Vec<RangeInclusive<u16>>,
    /// Text the page must hold.
    pub contains: Option<String>,
    /// Text the page must not hold.
    pub not_contains: Option<String>,
    pub follow_redirects: bool,
    /// Extra request headers, in order.
    pub headers: Vec<(String, String)>,
    /// Basic authentication with `password`, or a bearer token when only
    /// `password` is set.
    pub username: Option<String>,
    pub password: Option<String>,
    pub ip: IpFamily,
}

fn bad(name: &'static str, reason: impl Into<String>) -> BuildError {
    BuildError::BadParam {
        kind: KIND,
        name,
        reason: reason.into(),
    }
}

/// `200`, `200-299`, or a comma-separated list of both.
pub fn parse_status(text: &str) -> Result<Vec<RangeInclusive<u16>>, String> {
    let reason =
        "must be status codes from 100 to 599 or ranges like `200-299`, separated by commas";
    let code = |s: &str| {
        s.trim()
            .parse::<u16>()
            .ok()
            .filter(|c| (100..=599).contains(c))
    };
    let mut ranges = Vec::new();
    for item in text.split(',') {
        let range = match item.split_once('-') {
            Some((low, high)) => code(low).zip(code(high)).filter(|(l, h)| l <= h),
            None => code(item).map(|c| (c, c)),
        };
        let (low, high) = range.ok_or(reason)?;
        ranges.push(low..=high);
    }
    if ranges.len() > 32 {
        return Err("must list at most 32 codes or ranges".into());
    }
    Ok(ranges)
}

/// `Name: value` lines. Names are HTTP tokens; headers MonitorFlow sets
/// itself, or that carry secrets in clear, are refused.
pub fn parse_headers(text: &str) -> Result<Vec<(String, String)>, String> {
    const OWN: &[&str] = &[
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "upgrade",
        "te",
        "trailer",
        "keep-alive",
        "proxy-connection",
    ];
    const SECRET: &[&str] = &["authorization", "proxy-authorization", "cookie"];
    let mut headers = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("`{}` is not `Name: value`", shorten(line)))?;
        let (name, value) = (name.trim(), value.trim());
        let token = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
        if !token {
            return Err(format!("`{}` is not a header name", shorten(name)));
        }
        let lower = name.to_ascii_lowercase();
        if OWN.contains(&lower.as_str()) {
            return Err(format!("{name} is set by the check itself"));
        }
        if SECRET.contains(&lower.as_str()) {
            return Err(format!(
                "{name} would be shown in clear: use the user name and password, which are kept sealed"
            ));
        }
        if value.len() > 512 || value.chars().any(char::is_control) {
            return Err(format!(
                "the value of {name} must be at most 512 bytes without control characters"
            ));
        }
        headers.push((name.to_owned(), value.to_owned()));
    }
    if headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} headers"));
    }
    Ok(headers)
}

/// A URL a website check may request: http or https, a host, no user
/// name or password in it.
pub fn parse_url(text: &str) -> Result<Url, String> {
    if text.len() > MAX_URL {
        return Err(format!("must be at most {MAX_URL} bytes"));
    }
    let url = Url::parse(text).map_err(|_| format!("`{}` is not a URL", shorten(text)))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("must start with http:// or https://".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("must name a host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not hold a user name or password: use the check's own fields".into());
    }
    Ok(url)
}

fn shorten(text: &str) -> String {
    text.chars()
        .take(64)
        .collect::<String>()
        .escape_debug()
        .to_string()
}

fn text_param(name: &'static str, value: &str) -> Result<String, BuildError> {
    if value.is_empty() || value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        return Err(bad(
            name,
            format!("must be 1 to {MAX_TEXT} bytes without control characters"),
        ));
    }
    Ok(value.to_owned())
}

impl Spec {
    /// Checks `params`; an unknown parameter is refused, so a typo does
    /// not silently fall back to a default.
    pub fn parse(params: &BTreeMap<String, String>) -> Result<Self, BuildError> {
        let known: BTreeSet<&str> = PARAMS.iter().copied().collect();
        if let Some(name) = params.keys().find(|k| !known.contains(k.as_str())) {
            return Err(BuildError::UnknownParam {
                kind: KIND,
                name: name.chars().take(64).collect(),
            });
        }
        // Values are trimmed, as for the other kinds, except the secret.
        let get = |name: &str| params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
        let url = get("url").ok_or_else(|| bad("url", "is required"))?;
        let url = parse_url(url).map_err(|reason| bad("url", reason))?;
        let method = match get("method").map(str::to_ascii_uppercase).as_deref() {
            None | Some("GET") => Method::Get,
            Some("HEAD") => Method::Head,
            Some(_) => return Err(bad("method", "must be `GET` or `HEAD`")),
        };
        let follow_redirects = match get("follow_redirects") {
            None | Some("true") => true,
            Some("false") => false,
            Some(_) => return Err(bad("follow_redirects", "must be `true` or `false`")),
        };
        let status = match get("status") {
            Some(text) => parse_status(text).map_err(|reason| bad("status", reason))?,
            // Without following, a redirect is the answer: it passes.
            None if follow_redirects => std::iter::once(200..=299).collect(),
            None => std::iter::once(200..=399).collect(),
        };
        let contains = get("contains")
            .map(|v| text_param("contains", v))
            .transpose()?;
        let not_contains = get("not_contains")
            .map(|v| text_param("not_contains", v))
            .transpose()?;
        if method == Method::Head && (contains.is_some() || not_contains.is_some()) {
            return Err(bad(
                "method",
                "HEAD reads no page, so it cannot look for text: use GET",
            ));
        }
        let headers = match get("headers") {
            Some(text) => parse_headers(text).map_err(|reason| bad("headers", reason))?,
            None => Vec::new(),
        };
        let username = get("username")
            .map(|name| {
                if name.len() > MAX_USERNAME || name.contains(':') || name.chars().any(char::is_control)
                {
                    Err(bad(
                        "username",
                        format!("must be at most {MAX_USERNAME} bytes, without `:` or control characters"),
                    ))
                } else {
                    Ok(name.to_owned())
                }
            })
            .transpose()?;
        let password = params
            .get(SECRET_PARAM)
            .filter(|p| !p.is_empty())
            .map(|p| {
                if p.len() > 1024 || p.chars().any(char::is_control) {
                    Err(bad(
                        SECRET_PARAM,
                        "must be at most 1024 bytes without control characters",
                    ))
                } else {
                    Ok(p.clone())
                }
            })
            .transpose()?;
        if username.is_some() && password.is_none() {
            return Err(bad(SECRET_PARAM, "is required with a user name"));
        }
        if password.is_some() && url.scheme() != "https" {
            return Err(bad(
                SECRET_PARAM,
                "is sent only over https, never in clear: use an https:// URL",
            ));
        }
        let ip = match get("ip") {
            None | Some("any") => IpFamily::Any,
            Some("4") | Some("ipv4") => IpFamily::V4,
            Some("6") | Some("ipv6") => IpFamily::V6,
            Some(_) => return Err(bad("ip", "must be `any`, `4` or `6`")),
        };
        let literal = match url.host() {
            Some(url::Host::Ipv4(_)) => Some(IpFamily::V4),
            Some(url::Host::Ipv6(_)) => Some(IpFamily::V6),
            _ => None,
        };
        if literal.is_some_and(|family| ip != IpFamily::Any && ip != family) {
            return Err(bad(
                "ip",
                "the URL holds an address of the other family: choose it only for host names",
            ));
        }
        Ok(Self {
            url,
            method,
            status,
            contains,
            not_contains,
            follow_redirects,
            headers,
            username,
            password,
            ip,
        })
    }

    /// Whether `code` is one the check expects.
    pub fn status_ok(&self, code: u16) -> bool {
        self.status.iter().any(|r| r.contains(&code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn error(pairs: &[(&str, &str)]) -> String {
        Spec::parse(&params(pairs)).unwrap_err().to_string()
    }

    #[test]
    fn a_url_alone_gets_the_defaults() {
        let spec = Spec::parse(&params(&[("url", "https://example.com/health")])).unwrap();
        assert_eq!(spec.url.as_str(), "https://example.com/health");
        assert_eq!(spec.method, Method::Get);
        assert!(spec.follow_redirects);
        assert!(spec.status_ok(200) && spec.status_ok(299) && !spec.status_ok(301));
        assert_eq!(spec.ip, IpFamily::Any);
        assert!(spec.headers.is_empty() && spec.username.is_none() && spec.password.is_none());
        let spec = Spec::parse(&params(&[
            ("url", "http://example.com"),
            ("follow_redirects", "false"),
        ]))
        .unwrap();
        assert!(spec.status_ok(302) && !spec.status_ok(404));
    }

    #[test]
    fn every_parameter_is_read() {
        let spec = Spec::parse(&params(&[
            ("url", "https://[2001:db8::1]:8443/a?b=c"),
            ("method", "head"),
            ("status", "200, 301-302,418"),
            ("headers", "X-Probe: 1\n\nAccept-Language: fr\n"),
            ("username", "monitor"),
            ("password", " s3cret "),
            ("ip", "6"),
        ]))
        .unwrap();
        assert_eq!(spec.method, Method::Head);
        assert_eq!(spec.status, vec![200..=200, 301..=302, 418..=418]);
        assert_eq!(
            spec.headers,
            vec![
                ("X-Probe".into(), "1".into()),
                ("Accept-Language".into(), "fr".into())
            ]
        );
        assert_eq!(spec.password.as_deref(), Some(" s3cret "));
        assert_eq!(spec.ip, IpFamily::V6);
    }

    #[test]
    fn bad_values_say_why() {
        assert!(error(&[]).contains("`url` is required"));
        assert!(error(&[("url", "ftp://x")]).contains("http://"));
        assert!(error(&[("url", "https://u:p@x")]).contains("user name or password"));
        assert!(error(&[("url", "nonsense")]).contains("not a URL"));
        assert!(error(&[("url", "http://x"), ("method", "POST")]).contains("GET"));
        assert!(error(&[("url", "http://x"), ("status", "2xx")]).contains("ranges"));
        assert!(error(&[("url", "http://x"), ("status", "300-200")]).contains("ranges"));
        assert!(error(&[("url", "http://x"), ("status", "99")]).contains("ranges"));
        assert!(
            error(&[("url", "http://x"), ("method", "HEAD"), ("contains", "a")])
                .contains("use GET")
        );
        assert!(error(&[("url", "http://x"), ("headers", "Host: y")]).contains("itself"));
        assert!(
            error(&[("url", "http://x"), ("headers", "Authorization: Bearer t")])
                .contains("sealed")
        );
        assert!(error(&[("url", "http://x"), ("headers", "no colon")]).contains("Name: value"));
        assert!(error(&[("url", "http://x"), ("headers", "Bad Name: v")]).contains("header name"));
        assert!(error(&[("url", "https://x"), ("username", "a")]).contains("required"));
        assert!(
            error(&[("url", "https://x"), ("username", "a:b"), ("password", "p")]).contains("`:`")
        );
        assert!(error(&[("url", "http://x"), ("password", "p")]).contains("https"));
        assert!(error(&[("url", "http://x"), ("ip", "5")]).contains("`any`"));
        assert!(error(&[("url", "http://10.0.0.1"), ("ip", "6")]).contains("other family"));
        assert!(error(&[("url", "http://[::1]"), ("ip", "4")]).contains("other family"));
        assert!(Spec::parse(&params(&[("url", "http://[::1]"), ("ip", "6")])).is_ok());
        assert!(error(&[("url", "http://x"), ("contains", "a\nb")]).contains("control"));
        assert!(error(&[("url", "http://x"), ("verify", "false")]).contains("unknown parameter"));
        let many = (0..17).map(|i| format!("X-{i}: v\n")).collect::<String>();
        assert!(error(&[("url", "http://x"), ("headers", &many)]).contains("at most 16"));
    }

    #[test]
    fn a_token_alone_is_allowed_over_https() {
        let spec = Spec::parse(&params(&[("url", "https://x"), ("password", "t")])).unwrap();
        assert!(spec.username.is_none());
        assert_eq!(spec.password.as_deref(), Some("t"));
    }
}
