//! What readers share to describe a website check (ADR-0150): text
//! conditions that other tools write as regular expressions, lists of
//! status codes, and the address of a certificate check.

use crate::args;
use crate::notes::Note;

/// Longest text a website check looks for, as the check accepts it.
pub const MAX_TEXT: usize = 256;

/// The plain text a regular expression matches, when it is one: `Welcome`
/// or `Order\ placed` read as text, `^OK$` or `a.b` not. `None` for
/// anything a plain text search would not do the same.
pub fn literal(pattern: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let escaped = chars.next()?;
                // An escaped punctuation character is itself; `\d`, `\s`
                // and the like are classes.
                if escaped.is_ascii_punctuation() || escaped == ' ' {
                    out.push(escaped);
                } else {
                    return None;
                }
            }
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
                return None
            }
            other => out.push(other),
        }
    }
    text(&out)
}

/// `text` when a website check may look for it: not empty, at most
/// [`MAX_TEXT`] bytes, without control characters.
pub fn text(text: &str) -> Option<String> {
    (!text.is_empty() && text.len() <= MAX_TEXT && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

/// A text condition: the text when `regex` is false and it fits, the
/// literal a regular expression is, or the note saying why it is not
/// moved.
pub fn condition(pattern: &str, regex: bool, setting: &str) -> Result<String, Note> {
    if regex {
        return literal(pattern)
            .ok_or_else(|| Note::new("web_regex", args! {"pattern" => pattern}));
    }
    text(pattern).ok_or_else(|| Note::new("web_setting", args! {"setting" => setting}))
}

/// Status codes and ranges as a website check reads them, `200,301-302`,
/// from a list written with commas and optional spaces. `None` when it is
/// not one: a macro, a word.
pub fn status_list(text: &str) -> Option<String> {
    let code = |s: &str| {
        s.trim()
            .parse::<u16>()
            .ok()
            .filter(|c| (100..=599).contains(c))
    };
    let mut parts = Vec::new();
    for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        match item.split_once('-') {
            Some((low, high)) => {
                let (low, high) = (code(low)?, code(high)?);
                if low > high {
                    return None;
                }
                parts.push(format!("{low}-{high}"));
            }
            None => parts.push(code(item)?.to_string()),
        }
    }
    (!parts.is_empty() && parts.len() <= 32).then(|| parts.join(","))
}

/// Status codes as a list of numbers, such as the blackbox exporter's.
pub fn status_codes(codes: &[u16]) -> Option<String> {
    status_list(
        &codes
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// `https://host:port/`, the port left out when it is 443, an IPv6
/// address in brackets.
pub fn https_url(host: &str, port: u16) -> String {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    if port == 443 {
        format!("https://{host}/")
    } else {
        format!("https://{host}:{port}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(literal("Welcome").as_deref(), Some("Welcome"));
        assert_eq!(literal("Order\\ placed").as_deref(), Some("Order placed"));
        assert_eq!(literal("1\\.2").as_deref(), Some("1.2"));
        assert_eq!(literal("status: ok").as_deref(), Some("status: ok"));
        assert_eq!(literal("^OK$"), None);
        assert_eq!(literal("a.b"), None);
        assert_eq!(literal("\\d+"), None);
        assert_eq!(literal("ok|fine"), None);
        assert_eq!(literal(""), None);
        assert_eq!(literal(&"x".repeat(300)), None);
    }

    #[test]
    fn conditions() {
        assert_eq!(condition("a.b", false, "required"), Ok("a.b".into()));
        assert_eq!(
            condition("a.b", true, "required").unwrap_err(),
            Note::new("web_regex", args! {"pattern" => "a.b"})
        );
        assert_eq!(
            condition("", false, "required").unwrap_err().code,
            "web_setting"
        );
    }

    #[test]
    fn statuses() {
        assert_eq!(status_list("200").as_deref(), Some("200"));
        assert_eq!(
            status_list("200, 201,210-299").as_deref(),
            Some("200,201,210-299")
        );
        assert_eq!(status_list("{$CODES}"), None);
        assert_eq!(status_list("299-200"), None);
        assert_eq!(status_list("700"), None);
        assert_eq!(status_list(""), None);
        assert_eq!(status_codes(&[200, 301]).as_deref(), Some("200,301"));
        assert_eq!(status_codes(&[]), None);
    }

    #[test]
    fn certificate_addresses() {
        assert_eq!(
            https_url("shop.example.com", 443),
            "https://shop.example.com/"
        );
        assert_eq!(
            https_url("shop.example.com", 8443),
            "https://shop.example.com:8443/"
        );
        assert_eq!(https_url("fd00::1", 443), "https://[fd00::1]/");
        assert_eq!(https_url("[fd00::1]", 8443), "https://[fd00::1]:8443/");
    }
}
