//! Helpers that keep secret values out of logs and error messages.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

/// Placeholder printed instead of a secret value.
pub const REDACTED: &str = "[redacted]";

/// A string whose `Debug` output never shows the value.
/// Call [`Secret::expose`] only where the value is actually used.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{REDACTED}\"")
    }
}

impl FromStr for Secret {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::new(s))
    }
}

/// Returns `url` with the password in its userinfo and any `password=`
/// query parameter replaced by [`REDACTED`].
pub fn redact_url_password(url: &str) -> String {
    let mut out = url.to_owned();
    if let Some(scheme_end) = out.find("://") {
        let rest = scheme_end + 3;
        let authority_end = out[rest..]
            .find(['/', '?', '#'])
            .map_or(out.len(), |i| rest + i);
        if let Some(at) = out[rest..authority_end].rfind('@') {
            let userinfo = &out[rest..rest + at];
            if let Some(colon) = userinfo.find(':') {
                out.replace_range(rest + colon + 1..rest + at, REDACTED);
            }
        }
    }
    if let Some(query_start) = out.find('?') {
        let (head, query) = out.split_at(query_start + 1);
        let query = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((k, _)) if k.eq_ignore_ascii_case("password") => format!("{k}={REDACTED}"),
                _ => pair.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("&");
        out = format!("{head}{query}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_hides_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "\"[redacted]\"");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn redacts_userinfo_password() {
        assert_eq!(
            redact_url_password("postgres://mf:hunter2@db:5432/mf"),
            "postgres://mf:[redacted]@db:5432/mf"
        );
    }

    #[test]
    fn redacts_query_password() {
        assert_eq!(
            redact_url_password("postgres://db/mf?user=mf&password=hunter2&sslmode=require"),
            "postgres://db/mf?user=mf&password=[redacted]&sslmode=require"
        );
    }

    #[test]
    fn leaves_urls_without_password_alone() {
        for url in ["postgres://mf@db/mf", "postgres://db/mf", "not a url"] {
            assert_eq!(redact_url_password(url), url);
        }
    }
}
