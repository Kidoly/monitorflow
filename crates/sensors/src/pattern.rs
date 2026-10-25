//! Patterns set by the server for the `log` and `eventlog` sensors:
//! regular expressions of the `regex` crate on bytes, linear in the input
//! whatever the pattern, bounded in length and compiled size (ADR-0024).

use regex::bytes::{Regex, RegexBuilder};

/// Longest pattern, in bytes.
pub const MAX_PATTERN: usize = 1024;
/// Compiled size limit of a pattern, so `(a{1000}){1000}` is refused.
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

/// Compiles a pattern, bounded in length and compiled size.
pub fn pattern(text: &str) -> Result<Regex, String> {
    if text.is_empty() || text.len() > MAX_PATTERN {
        return Err(format!("must be 1 to {MAX_PATTERN} bytes"));
    }
    RegexBuilder::new(text)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("is not a valid pattern: {}", first_line(&e.to_string())))
}

fn first_line(text: &str) -> String {
    text.lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_bounded() {
        assert!(pattern("err(or)?").unwrap().is_match(b"an error"));
        assert!(pattern("").is_err());
        assert!(pattern(&"a".repeat(MAX_PATTERN + 1)).is_err());
        let error = pattern(r"(\w{1000}){1000}").unwrap_err();
        assert!(error.starts_with("is not a valid pattern"), "{error}");
        assert!(pattern("(").is_err());
    }
}
