//! The secret of a one-time link: the setup link (ADR-0057) and password
//! links (ADR-0059). Only its SHA-256 is stored.

use std::fmt;

use monitorflow_common::redact::REDACTED;
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};

const TOKEN_BYTES: usize = 32;

/// 256 random bits as lowercase hex. `Debug` never shows them.
#[derive(Clone, PartialEq, Eq)]
pub struct OneTimeToken(String);

impl fmt::Debug for OneTimeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OneTimeToken({REDACTED})")
    }
}

impl OneTimeToken {
    pub(super) fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system random generator failed");
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Accepts only the exact format [`OneTimeToken::generate`] produces.
    pub(super) fn parse(value: &str) -> Option<Self> {
        let valid = value.len() == 2 * TOKEN_BYTES
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid.then(|| Self(value.to_owned()))
    }

    pub(super) fn hash(&self) -> Vec<u8> {
        digest(&SHA256, self.0.as_bytes()).as_ref().to_vec()
    }

    /// The secret itself, for the one response or log line that hands it
    /// out.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `<web_base_url><page>#<secret>`. The secret is in the fragment,
    /// which browsers never send, so it stays out of the reverse proxy's
    /// access log.
    pub fn url(&self, web_base_url: &str, page: &str) -> String {
        format!("{web_base_url}{page}#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex_and_parse_strictly() {
        let token = OneTimeToken::generate();
        assert_eq!(token.0.len(), 64);
        assert_ne!(token, OneTimeToken::generate());
        assert_eq!(OneTimeToken::parse(&token.0), Some(token.clone()));
        for bad in ["", "abc", &"A".repeat(64), &"0".repeat(63), &"0".repeat(65)] {
            assert_eq!(OneTimeToken::parse(bad), None, "{bad}");
        }
        assert_eq!(token.hash().len(), 32);
        assert!(!format!("{token:?}").contains(&token.0));
    }

    #[test]
    fn the_secret_is_in_the_fragment() {
        let token = OneTimeToken::parse(&"ab".repeat(32)).unwrap();
        assert_eq!(
            token.url("https://mon.example.net", "/setup/"),
            format!("https://mon.example.net/setup/#{}", "ab".repeat(32))
        );
    }
}
