//! Risk classification of proposed commands (architecture §8, ADR-0041).
//!
//! [`classify`] puts a shell command line into a [`Tier`] by rules only:
//! built-in allow-lists and deny patterns, plus the [`RuleSet`] an
//! administrator configured for the customer and site. The AI's own
//! opinion of a command is never an input. Anything the rules do not
//! recognize is high impact.
//!
//! The steps, in order (the first that decides wins):
//!
//! 1. The command is trimmed of surrounding whitespace, newlines included.
//!    Empty or longer than [`MAX_COMMAND_LEN`]: high impact.
//! 2. Deny patterns, administrator ones then built-in ones, are matched
//!    leniently anywhere in the command, inside quoted strings and
//!    substitutions too: forbidden. Deny always wins.
//! 3. Non-ASCII characters (lookalikes) or control characters: high.
//! 4. The strict lexer ([`lexer::lex`]) refuses command lists,
//!    substitutions, expansions, redirections, here-documents, subshells,
//!    groups, comments, `!`, `~` and broken quoting: high.
//! 5. Each pipeline stage starting with `NAME=value` (environment
//!    prefix): high.
//! 6. The first stage goes through the built-in rules ([`builtin`]);
//!    every later stage must be a built-in read-only filter fed by the
//!    pipe (`grep`, `head`, `tail`, `sort`, `uniq`, `wc`, `cut`, `tr`),
//!    else high.
//! 7. An unquoted glob anywhere outside the file operands of the
//!    read-only log and `/proc` readers: high.
//! 8. A command no built-in rule knows takes the tier of the most
//!    cautious matching administrator allow rule, else high
//!    (`default.unknown`). Allow rules never lower a command the built-in
//!    rules classified, so they cannot unlock shells, `sudo`, packages,
//!    `kill` or reboots.

mod builtin;
mod lexer;
mod pattern;
pub mod store;
#[cfg(test)]
mod tests;

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use pattern::{Pattern, PatternError, MAX_PATTERN_LEN};

/// Longest command classified, in bytes; longer ones are high impact.
pub const MAX_COMMAND_LEN: usize = 4096;

/// Risk tiers of architecture §8, from least to most dangerous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Runs automatically.
    ReadOnly,
    /// Needs approval unless the server group runs them automatically.
    LowImpact,
    /// Always needs approval.
    HighImpact,
    /// Never runs.
    Forbidden,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::LowImpact => "low_impact",
            Self::HighImpact => "high_impact",
            Self::Forbidden => "forbidden",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::ReadOnly,
            Self::LowImpact,
            Self::HighImpact,
            Self::Forbidden,
        ]
        .into_iter()
        .find(|t| t.as_str() == text)
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The rule that decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Rule {
    /// A built-in rule, by its stable ID such as `readonly.df`.
    Builtin { id: &'static str },
    /// A row of `remediation_rules`.
    Configured { id: Uuid },
}

/// What [`classify`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub tier: Tier,
    pub rule: Rule,
    /// One sentence for the approval queue and the audit log.
    pub reason: String,
}

impl Decision {
    fn builtin(tier: Tier, id: &'static str, reason: impl Into<String>) -> Self {
        Self {
            tier,
            rule: Rule::Builtin { id },
            reason: reason.into(),
        }
    }

    fn high(id: &'static str, reason: impl Into<String>) -> Self {
        Self::builtin(Tier::HighImpact, id, reason)
    }
}

/// An administrator allow rule: commands matching `pattern` as a whole
/// that no built-in rule knows get `tier`, read-only or low impact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    pub id: Uuid,
    pub pattern: Pattern,
    pub tier: Tier,
}

/// An administrator deny pattern: commands containing a match anywhere are
/// forbidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyRule {
    pub id: Uuid,
    pub pattern: Pattern,
}

/// A cache directory whose contents may be cleared as a low-impact change,
/// with `find <path> -mindepth 1 -delete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDir {
    pub id: Uuid,
    pub path: String,
}

/// Administrator rules in force for one server: its customer's and its
/// site's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    pub allow: Vec<AllowRule>,
    pub deny: Vec<DenyRule>,
    pub cache_dirs: Vec<CacheDir>,
}

/// Classifies `command`, a shell command line, with the built-in rules
/// and `rules`. See the module documentation for the steps.
pub fn classify(command: &str, rules: &RuleSet) -> Decision {
    let command = command.trim_matches(|c: char| c.is_ascii_whitespace());
    if command.is_empty() {
        return Decision::high("syntax.empty", "the command is empty");
    }
    if command.len() > MAX_COMMAND_LEN {
        return Decision::high(
            "syntax.too_long",
            format!("the command is longer than {MAX_COMMAND_LEN} bytes"),
        );
    }
    if let Some(decision) = deny(command, rules) {
        return decision;
    }
    if !command.is_ascii() {
        return Decision::high(
            "syntax.non_ascii",
            "the command has non-ASCII characters, which can hide lookalikes",
        );
    }
    if command
        .chars()
        .any(|c| c.is_ascii_control() && c != '\n' && c != '\t')
    {
        return Decision::high("syntax.control", "the command has control characters");
    }
    let stages = match lexer::lex(command) {
        Ok(stages) => stages,
        Err(construct) => {
            return Decision::high(construct.rule, format!("the command {}", construct.reason))
        }
    };
    if stages.iter().any(Vec::is_empty) {
        return Decision::high("syntax.pipe", "the command has an empty pipeline stage");
    }
    if stages.iter().any(|stage| is_assignment(&stage[0].text)) {
        return Decision::high(
            "syntax.env_prefix",
            "the command sets environment variables before running",
        );
    }
    let (first, filters) = stages.split_first().expect("lex returns a stage");
    let program = resolve(&first[0].text);
    let args = &first[1..];
    let verdict = builtin::command(&program, args, rules);
    for stage in filters {
        let filter = resolve(&stage[0].text);
        if !builtin::filter(&filter, &stage[1..]) {
            return Decision::high(
                "syntax.pipe_target",
                format!("`{filter}` with these arguments is not a read-only filter for a pipe"),
            );
        }
    }
    let glob_ok =
        matches!(&verdict, builtin::Verdict::Decided(d) if d.rule == builtin::GLOB_OPERANDS);
    if !glob_ok && stages.iter().flatten().any(|w| w.glob) {
        return Decision::high(
            "syntax.glob",
            "the command has unquoted glob characters outside read-only file operands",
        );
    }
    let unknown = match verdict {
        builtin::Verdict::Decided(decision) => return decision,
        builtin::Verdict::Unknown(reason) => reason,
    };
    let words: Vec<&str> = std::iter::once(program.as_str())
        .chain(args.iter().map(|w| w.text.as_str()))
        .collect();
    let allowed = rules
        .allow
        .iter()
        .filter(|rule| rule.tier < Tier::HighImpact && rule.pattern.matches_all(&words))
        .max_by_key(|rule| rule.tier);
    match allowed {
        Some(rule) => Decision {
            tier: rule.tier,
            rule: Rule::Configured { id: rule.id },
            reason: format!("allowed by the administrator rule `{}`", rule.pattern),
        },
        None => Decision::high("default.unknown", unknown),
    }
}

/// Whether `word` reads as `NAME=value`, a shell variable assignment.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Directories whose programs are known by their base name.
const STANDARD_DIRS: &[&str] = &[
    "/bin/",
    "/sbin/",
    "/usr/bin/",
    "/usr/sbin/",
    "/usr/local/bin/",
    "/usr/local/sbin/",
];

/// The name the built-in rules know `program` by: the base name of a path
/// in a standard directory, otherwise the word unchanged, so `/tmp/df` is
/// not `df`.
fn resolve(program: &str) -> String {
    STANDARD_DIRS
        .iter()
        .find_map(|dir| program.strip_prefix(dir))
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .unwrap_or(program)
        .to_owned()
}

/// Nesting depth of quoted strings searched for deny patterns.
const DENY_DEPTH: usize = 4;

/// The first deny pattern found in `command`: administrator patterns
/// first, then built-in ones.
fn deny(command: &str, rules: &RuleSet) -> Option<Decision> {
    let mut pieces = Vec::new();
    collect_pieces(command, DENY_DEPTH, &mut pieces);
    // The raw text split on whitespace too, so patterns spanning
    // operators (`curl ** | sh`) match.
    pieces.push(command.split_whitespace().map(str::to_owned).collect());
    for rule in &rules.deny {
        if pieces.iter().any(|p| rule.pattern.matches_within(p)) {
            return Some(Decision {
                tier: Tier::Forbidden,
                rule: Rule::Configured { id: rule.id },
                reason: format!("matches the deny pattern `{}`", rule.pattern),
            });
        }
    }
    pieces
        .iter()
        .find_map(|p| builtin::forbidden(p))
        .or_else(|| builtin::forbidden_text(command))
        .map(|(id, reason)| Decision::builtin(Tier::Forbidden, id, reason))
}

/// Every lenient fragment of `text`, also with program paths in standard
/// directories reduced to their base name, and recursively the fragments
/// of words that hold a command of their own (`bash -c '...'`, `"$(...)"`).
fn collect_pieces(text: &str, depth: usize, out: &mut Vec<Vec<String>>) {
    for fragment in lexer::fragments(text) {
        for word in &fragment {
            if depth > 0 && word.contains(|c: char| c.is_whitespace() || "|;&<>()`$".contains(c)) {
                collect_pieces(word, depth - 1, out);
            }
        }
        let resolved: Vec<String> = fragment.iter().map(|w| resolve(w)).collect();
        if resolved != fragment {
            out.push(resolved);
        }
        out.push(fragment);
    }
}
