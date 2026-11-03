//! A conservative Windows PowerShell lexer (ADR-0084).
//!
//! [`lex`] splits a command the way PowerShell splits a pipeline of
//! simple commands in argument mode, and refuses everything else:
//! statement separators, variables, subexpressions, script blocks, arrays
//! and hashtables, type literals, escapes, redirections and comments.
//! [`fragments`] is the lenient counterpart used for deny patterns.

use crate::remediation::risk::lexer::Construct;

/// One word after quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// It was a quoted string, so never a parameter name.
    pub quoted: bool,
}

impl Word {
    /// The parameter this word names, lower case, with its inline value
    /// (`-Name:value`), if it is an unquoted `-name`.
    pub fn parameter(&self) -> Option<(String, Option<&str>)> {
        let rest = self.text.strip_prefix('-').filter(|_| !self.quoted)?;
        if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        Some(match rest.split_once(':') {
            Some((name, value)) => (name.to_ascii_lowercase(), Some(value)),
            None => (rest.to_ascii_lowercase(), None),
        })
    }
}

const fn construct(rule: &'static str, reason: &'static str) -> Construct {
    Construct { rule, reason }
}

pub const COMMAND_LIST: Construct = construct(
    "syntax.command_list",
    "runs several statements (`;`, `&`, `&&`, `||` or a newline)",
);
pub const SUBSTITUTION: Construct = construct(
    "syntax.substitution",
    "uses variables or subexpressions (`$`), which PowerShell expands",
);
pub const ESCAPE: Construct = construct(
    "syntax.escape",
    "uses backtick escapes, which can hide what a word says",
);
pub const GROUPING: Construct = construct(
    "syntax.grouping",
    "uses parentheses, script blocks, arrays, hashtables or splatting (`(`, `{`, `@`)",
);
pub const TYPE: Construct = construct(
    "syntax.type",
    "uses type literals or brackets (`[`, `]`), which can call .NET directly",
);
pub const REDIRECTION: Construct = construct(
    "syntax.redirection",
    "redirects output (`>`, `<`), which writes files",
);
pub const COMMENT: Construct = construct("syntax.comment", "contains a comment (`#`)");
pub const UNTERMINATED: Construct =
    construct("syntax.unterminated", "has an unterminated quoted string");
pub const MIXED_QUOTES: Construct = construct(
    "syntax.quoting",
    "joins quoted and unquoted text in one word, which PowerShell may split",
);
pub const STOP_PARSING: Construct =
    construct("syntax.stop_parsing", "uses the stop-parsing token (`--%`)");

/// Words of each pipeline stage, in order, or the first refused construct.
/// The input must already be trimmed and checked for non-ASCII and
/// control characters, so PowerShell's typographic dashes and quotes
/// cannot appear.
pub fn lex(input: &str) -> Result<Vec<Vec<Word>>, Construct> {
    let mut stages = vec![Vec::new()];
    let mut chars = input.chars().peekable();
    let mut word: Option<Word> = None;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => finish(&mut word, &mut stages)?,
            '\n' | '\r' | ';' | '&' => return Err(COMMAND_LIST),
            '|' => {
                if chars.peek() == Some(&'|') {
                    return Err(COMMAND_LIST);
                }
                finish(&mut word, &mut stages)?;
                stages.push(Vec::new());
            }
            '$' => return Err(SUBSTITUTION),
            '`' => return Err(ESCAPE),
            '(' | ')' | '{' | '}' | '@' => return Err(GROUPING),
            '[' | ']' => return Err(TYPE),
            '<' | '>' => return Err(REDIRECTION),
            '#' => return Err(COMMENT),
            '\'' | '"' => {
                if word.is_some() {
                    return Err(MIXED_QUOTES);
                }
                let mut text = String::new();
                loop {
                    match chars.next() {
                        None => return Err(UNTERMINATED),
                        // A doubled quote is one quote character.
                        Some(q) if q == c && chars.peek() == Some(&c) => {
                            chars.next();
                            text.push(q);
                        }
                        Some(q) if q == c => break,
                        Some('$') if c == '"' => return Err(SUBSTITUTION),
                        Some('`') if c == '"' => return Err(ESCAPE),
                        Some(inner) => text.push(inner),
                    }
                }
                if chars.peek().is_some_and(|n| !matches!(n, ' ' | '\t' | '|')) {
                    return Err(MIXED_QUOTES);
                }
                word = Some(Word { text, quoted: true });
            }
            other => {
                let w = word.get_or_insert_with(|| Word {
                    text: String::new(),
                    quoted: false,
                });
                w.text.push(other);
            }
        }
    }
    finish(&mut word, &mut stages)?;
    Ok(stages)
}

fn finish(word: &mut Option<Word>, stages: &mut [Vec<Word>]) -> Result<(), Construct> {
    if let Some(w) = word.take() {
        if !w.quoted && w.text == "--%" {
            return Err(STOP_PARSING);
        }
        if let Some(stage) = stages.last_mut() {
            stage.push(w);
        }
    }
    Ok(())
}

/// Environment variables that name system directories, as PowerShell and
/// `cmd.exe` write them, with the directory they stand for.
const SYSTEM_VARIABLES: &[(&str, &str)] = &[
    ("systemroot", r"c:\windows"),
    ("windir", r"c:\windows"),
    ("systemdrive", "c:"),
    ("programfiles(x86)", r"c:\program files (x86)"),
    ("programfiles", r"c:\program files"),
    ("programw6432", r"c:\program files"),
    ("programdata", r"c:\programdata"),
    ("allusersprofile", r"c:\programdata"),
];

/// Lenient fragments of `input` for deny patterns: lower case, backticks
/// and carets (PowerShell's and `cmd.exe`'s escapes) removed, typographic
/// dashes and quotes made ASCII, system directory variables replaced by
/// their usual value, and cut at every operator. Each fragment comes
/// twice when quotes are involved: with quoted strings kept as one word,
/// and with them split on spaces, so a command hidden in a string
/// (`cmd /c "rd /s C:\Windows"`) is found too.
pub fn fragments(input: &str) -> Vec<Vec<String>> {
    let mut text: String = input
        .chars()
        .filter(|c| !matches!(c, '`' | '^'))
        .map(|c| match c {
            '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' => '"',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    for (name, value) in SYSTEM_VARIABLES {
        for form in [
            format!("${{env:{name}}}"),
            format!("$env:{name}"),
            format!("%{name}%"),
        ] {
            text = text.replace(&form, &protect(value));
        }
    }
    let mut out = cut(&text, true);
    out.extend(cut(&text, false));
    out.dedup();
    out
}

/// Placeholders for the space and parentheses of a directory a variable
/// stands for, so it stays one word in [`fragments`].
const PROTECTED: [(char, char); 3] = [(' ', '\u{1}'), ('(', '\u{2}'), (')', '\u{3}')];

fn protect(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            PROTECTED
                .iter()
                .find(|(p, _)| *p == c)
                .map_or(c, |(_, r)| *r)
        })
        .collect()
}

/// `word` of a fragment with the directory names [`fragments`] protected
/// written out again.
pub fn unprotect(word: &str) -> String {
    word.chars()
        .map(|c| {
            PROTECTED
                .iter()
                .find(|(_, r)| *r == c)
                .map_or(c, |(p, _)| *p)
        })
        .collect()
}

/// `text` cut at operators into fragments of words; with `keep_quoted`,
/// a quoted string is one word without its quotes, else quotes separate
/// words like spaces.
fn cut(text: &str, keep_quoted: bool) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut word = String::new();
    let mut chars = text.chars();
    let end_word = |word: &mut String, out: &mut Vec<Vec<String>>| {
        if !word.is_empty() {
            if let Some(last) = out.last_mut() {
                last.push(std::mem::take(word));
            }
        }
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() && c != '\n' => end_word(&mut word, &mut out),
            '\'' | '"' if keep_quoted => {
                for inner in chars.by_ref() {
                    if inner == c {
                        break;
                    }
                    word.push(inner);
                }
            }
            '\'' | '"' => end_word(&mut word, &mut out),
            '\n' | '\r' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '{' | '}' | '@' => {
                end_word(&mut word, &mut out);
                if out.last().is_some_and(|f| !f.is_empty()) {
                    out.push(Vec::new());
                }
            }
            other => word.push(other),
        }
    }
    end_word(&mut word, &mut out);
    out.retain(|f| !f.is_empty());
    out
}
