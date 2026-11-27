//! The HTTP client of the cloud checks: HTTPS only, answers bounded, and
//! failures sorted into the `reason` of `up`.

use std::time::Duration;

use reqwest::StatusCode;

/// Most bytes of one answer. A page of a thousand instances is a few
/// megabytes.
pub const MAX_ANSWER: usize = 32 << 20;
/// Longest error text kept.
const MAX_ERROR: usize = 256;

/// Why a call failed, as the `reason` of `up`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Nothing answered, or not in time.
    Connect(String),
    /// The credentials were refused.
    Login(String),
    /// The credentials work but lack a right.
    Permission(String),
    /// Anything else the API said, or an answer that does not read.
    Api(String),
}

impl Failure {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Connect(_) => "connect",
            Self::Login(_) => "login",
            Self::Permission(_) => "permission",
            Self::Api(_) => "api",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Connect(m) | Self::Login(m) | Self::Permission(m) | Self::Api(m) => m,
        }
    }

    /// The same failure, its message after `prefix`.
    pub fn within(self, prefix: &str) -> Self {
        let wrap = |m: String| bounded(&format!("{prefix}: {m}"));
        match self {
            Self::Connect(m) => Self::Connect(wrap(m)),
            Self::Login(m) => Self::Login(wrap(m)),
            Self::Permission(m) => Self::Permission(wrap(m)),
            Self::Api(m) => Self::Api(wrap(m)),
        }
    }
}

/// `text` cut to [`MAX_ERROR`] characters, control characters removed.
pub fn bounded(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_ERROR)
        .collect()
}

/// An HTTPS client for one check.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    /// A client whose requests last at most `timeout`. `https_only` is
    /// false only for tests against a local fake API.
    pub fn new(timeout: Duration, https_only: bool) -> Result<Self, Failure> {
        let client = reqwest::Client::builder()
            .https_only(https_only)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(10)))
            .user_agent(concat!("MonitorFlow/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Failure::Connect(bounded(&e.to_string())))?;
        Ok(Self { client })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Sends `request` and reads its answer, at most [`MAX_ANSWER`] bytes.
    pub async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<(StatusCode, Vec<u8>), Failure> {
        let mut answer = request.send().await.map_err(|error| {
            let text = bounded(&chain(&error));
            if error.is_timeout() {
                Failure::Connect(format!("no answer in time: {text}"))
            } else {
                Failure::Connect(text)
            }
        })?;
        let status = answer.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = answer
            .chunk()
            .await
            .map_err(|e| Failure::Connect(bounded(&chain(&e))))?
        {
            if bytes.len() + chunk.len() > MAX_ANSWER {
                return Err(Failure::Api("the answer is too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, bytes))
    }
}

/// `base` is a test server on this machine: `http://127.0.0.1:port`,
/// `http://localhost:port` or `http://[::1]:port`. Nothing else may stand
/// in for a provider's endpoints.
pub fn is_local(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    host == "127.0.0.1" || host == "localhost" || rest.starts_with("[::1]")
}

/// An error and its sources, which say what actually failed.
fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(inner) = source {
        let more = format!(": {inner}");
        if !text.contains(&inner.to_string()) {
            text.push_str(&more);
        }
        source = inner.source();
    }
    text
}

/// Percent-encodes `value` as SigV4 and form bodies want it: everything
/// but `A-Z a-z 0-9 - _ . ~`.
pub fn encode(value: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, SET).to_string()
}

/// `pairs` as a form body, in order.
pub fn form(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_keep_their_reason_and_are_bounded() {
        let failure = Failure::Permission("no".into()).within("eu-west-3");
        assert_eq!(failure.reason(), "permission");
        assert_eq!(failure.message(), "eu-west-3: no");
        assert_eq!(bounded(&"é\n".repeat(400)).chars().count(), 256);
        assert!(!bounded("a\nb").contains('\n'));
    }

    #[test]
    fn values_are_encoded_as_aws_wants() {
        assert_eq!(
            encode("a b/c~d-e_f.g+h=é"),
            "a%20b%2Fc~d-e_f.g%2Bh%3D%C3%A9"
        );
        assert_eq!(
            form(&[
                ("Action".into(), "Describe".into()),
                ("x y".into(), "1".into())
            ]),
            "Action=Describe&x%20y=1"
        );
    }
}
