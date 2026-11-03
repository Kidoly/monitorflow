//! Risk rules for Windows servers (ADR-0084): command lines that Windows
//! PowerShell 5.1 runs, classified into the same tiers as the POSIX rules
//! of ADR-0041, by rules written for PowerShell's own syntax.
//!
//! The steps after the shared length checks, in order (the first that
//! decides wins):
//!
//! 1. Deny patterns, administrator ones then built-in ones, matched
//!    leniently and without regard to case anywhere in the command,
//!    escapes removed: forbidden.
//! 2. Non-ASCII characters (PowerShell reads typographic dashes and
//!    quotes as `-` and `'`) or control characters: high.
//! 3. The strict lexer ([`lexer::lex`]) refuses statement lists,
//!    variables, subexpressions, script blocks, arrays, hashtables,
//!    splatting, type literals, escapes, redirections, comments and mixed
//!    quoting: high.
//! 4. Every stage must start with a plain command name. The first goes
//!    through the built-in rules ([`rules`]); every later stage must be a
//!    built-in read-only filter (`Select-Object`, `Where-Object` in its
//!    simplified form, `Sort-Object`, `Format-*`, `Measure-Object`,
//!    `Select-String`, ...), else high.
//! 5. A command no built-in rule knows takes the tier of the most
//!    cautious matching administrator allow rule, else high.

mod deny;
pub mod lexer;
mod rules;
#[cfg(test)]
mod tests;

use super::{charset, Decision, Rule, RuleSet, Tier};

use rules::Verdict;

/// Classifies `command`, already trimmed and of an accepted length.
pub fn classify(command: &str, rules: &RuleSet) -> Decision {
    if let Some(decision) = deny(command, rules) {
        return decision;
    }
    if let Some(decision) = charset(command) {
        return decision;
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
    if let Some(stage) = stages.iter().find(|s| !rules::command_name(&s[0])) {
        return Decision::high(
            "syntax.command_name",
            format!(
                "`{}` is not a plain command name, so PowerShell would evaluate it",
                stage[0].text
            ),
        );
    }
    let (first, filters) = stages.split_first().expect("lex returns a stage");
    let program = rules::program(&first[0].text);
    let args = &first[1..];
    let verdict = rules::command(&program, &first[0].text, args);
    for stage in filters {
        if !rules::filter(&rules::program(&stage[0].text), &stage[1..]) {
            return Decision::high(
                "syntax.pipe_target",
                format!(
                    "`{}` with these arguments is not a read-only filter for a pipe",
                    stage[0].text
                ),
            );
        }
    }
    let unknown = match verdict {
        Verdict::Decided(decision) => return decision,
        Verdict::Unknown(reason) => reason,
    };
    let words: Vec<&str> = first.iter().map(|w| w.text.as_str()).collect();
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

/// The first deny pattern found in `command`, without regard to case:
/// administrator patterns first, then built-in ones.
fn deny(command: &str, rules: &RuleSet) -> Option<Decision> {
    let mut pieces = lexer::fragments(command);
    pieces.push(
        command
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect(),
    );
    for rule in &rules.deny {
        let lower = super::Pattern::parse(&rule.pattern.text().to_ascii_lowercase());
        let pattern = lower.as_ref().unwrap_or(&rule.pattern);
        if pieces.iter().any(|p| pattern.matches_within(p)) {
            return Some(Decision {
                tier: Tier::Forbidden,
                rule: Rule::Configured { id: rule.id },
                reason: format!("matches the deny pattern `{}`", rule.pattern),
            });
        }
    }
    pieces
        .iter()
        .find_map(|p| deny::forbidden(p))
        .or_else(|| deny::forbidden_text(command))
        .map(|(id, reason)| Decision::builtin(Tier::Forbidden, id, reason))
}
