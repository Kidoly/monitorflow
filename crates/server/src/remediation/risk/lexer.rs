//! A conservative shell lexer (ADR-0041).
//!
//! [`lex`] splits a command the way a POSIX shell would split a simple
//! command or a pipeline of them, resolving quotes and backslashes, and
//! refuses everything else: the first construct it cannot reason about is
//! returned as a [`Construct`], which the classifier turns into at least
//! high impact. [`fragments`] is the lenient counterpart used for deny
//! patterns: it never fails and cuts the input at every operator, so a
//! denied command is found wherever it hides.

/// One word after quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// Some part of it was quoted or escaped.
    pub quoted: bool,
    /// It has an unquoted `*`, `?` or `[`, so the shell would expand it.
    pub glob: bool,
}

impl Word {
    #[cfg(test)]
    pub fn plain(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            quoted: false,
            glob: false,
        }
    }
}

/// A construct the strict lexer refuses, with the built-in rule naming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Construct {
    pub rule: &'static str,
    pub reason: &'static str,
}

const fn construct(rule: &'static str, reason: &'static str) -> Construct {
    Construct { rule, reason }
}

pub const COMMAND_LIST: Construct = construct(
    "syntax.command_list",
    "runs several commands (`;`, `&`, `&&`, `||` or a newline)",
);
pub const SUBSTITUTION: Construct = construct(
    "syntax.substitution",
    "uses command substitution or parameter expansion (`$`, backticks)",
);
pub const REDIRECTION: Construct = construct(
    "syntax.redirection",
    "redirects input or output (`<`, `>`, here-documents)",
);
pub const GROUPING: Construct = construct(
    "syntax.grouping",
    "uses a subshell, a group or brace expansion (`(`, `)`, `{`, `}`)",
);
pub const COMMENT: Construct = construct("syntax.comment", "contains a shell comment (`#`)");
pub const HISTORY: Construct =
    construct("syntax.history", "uses `!` (history expansion or negation)");
pub const TILDE: Construct = construct("syntax.tilde", "uses tilde expansion (`~`)");
pub const UNTERMINATED: Construct = construct(
    "syntax.unterminated",
    "has an unterminated quote or a trailing backslash",
);
pub const CONTINUATION: Construct = construct(
    "syntax.continuation",
    "uses a backslash-newline line continuation",
);
pub const STDERR_PIPE: Construct = construct("syntax.stderr_pipe", "pipes standard error (`|&`)");

/// Words of each pipeline stage, in order, or the first refused construct.
/// The input must already be trimmed and checked for non-ASCII and
/// control characters other than newline and tab.
pub fn lex(input: &str) -> Result<Vec<Vec<Word>>, Construct> {
    let mut stages = vec![Vec::new()];
    let mut chars = input.chars().peekable();
    let mut word: Option<Word> = None;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => finish(&mut word, &mut stages),
            '\n' | ';' | '&' => return Err(COMMAND_LIST),
            '|' => match chars.peek() {
                Some('|') => return Err(COMMAND_LIST),
                Some('&') => return Err(STDERR_PIPE),
                _ => {
                    finish(&mut word, &mut stages);
                    stages.push(Vec::new());
                }
            },
            '<' | '>' => return Err(REDIRECTION),
            '(' | ')' | '{' | '}' => return Err(GROUPING),
            '$' | '`' => return Err(SUBSTITUTION),
            '!' => return Err(HISTORY),
            '#' if word.is_none() => return Err(COMMENT),
            '~' if word.is_none() => return Err(TILDE),
            '\\' => match chars.next() {
                None => return Err(UNTERMINATED),
                Some('\n') => return Err(CONTINUATION),
                Some(escaped) => push(&mut word, escaped, true),
            },
            '\'' => {
                let w = word.get_or_insert_with(empty);
                w.quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(UNTERMINATED),
                        Some('\'') => break,
                        Some(inner) => w.text.push(inner),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(empty);
                w.quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(UNTERMINATED),
                        Some('"') => break,
                        Some('$' | '`') => return Err(SUBSTITUTION),
                        Some('\\') => match chars.next() {
                            None => return Err(UNTERMINATED),
                            Some('\n') => return Err(CONTINUATION),
                            Some(e @ ('"' | '\\')) => w.text.push(e),
                            Some(other) => {
                                w.text.push('\\');
                                w.text.push(other);
                            }
                        },
                        Some(inner) => w.text.push(inner),
                    }
                }
            }
            other => {
                let w = word.get_or_insert_with(empty);
                if matches!(other, '*' | '?' | '[') {
                    w.glob = true;
                }
                w.text.push(other);
            }
        }
    }
    finish(&mut word, &mut stages);
    Ok(stages)
}

fn empty() -> Word {
    Word {
        text: String::new(),
        quoted: false,
        glob: false,
    }
}

fn push(word: &mut Option<Word>, c: char, quoted: bool) {
    let w = word.get_or_insert_with(empty);
    w.quoted |= quoted;
    w.text.push(c);
}

fn finish(word: &mut Option<Word>, stages: &mut [Vec<Word>]) {
    if let Some(w) = word.take() {
        if let Some(stage) = stages.last_mut() {
            stage.push(w);
        }
    }
}

/// Lenient split for deny patterns: the words of every piece of `input`
/// between shell operators (`;`, `&`, `|`, newlines, redirections,
/// parentheses, braces, `$(`, backticks), with quotes and backslashes
/// resolved as far as they go. Never fails; an unterminated quote runs to
/// the end of the input.
pub fn fragments(input: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut chars = input.chars().peekable();
    let cut = |word: &mut Option<String>, out: &mut Vec<Vec<String>>| {
        if let Some(w) = word.take() {
            if let Some(last) = out.last_mut() {
                last.push(w);
            }
        }
        if out.last().is_some_and(|f| !f.is_empty()) {
            out.push(Vec::new());
        }
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() && c != '\n' => {
                if let Some(w) = word.take() {
                    if let Some(last) = out.last_mut() {
                        last.push(w);
                    }
                }
            }
            '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '{' | '}' | '`' => {
                cut(&mut word, &mut out)
            }
            '$' if chars.peek() == Some(&'(') => cut(&mut word, &mut out),
            '\\' => {
                if let Some(escaped) = chars.next() {
                    word.get_or_insert_with(String::new).push(escaped);
                }
            }
            '\'' | '"' => {
                let w = word.get_or_insert_with(String::new);
                for inner in chars.by_ref() {
                    if inner == c {
                        break;
                    }
                    w.push(inner);
                }
            }
            other => word.get_or_insert_with(String::new).push(other),
        }
    }
    cut(&mut word, &mut out);
    out.retain(|f| !f.is_empty());
    out
}
