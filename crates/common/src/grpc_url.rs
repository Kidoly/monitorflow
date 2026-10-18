//! The one parser for gRPC upstream URLs, shared by config validation and
//! `tls::endpoint` so both apply the same rule.

use std::fmt;

use tonic::codegen::http::Uri;

/// Port used when an upstream URL does not give one.
pub const DEFAULT_GRPC_PORT: u16 = 7443;

/// A validated `https://host[:port][/]` upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcUrl {
    /// Host name or IP address, without brackets for IPv6.
    pub host: String,
    pub port: u16,
}

impl GrpcUrl {
    /// Parses `url`, which must be `https` with a non-empty host and no user
    /// info, path (other than `/`), query or fragment. The error is a short
    /// reason, for the caller to wrap with the offending key or URL.
    pub fn parse(url: &str) -> Result<Self, String> {
        if url.contains('#') {
            return Err("fragment is not allowed".into());
        }
        let uri: Uri = url.parse().map_err(|_| "not a URL".to_owned())?;
        if uri.scheme_str() != Some("https") {
            return Err("scheme must be https, gRPC is always mTLS".into());
        }
        let authority = uri.authority().ok_or("missing host")?;
        if authority.as_str().contains('@') {
            return Err("user info is not allowed".into());
        }
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        if host.is_empty() {
            return Err("missing host".into());
        }
        if !matches!(uri.path(), "" | "/") {
            return Err("path is not allowed".into());
        }
        if uri.query().is_some() {
            return Err("query is not allowed".into());
        }
        Ok(Self {
            host: host.to_owned(),
            port: authority.port_u16().unwrap_or(DEFAULT_GRPC_PORT),
        })
    }

    /// `host:port`, with IPv6 addresses in brackets.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for GrpcUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "https://{}", self.authority())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_and_port() {
        let url = GrpcUrl::parse("https://relay.example.com:8443").unwrap();
        assert_eq!((url.host.as_str(), url.port), ("relay.example.com", 8443));
        assert_eq!(url.to_string(), "https://relay.example.com:8443");
    }

    #[test]
    fn default_port_and_trailing_slash() {
        let url = GrpcUrl::parse("https://relay/").unwrap();
        assert_eq!(url.authority(), "relay:7443");
    }

    #[test]
    fn ipv6_host_loses_brackets() {
        let url = GrpcUrl::parse("https://[::1]:9000").unwrap();
        assert_eq!(url.host, "::1");
        assert_eq!(url.authority(), "[::1]:9000");
    }

    #[test]
    fn rejects_everything_else() {
        for (url, reason) in [
            ("not a url", "not a URL"),
            ("http://relay:7443", "scheme must be https"),
            ("relay:7443", "scheme must be https"),
            ("https://", "not a URL"),
            ("https://:7443", "missing host"),
            ("https://user:pw@relay", "user info is not allowed"),
            ("https://relay/prefix", "path is not allowed"),
            ("https://relay/?a=b", "query is not allowed"),
            ("https://relay#frag", "fragment is not allowed"),
        ] {
            let err = GrpcUrl::parse(url).unwrap_err();
            assert!(err.starts_with(reason), "{url}: {err}");
        }
    }
}
