//! Word patterns of administrator rules (ADR-0041).
//!
//! A pattern is a list of words separated by spaces. Each word matches one
//! word of the command, where `*` stands for any run of characters and `?`
//! for one character; the word `**` alone matches any number of words,
//! none included. There is no quoting: patterns are ASCII without control
//! characters, and a word of the command matches by its text after the
//! shell's quote removal.

use std::fmt;

/// Longest accepted pattern, in bytes.
pub const MAX_PATTERN_LEN: usize = 512;

/// Most words in a pattern.
const MAX_WORDS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    words: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("pattern must not be empty")]
    Empty,
    #[error("pattern is longer than {MAX_PATTERN_LEN} bytes or {MAX_WORDS} words")]
    TooLong,
    #[error("pattern must be printable ASCII")]
    Charset,
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Self, PatternError> {
        if text.len() > MAX_PATTERN_LEN {
            return Err(PatternError::TooLong);
        }
        if !text.chars().all(|c| c == ' ' || c.is_ascii_graphic()) {
            return Err(PatternError::Charset);
        }
        let words: Vec<String> = text
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect();
        if words.is_empty() {
            return Err(PatternError::Empty);
        }
        if words.len() > MAX_WORDS {
            return Err(PatternError::TooLong);
        }
        Ok(Self { words })
    }

    /// The first word, which names the program for allow rules.
    pub fn program(&self) -> &str {
        &self.words[0]
    }

    /// Whether the pattern matches all of `words`.
    pub fn matches_all<S: AsRef<str>>(&self, words: &[S]) -> bool {
        seq(&self.words, words, false, false)
    }

    /// Whether the pattern matches some run of consecutive `words`.
    pub fn matches_within<S: AsRef<str>>(&self, words: &[S]) -> bool {
        seq(&self.words, words, true, true)
    }

    /// The pattern as one line, words separated by single spaces.
    pub fn text(&self) -> String {
        self.words.join(" ")
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

/// Matches `pattern` against `words`, by dynamic programming so that
/// `**` stays polynomial. With `anywhere`, the match may start after the
/// first word; with `prefix`, words may remain after it.
fn seq<S: AsRef<str>>(pattern: &[String], words: &[S], anywhere: bool, prefix: bool) -> bool {
    // reach[j]: the pattern words seen so far can end just before word j.
    let mut reach: Vec<bool> = (0..=words.len()).map(|j| j == 0 || anywhere).collect();
    for p in pattern {
        let mut next = vec![false; words.len() + 1];
        if p == "**" {
            let mut any = false;
            for j in 0..=words.len() {
                any |= reach[j];
                next[j] = any;
            }
        } else {
            for j in 0..words.len() {
                next[j + 1] = reach[j] && glob(p.as_bytes(), words[j].as_ref().as_bytes());
            }
        }
        reach = next;
        if !reach.contains(&true) {
            return false;
        }
    }
    if prefix {
        reach.contains(&true)
    } else {
        reach[words.len()]
    }
}

/// `*` and `?` glob of one word, iterative with backtracking on the last
/// star, so it stays linear in practice.
pub fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Pattern {
        Pattern::parse(text).unwrap()
    }

    #[test]
    fn globs_match_within_one_word() {
        for (pattern, text, expected) in [
            ("*", "", true),
            ("*", "anything", true),
            ("mkfs*", "mkfs.ext4", true),
            ("mkfs*", "xmkfs", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("*.log", "/var/log/syslog.log", true),
            ("*a*b*", "xxaxxbxx", true),
            ("*a*b", "xxaxxbxxc", false),
            ("", "", true),
            ("", "x", false),
        ] {
            assert_eq!(
                glob(pattern.as_bytes(), text.as_bytes()),
                expected,
                "{pattern} {text}"
            );
        }
    }

    #[test]
    fn words_match_one_to_one_and_double_star_spans_words() {
        let words = ["systemctl", "restart", "nginx"];
        assert!(p("systemctl restart *").matches_all(&words));
        assert!(p("systemctl **").matches_all(&words));
        assert!(p("** nginx").matches_all(&words));
        assert!(p("systemctl ** restart **").matches_all(&words));
        assert!(!p("systemctl restart").matches_all(&words));
        assert!(!p("systemctl *").matches_all(&words));
        assert!(p("restart nginx").matches_within(&words));
        assert!(p("restart").matches_within(&words));
        assert!(!p("nginx restart").matches_within(&words));
        assert!(p("rm ** /").matches_within(&["sudo", "rm", "-r", "-f", "/"]));
        assert!(!p("x").matches_within::<&str>(&[]));
    }

    #[test]
    fn parsing_rejects_empty_long_and_non_ascii_patterns() {
        assert_eq!(Pattern::parse("   "), Err(PatternError::Empty));
        assert_eq!(Pattern::parse("rm\t-rf"), Err(PatternError::Charset));
        assert_eq!(Pattern::parse("rм"), Err(PatternError::Charset));
        assert_eq!(Pattern::parse(&"a".repeat(513)), Err(PatternError::TooLong));
        assert_eq!(Pattern::parse(&"a ".repeat(65)), Err(PatternError::TooLong));
        assert_eq!(p("  rm   -rf  / ").text(), "rm -rf /");
        assert_eq!(p("rm -rf /").program(), "rm");
    }
}
