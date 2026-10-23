//! Secret redaction of free text before it goes into AI context
//! (ADR-0040).
//!
//! [`Redactor::redact`] replaces every match of the rules below with
//! `[REDACTED:<kind>]` and counts replacements per [`Kind`]. Rules run in
//! a fixed order, most specific first, and never match a placeholder, so
//! one secret is counted once. The rules over-redact rather than miss:
//! a lost word of diagnosis costs less than a leaked credential.
//!
//! High-entropy strings ([`Kind::HighEntropy`]) are runs of at least
//! [`ENTROPY_MIN_LEN`] characters of `[A-Za-z0-9+/_-]` (plus `=` padding)
//! that:
//!
//! - mix upper case, lower case and digits, which keeps out hashes,
//!   UUIDs, Git and container IDs (hex, one case), package versions and
//!   most paths (lower case);
//! - do not start with `/`, an absolute path;
//! - have a Shannon entropy over their own characters of at least
//!   [`ENTROPY_MIN_BITS`] bits per character;
//! - change character class (upper, lower, digit, other) between at least
//!   [`MIN_CLASS_CHANGES`] of adjacent pairs. Entropy alone does not
//!   separate random text from long CamelCase identifiers
//!   (`AbstractSingletonProxyFactoryBean2Impl` scores 4.3 bits); their
//!   class changes, once per word, stay under 0.37 where random base62
//!   is around 0.6.
//!
//! Together they catch about 99 % of random 32-character base62 strings
//! and 99.7 % of 40-character ones. A hex secret without a telling key
//! name is not caught; base64 digests and SSH fingerprints are redacted,
//! a harmless false positive.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::Serialize;

/// Shortest run checked for entropy.
pub const ENTROPY_MIN_LEN: usize = 32;
/// Least Shannon entropy, in bits per character, of a redacted run. Random
/// base62 of 32 characters scores 4.5 on median, 4.1 at the 0.1 %
/// percentile.
pub const ENTROPY_MIN_BITS: f64 = 4.0;
/// Least share of adjacent character pairs of different classes.
pub const MIN_CLASS_CHANGES: f64 = 0.4;

/// What a redaction replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    PrivateKey,
    PemBlock,
    Jwt,
    BearerToken,
    BasicAuth,
    AwsAccessKey,
    AwsSecretKey,
    GcpApiKey,
    GcpOauthToken,
    AzureClientSecret,
    AzureSasSignature,
    GithubToken,
    GitlabToken,
    SlackToken,
    UrlPassword,
    ConnectionString,
    Password,
    Secret,
    Token,
    ApiKey,
    HighEntropy,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateKey => "private_key",
            Self::PemBlock => "pem_block",
            Self::Jwt => "jwt",
            Self::BearerToken => "bearer_token",
            Self::BasicAuth => "basic_auth",
            Self::AwsAccessKey => "aws_access_key",
            Self::AwsSecretKey => "aws_secret_key",
            Self::GcpApiKey => "gcp_api_key",
            Self::GcpOauthToken => "gcp_oauth_token",
            Self::AzureClientSecret => "azure_client_secret",
            Self::AzureSasSignature => "azure_sas_signature",
            Self::GithubToken => "github_token",
            Self::GitlabToken => "gitlab_token",
            Self::SlackToken => "slack_token",
            Self::UrlPassword => "url_password",
            Self::ConnectionString => "connection_string",
            Self::Password => "password",
            Self::Secret => "secret",
            Self::Token => "token",
            Self::ApiKey => "api_key",
            Self::HighEntropy => "high_entropy",
        }
    }

    fn placeholder(self) -> String {
        format!("[REDACTED:{}]", self.as_str())
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Redactions per kind, for the audit log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Redactions(BTreeMap<Kind, u32>);

impl Redactions {
    pub fn get(&self, kind: Kind) -> u32 {
        self.0.get(&kind).copied().unwrap_or(0)
    }

    pub fn total(&self) -> u32 {
        self.0.values().sum()
    }

    /// Kinds with at least one redaction, in [`Kind`] order.
    pub fn iter(&self) -> impl Iterator<Item = (Kind, u32)> + '_ {
        self.0.iter().map(|(k, n)| (*k, *n))
    }

    fn add(&mut self, kind: Kind) {
        *self.0.entry(kind).or_default() += 1;
    }
}

/// Text that went through a [`Redactor`]. Only this module builds one, so
/// a field of this type cannot hold unredacted input.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Redacted(String);

impl Redacted {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first `max` bytes, cut on a character boundary, then a marker
    /// with how many bytes were dropped. Cutting redacted text cannot
    /// reveal a secret, which is why redaction runs first.
    pub fn head(self, max: usize) -> Self {
        if self.0.len() <= max {
            return self;
        }
        let end = floor_char_boundary(&self.0, max);
        Self(format!(
            "{} [TRUNCATED: {} bytes]",
            &self.0[..end],
            self.0.len() - end
        ))
    }

    /// The last `max` bytes after a marker, like [`Self::head`]; for
    /// command output, whose end says how it failed.
    pub fn tail(self, max: usize) -> Self {
        if self.0.len() <= max {
            return self;
        }
        let mut start = self.0.len() - max;
        while !self.0.is_char_boundary(start) {
            start += 1;
        }
        Self(format!("[TRUNCATED: {} bytes] {}", start, &self.0[start..]))
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Redacts text and keeps the counts of everything it redacted.
#[derive(Debug, Default)]
pub struct Redactor {
    counts: Redactions,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn redact(&mut self, text: &str) -> Redacted {
        let mut out = text.to_owned();
        for rule in RULES.iter() {
            if rule.regex.is_match(&out) {
                out = rule.apply(&out, &mut self.counts);
            }
        }
        out = high_entropy(&out, &mut self.counts);
        Redacted(out)
    }

    pub fn redactions(&self) -> &Redactions {
        &self.counts
    }

    pub fn into_redactions(self) -> Redactions {
        self.counts
    }
}

/// Which part of a match a rule replaces.
enum Target {
    /// The whole match.
    All(Kind),
    /// Capture group 2; groups 1 and 3 are kept. The kind comes from
    /// group 1 (a key name, for instance).
    Value(fn(&str) -> Option<Kind>),
}

struct Rule {
    regex: Regex,
    target: Target,
}

impl Rule {
    fn all(pattern: &str, kind: Kind) -> Self {
        Self {
            regex: Regex::new(pattern).expect("valid redaction pattern"),
            target: Target::All(kind),
        }
    }

    fn value(pattern: &str, kind: fn(&str) -> Option<Kind>) -> Self {
        Self {
            regex: Regex::new(pattern).expect("valid redaction pattern"),
            target: Target::Value(kind),
        }
    }

    fn apply(&self, text: &str, counts: &mut Redactions) -> String {
        self.regex
            .replace_all(text, |caps: &Captures| {
                let whole = &caps[0];
                match self.target {
                    Target::All(kind) => {
                        let kind = refine(kind, whole);
                        counts.add(kind);
                        kind.placeholder()
                    }
                    Target::Value(classify) => {
                        let value = caps.get(2).map_or("", |m| m.as_str());
                        let kind = classify(&caps[1]).filter(|_| is_secret_value(value));
                        let Some(kind) = kind else {
                            return whole.to_owned();
                        };
                        counts.add(kind);
                        let tail = caps.get(3).map_or("", |m| m.as_str());
                        format!("{}{}{tail}", &caps[1], kind.placeholder())
                    }
                }
            })
            .into_owned()
    }
}

/// A PEM block is a private key when its label says so.
fn refine(kind: Kind, matched: &str) -> Kind {
    if kind == Kind::PemBlock
        && matched
            .lines()
            .next()
            .is_some_and(|l| l.contains("PRIVATE"))
    {
        Kind::PrivateKey
    } else {
        kind
    }
}

/// Values that are not secrets themselves: empty, masked, placeholders and
/// references to variables (`$DB_PASSWORD`, `${X}`, `%(pw)s`).
fn is_secret_value(value: &str) -> bool {
    let bare = value.trim_matches(|c| c == '"' || c == '\'').trim();
    let lower = bare.to_ascii_lowercase();
    !(bare.is_empty()
        || bare.starts_with("[REDACTED:")
        || bare.starts_with('$')
        || bare.starts_with("%(")
        || bare.chars().all(|c| c == '*' || c == 'x' || c == 'X')
        || matches!(
            lower.as_str(),
            "null" | "none" | "nil" | "true" | "false" | "<redacted>" | "redacted" | "[redacted]"
        ))
}

/// The kind of a `key=value` pair from its key, or `None` to keep it.
fn key_kind(key: &str) -> Option<Kind> {
    let key = key.to_ascii_lowercase();
    Some(if key.contains("aws") && key.contains("secret") {
        Kind::AwsSecretKey
    } else if key.contains("pass") || key.contains("pwd") {
        Kind::Password
    } else if key.contains("secret") || key.contains("credential") || key.contains("private") {
        Kind::Secret
    } else if key.contains("token") {
        Kind::Token
    } else {
        Kind::ApiKey
    })
}

fn connection_kind(_: &str) -> Option<Kind> {
    Some(Kind::ConnectionString)
}

fn url_kind(_: &str) -> Option<Kind> {
    Some(Kind::UrlPassword)
}

fn bearer_kind(_: &str) -> Option<Kind> {
    Some(Kind::BearerToken)
}

fn basic_kind(_: &str) -> Option<Kind> {
    Some(Kind::BasicAuth)
}

fn sas_kind(_: &str) -> Option<Kind> {
    Some(Kind::AzureSasSignature)
}

/// In order: whole blocks, then formats with a known prefix, then
/// credentials in URLs, headers and connection strings, then key-value
/// pairs. High entropy runs last, over what is left.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        // A whole PEM block, or a truncated one up to the end of the text.
        // `\n` may be literal (a key inside JSON).
        Rule::all(
            r"(?s)-----BEGIN ([A-Z0-9 ]{1,64})-----.*?(?:-----END [A-Z0-9 ]{1,64}-----|\z)",
            Kind::PemBlock,
        ),
        Rule::all(
            r"\beyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*",
            Kind::Jwt,
        ),
        Rule::all(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", Kind::AwsAccessKey),
        Rule::all(r"\bAIza[0-9A-Za-z_-]{35}", Kind::GcpApiKey),
        Rule::all(r"\bya29\.[0-9A-Za-z_-]{20,}", Kind::GcpOauthToken),
        Rule::all(
            r"[A-Za-z0-9_~.]{3}[0-9]Q~[A-Za-z0-9_~.-]{31,34}",
            Kind::AzureClientSecret,
        ),
        Rule::all(
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})",
            Kind::GithubToken,
        ),
        Rule::all(
            r"\b(?:gl(?:pat|dt|ptt|rt|cbt|soat|ffct|imt|agent)-[A-Za-z0-9_-]{20,}|GR1348941[A-Za-z0-9_-]{20,})",
            Kind::GitlabToken,
        ),
        Rule::all(r"\bxox[abposr]-[A-Za-z0-9-]{10,}", Kind::SlackToken),
        // scheme://user:password@host
        Rule::value(
            r"(\b[A-Za-z][A-Za-z0-9+.-]{1,30}://[^\s:/@\[\]]*:)([^\s@/]+)(@)",
            url_kind,
        ),
        Rule::value(r"(?i)([?&]sig=)([A-Za-z0-9%+/=]{16,})()", sas_kind),
        Rule::value(
            r"(?i)(\bauthorization\s*[:=]\s*basic\s+)([A-Za-z0-9+/]{8,}=*)()",
            basic_kind,
        ),
        // At least 16 characters, so prose like "bearer authentication"
        // is left alone.
        Rule::value(
            r"(?i)(\bbearer\s+)([A-Za-z0-9._~+/-]{16,}=*)()",
            bearer_kind,
        ),
        // `Server=db;User Id=sa;Password=p a s s;`: the value runs to the
        // next `;`, spaces included.
        Rule::value(
            r#"(?i)((?:^|[;\s"'])(?:password|pwd|accountkey|sharedaccesskey|sharedaccesssignature)\s*=\s*)([^;"'\r\n]+)(;)"#,
            connection_kind,
        ),
        // `DB_PASSWORD=x`, `"api_key": "x"`, `X-Auth-Token: x`, `?token=x`:
        // a key ending in one of these words, `=` or `:`, then a quoted
        // or bare value.
        Rule::value(
            r#"(?i)(\b[a-z0-9_.-]*?(?:password|passwd|passphrase|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|auth[_-]?key|credentials?)["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"',;&]+)()"#,
            key_kind,
        ),
    ]
});

static RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_-]+=*").expect("valid run pattern"));

fn high_entropy(text: &str, counts: &mut Redactions) -> String {
    RUNS.replace_all(text, |caps: &Captures| {
        let run = &caps[0];
        if looks_random(run) {
            counts.add(Kind::HighEntropy);
            Kind::HighEntropy.placeholder()
        } else {
            run.to_owned()
        }
    })
    .into_owned()
}

/// The high-entropy rule of the module documentation.
pub fn looks_random(run: &str) -> bool {
    let body = run.trim_end_matches('=');
    body.len() >= ENTROPY_MIN_LEN
        && !body.starts_with('/')
        && body.bytes().any(|b| b.is_ascii_uppercase())
        && body.bytes().any(|b| b.is_ascii_lowercase())
        && body.bytes().any(|b| b.is_ascii_digit())
        && entropy(body) >= ENTROPY_MIN_BITS
        && class_changes(body) >= MIN_CLASS_CHANGES
}

/// The share of adjacent characters of `text` in different classes:
/// upper case, lower case, digit, other.
pub fn class_changes(text: &str) -> f64 {
    let class = |b: u8| match b {
        b'A'..=b'Z' => 0,
        b'a'..=b'z' => 1,
        b'0'..=b'9' => 2,
        _ => 3,
    };
    let bytes = text.as_bytes();
    if bytes.len() < 2 {
        return 0.0;
    }
    let changes = bytes
        .windows(2)
        .filter(|w| class(w[0]) != class(w[1]))
        .count();
    changes as f64 / (bytes.len() - 1) as f64
}

/// Shannon entropy of the characters of `text`, in bits per character.
pub fn entropy(text: &str) -> f64 {
    let mut freq = [0u32; 256];
    for b in text.bytes() {
        freq[usize::from(b)] += 1;
    }
    let len = text.len() as f64;
    freq.iter()
        .filter(|&&n| n > 0)
        .map(|&n| {
            let p = f64::from(n) / len;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines `+ <kind>[,<kind>...] | <text>` must redact exactly those
    /// kinds and lose every `@@`-marked secret; lines `- | <text>` must
    /// come out unchanged. `\n` in a text is a newline.
    const CORPUS: &str = include_str!("redact_corpus.txt");

    fn cases() -> impl Iterator<Item = (usize, &'static str, String)> {
        CORPUS.lines().enumerate().filter_map(|(n, line)| {
            if line.trim().is_empty() || line.starts_with('#') {
                return None;
            }
            let (head, text) = line.split_once(" | ").expect("`<head> | <text>`");
            let text = text.replace("\\n", "\n").replace("{PRIVATE}", "PRIVATE");
            Some((n + 1, head.trim(), text))
        })
    }

    #[test]
    fn corpus() {
        let mut checked = 0;
        for (line, head, text) in cases() {
            let mut redactor = Redactor::new();
            let secrets: Vec<&str> = text.split("@@").skip(1).step_by(2).collect();
            let input = text.replace("@@", "");
            let out = redactor.redact(&input);
            if head == "-" {
                assert_eq!(out.as_str(), input, "line {line}: false positive");
                assert_eq!(redactor.redactions().total(), 0, "line {line}");
            } else {
                let mut kinds: Vec<&str> = head.trim_start_matches("+ ").split(',').collect();
                kinds.sort_unstable();
                let mut got: Vec<&str> = redactor
                    .redactions()
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect();
                got.sort_unstable();
                assert_eq!(got, kinds, "line {line}: kinds of {:?}", out.as_str());
                for secret in secrets {
                    assert!(
                        !out.as_str().contains(secret),
                        "line {line}: `{secret}` left in {:?}",
                        out.as_str()
                    );
                }
                for kind in kinds {
                    assert!(
                        out.as_str().contains(&format!("[REDACTED:{kind}]")),
                        "line {line}"
                    );
                }
            }
            checked += 1;
        }
        assert!(checked > 40, "corpus has {checked} cases");
    }

    #[test]
    fn redacting_twice_changes_nothing_more() {
        for (_, _, text) in cases() {
            let once = Redactor::new().redact(&text.replace("@@", ""));
            let mut again = Redactor::new();
            assert_eq!(again.redact(once.as_str()), once);
            assert_eq!(again.redactions().total(), 0, "{:?}", once.as_str());
        }
    }

    #[test]
    fn counts_add_up_per_kind() {
        let mut redactor = Redactor::new();
        redactor.redact("password=a1 password=b2");
        redactor.redact("DB_PASSWORD=c3 token=d4");
        let counts = redactor.into_redactions();
        assert_eq!(counts.get(Kind::Password), 3);
        assert_eq!(counts.get(Kind::Token), 1);
        assert_eq!(counts.get(Kind::Jwt), 0);
        assert_eq!(counts.total(), 4);
        assert_eq!(
            serde_json::to_value(&counts).unwrap(),
            serde_json::json!({"password": 3, "token": 1})
        );
    }

    #[test]
    fn truncation_marks_what_it_drops_on_char_boundaries() {
        let text = Redactor::new().redact("ééé abc");
        assert_eq!(text.clone().head(100), text);
        assert_eq!(text.clone().head(3).as_str(), "é [TRUNCATED: 8 bytes]");
        assert_eq!(text.clone().tail(4).as_str(), "[TRUNCATED: 6 bytes]  abc");
        assert_eq!(text.tail(5).as_str(), "[TRUNCATED: 6 bytes]  abc");
    }

    #[test]
    fn entropy_separates_random_from_words() {
        assert_eq!(entropy("aaaa"), 0.0);
        assert!((entropy("abcd") - 2.0).abs() < 1e-9);
        assert!(looks_random("Zq3vT9xKpL2mW8rB5nY7cF4hJ6dG1sA0"));
        // One case only: hex digests.
        assert!(!looks_random(
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        ));
        // Identifiers: high entropy, few class changes.
        assert!(entropy("AbstractSingletonProxyFactoryBean2Impl") > ENTROPY_MIN_BITS);
        assert!(!looks_random("AbstractSingletonProxyFactoryBean2Impl"));
        assert!(!looks_random("Win32ProcessStartupInformationEx64"));
        // A path.
        assert!(!looks_random(
            "/opt/App2/Releases/Build7/Current/Bin/Server"
        ));
    }
}
