//! A bounded reader of MIB files (ADR-0157): the subset of SMIv1 and
//! SMIv2 that names objects, their OIDs, types, enumerations and units.
//!
//! It reads text only. It never fetches an imported module and runs
//! nothing; every loop is over tokens with explicit limits, and nesting
//! is counted, never recursed on input. Imports resolve against a small
//! built-in base ([`base`]), the modules given as [`Known`], and the other
//! files of the same upload.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Largest file read.
pub const MAX_FILE: usize = 1 << 20;
/// Most files in one upload.
pub const MAX_FILES: usize = 20;
/// Most objects one file may define.
pub const MAX_OBJECTS: usize = 20_000;
/// Deepest nesting of braces and parentheses.
pub const MAX_DEPTH: usize = 32;
/// Longest description kept.
pub const MAX_DESCRIPTION: usize = 500;
/// Most tokens of one file, well above what 1 MiB of MIB holds.
const MAX_TOKENS: usize = 1 << 20;
/// Most enumeration items of one type.
const MAX_ENUM: usize = 1_000;
/// Longest name of a module, an object or an enumeration item.
const MAX_IDENT: usize = 128;

/// Why files could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MibError {
    /// A file breaks a limit or is not a MIB: its name and why.
    Invalid { file: String, reason: String },
    /// Objects hang off names no module given or known defines: by
    /// module to upload, the names wanted from it.
    Missing(BTreeMap<String, BTreeSet<String>>),
}

impl std::fmt::Display for MibError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid { file, reason } => write!(f, "{file}: {reason}"),
            Self::Missing(modules) => {
                let list: Vec<String> = modules.keys().cloned().collect();
                write!(f, "upload these modules too: {}", list.join(", "))
            }
        }
    }
}

impl std::error::Error for MibError {}

/// What an object is in its MIB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    /// A branch of the tree: OBJECT IDENTIFIER, MODULE-IDENTITY...
    Node,
    /// An OBJECT-TYPE with one value, read at `.0`.
    Scalar,
    /// A table (SEQUENCE OF).
    Table,
    /// The entry of a table.
    Entry,
    /// A column of a table, read at `.<index>`.
    Column,
    /// A NOTIFICATION-TYPE or a trap.
    Notification,
}

/// One named object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub name: String,
    pub oid: Vec<u32>,
    pub kind: ObjectKind,
    /// The type as written, resolved through textual conventions to the
    /// name a person knows: `Counter64`, `DisplayString`, `INTEGER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax: Option<String>,
    /// Named values of an enumerated integer, from the object or its type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enumeration: Vec<(i64, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A named type: the base it stands for and its enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TypeInfo {
    pub base: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enumeration: Vec<(i64, String)>,
    /// A SEQUENCE: the type of a table's entry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sequence: bool,
}

/// One module read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub name: String,
    /// The file it came from.
    pub file: String,
    pub objects: Vec<Object>,
    pub types: BTreeMap<String, TypeInfo>,
}

/// Modules already known, by name: their objects' OIDs and their types.
#[derive(Debug, Clone, Default)]
pub struct Known {
    pub modules: BTreeMap<String, KnownModule>,
}

#[derive(Debug, Clone, Default)]
pub struct KnownModule {
    pub nodes: BTreeMap<String, Vec<u32>>,
    pub types: BTreeMap<String, TypeInfo>,
}

impl Known {
    pub fn add(&mut self, module: &Module) {
        let entry = self.modules.entry(module.name.clone()).or_default();
        for object in &module.objects {
            entry.nodes.insert(object.name.clone(), object.oid.clone());
        }
        entry
            .types
            .extend(module.types.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Number(i64),
    Text(String),
    Assign,
    Open,
    Close,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    Comma,
    Semicolon,
    Range,
    Bar,
    Other,
}

fn invalid(file: &str, reason: impl Into<String>) -> MibError {
    MibError::Invalid {
        file: file.to_owned(),
        reason: reason.into(),
    }
}

/// Splits `text` into tokens, comments dropped.
fn tokens(file: &str, text: &str) -> Result<Vec<Token>, MibError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if out.len() > MAX_TOKENS {
            return Err(invalid(file, "too long"));
        }
        let b = bytes[i];
        match b {
            b if b.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                // A comment ends at the line's end or at the next `--`.
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            b'"' => {
                let start = i + 1;
                i = start;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(invalid(file, "a quoted text never ends"));
                }
                out.push(Token::Text(text[start..i].to_owned()));
                i += 1;
            }
            b'\'' => {
                // A binary or hexadecimal string, `'0A'H`.
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                i += 2;
                out.push(Token::Other);
            }
            b':' if text[i..].starts_with("::=") => {
                out.push(Token::Assign);
                i += 3;
            }
            b'.' if bytes.get(i + 1) == Some(&b'.') => {
                out.push(Token::Range);
                i += 2;
            }
            b'{' => (out.push(Token::Open), i += 1).1,
            b'}' => (out.push(Token::Close), i += 1).1,
            b'(' => (out.push(Token::OpenParen), i += 1).1,
            b')' => (out.push(Token::CloseParen), i += 1).1,
            b'[' => (out.push(Token::OpenBracket), i += 1).1,
            b']' => (out.push(Token::CloseBracket), i += 1).1,
            b',' => (out.push(Token::Comma), i += 1).1,
            b';' => (out.push(Token::Semicolon), i += 1).1,
            b'|' => (out.push(Token::Bar), i += 1).1,
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                match text[start..i].parse::<i64>() {
                    Ok(n) => out.push(Token::Number(n)),
                    Err(_) => out.push(Token::Other),
                }
            }
            b if b.is_ascii_alphabetic() => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric()
                        || bytes[i] == b'_'
                        || (bytes[i] == b'-' && bytes.get(i + 1) != Some(&b'-')))
                {
                    i += 1;
                }
                out.push(Token::Ident(text[start..i].to_owned()));
            }
            _ => {
                // Anything else, multi-byte characters included.
                i += text[i..].chars().next().map_or(1, char::len_utf8);
                out.push(Token::Other);
            }
        }
    }
    Ok(out)
}

/// An object as written, before its OID is resolved.
#[derive(Debug, Clone)]
struct Raw {
    name: String,
    /// `{ parent 1 2 }`: the parent's name, or none for `{ 1 3 6 }`.
    parent: Option<String>,
    arcs: Vec<u32>,
    /// Names given inside the value, like `org(3)`, with their arcs.
    named: Vec<(String, usize)>,
    macro_kind: String,
    syntax: Option<Syntax>,
    units: Option<String>,
    access: Option<String>,
    description: String,
}

#[derive(Debug, Clone, Default)]
struct Syntax {
    name: String,
    enumeration: Vec<(i64, String)>,
    sequence_of: bool,
    sequence: bool,
}

/// An OID value as written: the parent's name, the arcs after it, and the
/// names given to arcs on the way.
type OidValue = (Option<String>, Vec<u32>, Vec<(String, usize)>);

struct Parser<'a> {
    file: &'a str,
    tokens: Vec<Token>,
    at: usize,
}

const MACROS: [&str; 10] = [
    "OBJECT-TYPE",
    "MODULE-IDENTITY",
    "OBJECT-IDENTITY",
    "NOTIFICATION-TYPE",
    "OBJECT-GROUP",
    "NOTIFICATION-GROUP",
    "MODULE-COMPLIANCE",
    "AGENT-CAPABILITIES",
    "TRAP-TYPE",
    "TEXTUAL-CONVENTION",
];

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn peek_at(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.at + n)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn is_ident(&self, n: usize, word: &str) -> bool {
        matches!(self.peek_at(n), Some(Token::Ident(w)) if w == word)
    }

    fn err(&self, reason: impl Into<String>) -> MibError {
        invalid(self.file, reason)
    }

    /// Skips a balanced group opening at the current token.
    fn skip_group(&mut self) -> Result<(), MibError> {
        let mut depth = 0usize;
        while let Some(t) = self.next() {
            match t {
                Token::Open | Token::OpenParen | Token::OpenBracket => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(self.err("nested too deeply"));
                    }
                }
                Token::Close | Token::CloseParen | Token::CloseBracket => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err(self.err("a group never closes"))
    }

    fn ident(&mut self) -> Option<String> {
        match self.peek() {
            Some(Token::Ident(w)) => {
                let w = w.clone();
                self.at += 1;
                Some(w)
            }
            _ => None,
        }
    }

    /// `{ name(1), other(2) }`, the opening brace current.
    fn enumeration(&mut self) -> Result<Vec<(i64, String)>, MibError> {
        let mut items = Vec::new();
        self.at += 1;
        loop {
            match self.next() {
                Some(Token::Close) => return Ok(items),
                Some(Token::Ident(name)) => {
                    if self.peek() == Some(&Token::OpenParen) {
                        self.at += 1;
                        if let Some(Token::Number(n)) = self.next() {
                            if name.len() <= MAX_IDENT && items.len() < MAX_ENUM {
                                items.push((n, name));
                            }
                        }
                        // The closing parenthesis.
                        if self.next() != Some(Token::CloseParen) {
                            return Err(self.err("an enumeration item does not read"));
                        }
                    }
                }
                Some(Token::Comma) => {}
                Some(Token::Open | Token::OpenParen) => {
                    return Err(self.err("an enumeration does not read"))
                }
                Some(_) => {}
                None => return Err(self.err("an enumeration never closes")),
            }
        }
    }

    /// A type, as in `SYNTAX` or a type assignment.
    fn syntax(&mut self) -> Result<Syntax, MibError> {
        let mut syntax = Syntax::default();
        if self.peek() == Some(&Token::OpenBracket) {
            self.skip_group()?;
        }
        while self.is_ident(0, "IMPLICIT") || self.is_ident(0, "EXPLICIT") {
            self.at += 1;
        }
        let Some(first) = self.ident() else {
            return Err(self.err("a type is missing"));
        };
        syntax.name = match first.as_str() {
            "OCTET" if self.is_ident(0, "STRING") => {
                self.at += 1;
                "OCTET STRING".into()
            }
            "OBJECT" if self.is_ident(0, "IDENTIFIER") => {
                self.at += 1;
                "OBJECT IDENTIFIER".into()
            }
            "SEQUENCE" if self.is_ident(0, "OF") => {
                self.at += 1;
                syntax.sequence_of = true;
                self.ident()
                    .ok_or_else(|| self.err("SEQUENCE OF names no type"))?
            }
            "SEQUENCE" | "CHOICE" => {
                syntax.sequence = first == "SEQUENCE";
                if self.peek() == Some(&Token::Open) {
                    self.skip_group()?;
                }
                return Ok(syntax);
            }
            _ => first,
        };
        match self.peek() {
            Some(Token::Open)
                if matches!(syntax.name.as_str(), "INTEGER" | "BITS" | "Integer32")
                    || syntax.name.starts_with(|c: char| c.is_ascii_uppercase()) =>
            {
                syntax.enumeration = self.enumeration()?;
            }
            _ => {}
        }
        if self.peek() == Some(&Token::OpenParen) {
            self.skip_group()?;
        }
        Ok(syntax)
    }

    /// `{ parent 1 name(2) 3 }`, the opening brace current.
    fn oid_value(&mut self) -> Result<OidValue, MibError> {
        self.at += 1;
        let mut parent = None;
        let mut arcs = Vec::new();
        let mut named = Vec::new();
        let mut first = true;
        loop {
            match self.next() {
                Some(Token::Close) => return Ok((parent, arcs, named)),
                Some(Token::Number(n)) => {
                    arcs.push(u32::try_from(n).map_err(|_| self.err("an OID arc is negative"))?)
                }
                Some(Token::Ident(name)) => {
                    if self.peek() == Some(&Token::OpenParen) {
                        self.at += 1;
                        let Some(Token::Number(n)) = self.next() else {
                            return Err(self.err("an OID arc does not read"));
                        };
                        self.next();
                        let arc =
                            u32::try_from(n).map_err(|_| self.err("an OID arc is negative"))?;
                        named.push((name, arcs.len()));
                        arcs.push(arc);
                    } else if first {
                        parent = Some(name);
                    } else {
                        return Err(self.err("an OID value does not read"));
                    }
                }
                _ => return Err(self.err("an OID value does not read")),
            }
            first = false;
            if arcs.len() > 128 {
                return Err(self.err("an OID is too long"));
            }
        }
    }
}

/// Collapses spaces and keeps at most [`MAX_DESCRIPTION`] characters.
fn description(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: String = words.join(" ").chars().take(MAX_DESCRIPTION).collect();
    if words.join(" ").chars().count() > MAX_DESCRIPTION {
        out.push('…');
    }
    out
}

/// What one file defines: modules with their imports and raw objects.
struct Parsed {
    name: String,
    file: String,
    imports: BTreeMap<String, String>,
    raws: Vec<Raw>,
    types: BTreeMap<String, Syntax>,
}

fn parse_file(file: &str, text: &str) -> Result<Vec<Parsed>, MibError> {
    if text.len() > MAX_FILE {
        return Err(invalid(
            file,
            format!("larger than {} KiB", MAX_FILE / 1024),
        ));
    }
    let mut p = Parser {
        file,
        tokens: tokens(file, text)?,
        at: 0,
    };
    let mut modules = Vec::new();
    while p.at < p.tokens.len() {
        // `NAME DEFINITIONS [tags] ::= BEGIN`
        let Some(name) = p.ident() else {
            p.at += 1;
            continue;
        };
        if !p.is_ident(0, "DEFINITIONS") {
            continue;
        }
        while !matches!(p.peek(), Some(Token::Assign) | None) {
            p.at += 1;
        }
        p.at += 1;
        if !p.is_ident(0, "BEGIN") {
            return Err(p.err("a module does not start with BEGIN"));
        }
        p.at += 1;
        if name.len() > MAX_IDENT {
            return Err(p.err("a module name is too long"));
        }
        let mut module = Parsed {
            name,
            file: file.to_owned(),
            imports: BTreeMap::new(),
            raws: Vec::new(),
            types: BTreeMap::new(),
        };
        statements(&mut p, &mut module)?;
        modules.push(module);
    }
    if modules.is_empty() {
        return Err(invalid(file, "no MIB module (NAME DEFINITIONS ::= BEGIN)"));
    }
    Ok(modules)
}

fn statements(p: &mut Parser, module: &mut Parsed) -> Result<(), MibError> {
    loop {
        let Some(token) = p.peek().cloned() else {
            return Err(p.err("the module never ends (END)"));
        };
        let Token::Ident(word) = token else {
            p.at += 1;
            continue;
        };
        match word.as_str() {
            "END" => {
                p.at += 1;
                return Ok(());
            }
            "IMPORTS" => {
                p.at += 1;
                let mut pending = Vec::new();
                loop {
                    match p.next() {
                        Some(Token::Semicolon) | None => break,
                        Some(Token::Ident(w)) if w == "FROM" => {
                            if let Some(from) = p.ident() {
                                for symbol in pending.drain(..) {
                                    module.imports.insert(symbol, from.clone());
                                }
                            }
                        }
                        Some(Token::Ident(w)) => pending.push(w),
                        _ => {}
                    }
                }
                continue;
            }
            "EXPORTS" => {
                while !matches!(p.next(), Some(Token::Semicolon) | None) {}
                continue;
            }
            _ => {}
        }
        p.at += 1;
        if p.is_ident(0, "MACRO") {
            // A macro's definition, as in SNMPv2-SMI: skipped to its END.
            while !matches!(p.next(), Some(Token::Ident(w)) if w == "END") {
                if p.at >= p.tokens.len() {
                    return Err(p.err("a MACRO never ends"));
                }
            }
            continue;
        }
        if p.peek() == Some(&Token::Assign) {
            // A type: `Name ::= TEXTUAL-CONVENTION ... SYNTAX t`, or a type.
            p.at += 1;
            let syntax = if p.is_ident(0, "TEXTUAL-CONVENTION") {
                p.at += 1;
                loop {
                    match p.peek() {
                        Some(Token::Ident(w)) if w == "SYNTAX" => {
                            p.at += 1;
                            break p.syntax()?;
                        }
                        Some(Token::Open | Token::OpenParen) => p.skip_group()?,
                        None => return Err(p.err("a TEXTUAL-CONVENTION has no SYNTAX")),
                        _ => p.at += 1,
                    }
                }
            } else {
                p.syntax()?
            };
            module.types.insert(word, syntax);
            continue;
        }
        let macro_kind = if p.is_ident(0, "OBJECT") && p.is_ident(1, "IDENTIFIER") {
            p.at += 2;
            "OBJECT IDENTIFIER".to_owned()
        } else if let Some(Token::Ident(m)) = p
            .peek()
            .filter(|t| matches!(t, Token::Ident(m) if MACROS.contains(&m.as_str())))
        {
            let m = m.clone();
            p.at += 1;
            m
        } else {
            // Something this reader does not need: skipped to its value.
            while !matches!(p.peek(), Some(Token::Assign) | None) {
                if matches!(p.peek(), Some(Token::Ident(w)) if w == "END") {
                    break;
                }
                p.at += 1;
            }
            if p.peek() == Some(&Token::Assign) {
                p.at += 1;
                if matches!(p.peek(), Some(Token::Open)) {
                    p.skip_group()?;
                }
            }
            continue;
        };
        let mut raw = Raw {
            name: word,
            parent: None,
            arcs: Vec::new(),
            named: Vec::new(),
            macro_kind,
            syntax: None,
            units: None,
            access: None,
            description: String::new(),
        };
        // Clauses up to `::=`.
        loop {
            match p.peek().cloned() {
                Some(Token::Assign) => {
                    p.at += 1;
                    break;
                }
                None => return Err(p.err("a definition has no value (::=)")),
                Some(Token::Ident(w)) => {
                    p.at += 1;
                    match w.as_str() {
                        "SYNTAX" if raw.syntax.is_none() => raw.syntax = Some(p.syntax()?),
                        "UNITS" => {
                            if let Some(Token::Text(t)) = p.next() {
                                raw.units = Some(t.chars().take(32).collect());
                            }
                        }
                        "MAX-ACCESS" | "ACCESS" => raw.access = p.ident(),
                        "DESCRIPTION" => {
                            if let Some(Token::Text(t)) = p.next() {
                                if raw.description.is_empty() {
                                    raw.description = description(&t);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Some(Token::Open | Token::OpenParen | Token::OpenBracket) => p.skip_group()?,
                Some(_) => p.at += 1,
            }
        }
        if raw.macro_kind == "TRAP-TYPE" {
            // `::= 3`: a trap number, not an OID of the tree.
            p.at += 1;
            continue;
        }
        if p.peek() != Some(&Token::Open) {
            return Err(p.err(format!("`{}` has no OID value", raw.name)));
        }
        let (parent, arcs, named) = p.oid_value()?;
        raw.parent = parent;
        raw.arcs = arcs;
        raw.named = named;
        if module.raws.len() >= MAX_OBJECTS {
            return Err(p.err(format!("more than {MAX_OBJECTS} objects")));
        }
        if raw.name.len() <= MAX_IDENT {
            module.raws.push(raw);
        }
    }
}

/// Reads `files` (name and text) into modules, resolving each object's
/// OID against `known`, the built-in base and one another.
pub fn parse(files: &[(String, String)], known: &Known) -> Result<Vec<Module>, MibError> {
    if files.len() > MAX_FILES {
        return Err(invalid(
            "upload",
            format!("at most {MAX_FILES} files at once"),
        ));
    }
    let mut parsed = Vec::new();
    for (file, text) in files {
        parsed.extend(parse_file(file, text)?);
    }
    let base = base();
    // Names resolved so far, per module; the base and known modules first.
    let mut nodes: BTreeMap<String, BTreeMap<String, Vec<u32>>> = BTreeMap::new();
    let mut types: BTreeMap<String, BTreeMap<String, TypeInfo>> = BTreeMap::new();
    for (name, m) in base.modules.iter().chain(&known.modules) {
        nodes
            .entry(name.clone())
            .or_default()
            .extend(m.nodes.clone());
        types
            .entry(name.clone())
            .or_default()
            .extend(m.types.clone());
    }
    let global_root: BTreeMap<&str, Vec<u32>> = [
        ("iso", vec![1]),
        ("ccitt", vec![0]),
        ("joint-iso-ccitt", vec![2]),
    ]
    .into();
    // Types first: each module's own, through imports.
    for module in &parsed {
        let own = types.entry(module.name.clone()).or_default();
        for (name, syntax) in &module.types {
            own.insert(
                name.clone(),
                TypeInfo {
                    base: syntax.name.clone(),
                    enumeration: syntax.enumeration.clone(),
                    sequence: syntax.sequence,
                },
            );
        }
    }
    // Then OIDs, over as many rounds as the longest chain of modules.
    let mut done: Vec<BTreeMap<String, Vec<u32>>> = vec![BTreeMap::new(); parsed.len()];
    loop {
        let mut progress = false;
        for (i, module) in parsed.iter().enumerate() {
            for raw in &module.raws {
                if done[i].contains_key(&raw.name) {
                    continue;
                }
                let start = match &raw.parent {
                    None => Some(Vec::new()),
                    Some(parent) => done[i]
                        .get(parent)
                        .cloned()
                        .or_else(|| global_root.get(parent.as_str()).cloned())
                        .or_else(|| {
                            let from = module.imports.get(parent)?;
                            nodes.get(from)?.get(parent).cloned()
                        })
                        .or_else(|| nodes.get(&module.name)?.get(parent).cloned()),
                };
                if let Some(mut oid) = start {
                    oid.extend(&raw.arcs);
                    done[i].insert(raw.name.clone(), oid.clone());
                    nodes
                        .entry(module.name.clone())
                        .or_default()
                        .insert(raw.name.clone(), oid);
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }
    let mut missing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (i, module) in parsed.iter().enumerate() {
        for raw in &module.raws {
            if done[i].contains_key(&raw.name) {
                continue;
            }
            let Some(parent) = &raw.parent else { continue };
            let defined_here = module.raws.iter().any(|r| &r.name == parent);
            if !defined_here {
                let from = module
                    .imports
                    .get(parent)
                    .cloned()
                    .unwrap_or_else(|| format!("the module defining {parent}"));
                missing.entry(from).or_default().insert(parent.clone());
            }
        }
    }
    if !missing.is_empty() {
        return Err(MibError::Missing(missing));
    }
    Ok(parsed
        .iter()
        .enumerate()
        .map(|(i, module)| build_module(module, &done[i], &types))
        .collect())
}

/// A type by name, as `module` sees it: its own, imported, or built in.
fn lookup_type<'a>(
    module: &Parsed,
    name: &str,
    types: &'a BTreeMap<String, BTreeMap<String, TypeInfo>>,
) -> Option<&'a TypeInfo> {
    types
        .get(&module.name)
        .and_then(|t| t.get(name))
        .or_else(|| types.get(module.imports.get(name)?)?.get(name))
        .or_else(|| types.values().find_map(|t| t.get(name)))
}

fn build_module(
    module: &Parsed,
    oids: &BTreeMap<String, Vec<u32>>,
    types: &BTreeMap<String, BTreeMap<String, TypeInfo>>,
) -> Module {
    let entries: BTreeSet<&str> = module
        .raws
        .iter()
        .filter(|r| r.syntax.as_ref().is_some_and(|s| s.sequence_of))
        .filter_map(|r| oids.get(&r.name))
        .flat_map(|table| {
            module
                .raws
                .iter()
                .filter(move |r| {
                    oids.get(&r.name)
                        .is_some_and(|o| o.len() == table.len() + 1 && o.starts_with(table))
                })
                .map(|r| r.name.as_str())
        })
        .collect();
    let entry_oids: Vec<&Vec<u32>> = entries.iter().filter_map(|e| oids.get(*e)).collect();
    let mut objects = Vec::new();
    for raw in &module.raws {
        let Some(oid) = oids.get(&raw.name) else {
            continue;
        };
        let kind = match raw.macro_kind.as_str() {
            "OBJECT-TYPE" => {
                if raw.syntax.as_ref().is_some_and(|s| s.sequence_of) {
                    ObjectKind::Table
                } else if entries.contains(raw.name.as_str()) {
                    ObjectKind::Entry
                } else if entry_oids
                    .iter()
                    .any(|e| oid.len() == e.len() + 1 && oid.starts_with(e))
                {
                    ObjectKind::Column
                } else {
                    ObjectKind::Scalar
                }
            }
            "NOTIFICATION-TYPE" => ObjectKind::Notification,
            _ => ObjectKind::Node,
        };
        let (syntax, mut enumeration) = match &raw.syntax {
            None => (None, Vec::new()),
            Some(s) => {
                let named = lookup_type(module, &s.name, types);
                let enumeration = if s.enumeration.is_empty() {
                    named.map(|t| t.enumeration.clone()).unwrap_or_default()
                } else {
                    s.enumeration.clone()
                };
                (Some(s.name.clone()), enumeration)
            }
        };
        enumeration.truncate(MAX_ENUM);
        objects.push(Object {
            name: raw.name.clone(),
            oid: oid.clone(),
            kind,
            syntax,
            enumeration,
            units: raw.units.clone(),
            access: raw.access.clone(),
            description: raw.description.clone(),
        });
    }
    let module_types = types
        .get(&module.name)
        .map(|t| {
            module
                .types
                .keys()
                .filter_map(|k| Some((k.clone(), t.get(k)?.clone())))
                .collect()
        })
        .unwrap_or_default();
    Module {
        name: module.name.clone(),
        file: module.file.clone(),
        objects,
        types: module_types,
    }
}

/// The built-in modules imports resolve against: the SMI's roots and
/// textual conventions, and the standard objects ADR-0149 reads.
pub fn base() -> Known {
    let mut known = Known::default();
    let mut add = |module: &str, nodes: &[(&str, &[u32])]| {
        let m = known.modules.entry(module.to_owned()).or_default();
        for (name, oid) in nodes {
            m.nodes.insert((*name).to_owned(), oid.to_vec());
        }
    };
    let smi: &[(&str, &[u32])] = &[
        ("iso", &[1]),
        ("org", &[1, 3]),
        ("dod", &[1, 3, 6]),
        ("internet", &[1, 3, 6, 1]),
        ("directory", &[1, 3, 6, 1, 1]),
        ("mgmt", &[1, 3, 6, 1, 2]),
        ("mib-2", &[1, 3, 6, 1, 2, 1]),
        ("transmission", &[1, 3, 6, 1, 2, 1, 10]),
        ("experimental", &[1, 3, 6, 1, 3]),
        ("private", &[1, 3, 6, 1, 4]),
        ("enterprises", &[1, 3, 6, 1, 4, 1]),
        ("security", &[1, 3, 6, 1, 5]),
        ("snmpV2", &[1, 3, 6, 1, 6]),
        ("snmpDomains", &[1, 3, 6, 1, 6, 1]),
        ("snmpProxys", &[1, 3, 6, 1, 6, 2]),
        ("snmpModules", &[1, 3, 6, 1, 6, 3]),
        ("zeroDotZero", &[0, 0]),
    ];
    add("SNMPv2-SMI", smi);
    add("RFC1155-SMI", smi);
    add("RFC1065-SMI", smi);
    let system: &[(&str, &[u32])] = &[
        ("system", &[1, 3, 6, 1, 2, 1, 1]),
        ("sysDescr", &[1, 3, 6, 1, 2, 1, 1, 1]),
        ("sysObjectID", &[1, 3, 6, 1, 2, 1, 1, 2]),
        ("sysUpTime", &[1, 3, 6, 1, 2, 1, 1, 3]),
        ("sysName", &[1, 3, 6, 1, 2, 1, 1, 5]),
        ("interfaces", &[1, 3, 6, 1, 2, 1, 2]),
        ("ifIndex", &[1, 3, 6, 1, 2, 1, 2, 2, 1, 1]),
        ("ifDescr", &[1, 3, 6, 1, 2, 1, 2, 2, 1, 2]),
        ("ip", &[1, 3, 6, 1, 2, 1, 4]),
        ("snmp", &[1, 3, 6, 1, 2, 1, 11]),
    ];
    add("RFC1213-MIB", system);
    add("SNMPv2-MIB", &system[..5]);
    add(
        "IF-MIB",
        &[
            ("ifIndex", &[1, 3, 6, 1, 2, 1, 2, 2, 1, 1]),
            ("ifDescr", &[1, 3, 6, 1, 2, 1, 2, 2, 1, 2]),
            ("ifName", &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 1]),
            ("ifMIB", &[1, 3, 6, 1, 2, 1, 31]),
        ],
    );
    add(
        "HOST-RESOURCES-MIB",
        &[
            ("host", &[1, 3, 6, 1, 2, 1, 25]),
            ("hrDeviceIndex", &[1, 3, 6, 1, 2, 1, 25, 3, 2, 1, 1]),
            ("hrStorageIndex", &[1, 3, 6, 1, 2, 1, 25, 2, 3, 1, 1]),
        ],
    );
    add(
        "ENTITY-MIB",
        &[
            ("entityMIB", &[1, 3, 6, 1, 2, 1, 47]),
            ("entPhysicalIndex", &[1, 3, 6, 1, 2, 1, 47, 1, 1, 1, 1, 1]),
            ("entPhysicalName", &[1, 3, 6, 1, 2, 1, 47, 1, 1, 1, 1, 7]),
        ],
    );
    add("UPS-MIB", &[("upsMIB", &[1, 3, 6, 1, 2, 1, 33])]);
    add("Printer-MIB", &[("printmib", &[1, 3, 6, 1, 2, 1, 43])]);
    add(
        "ENTITY-SENSOR-MIB",
        &[("entitySensorMIB", &[1, 3, 6, 1, 2, 1, 99])],
    );
    add(
        "SNMP-FRAMEWORK-MIB",
        &[("snmpFrameworkMIB", &[1, 3, 6, 1, 6, 3, 10])],
    );
    let tc = known.modules.entry("SNMPv2-TC".to_owned()).or_default();
    let enum_type = |base: &str, items: &[(i64, &str)]| TypeInfo {
        base: base.to_owned(),
        enumeration: items.iter().map(|(v, n)| (*v, (*n).to_owned())).collect(),
        sequence: false,
    };
    tc.types.insert(
        "TruthValue".into(),
        enum_type("INTEGER", &[(1, "true"), (2, "false")]),
    );
    tc.types.insert(
        "RowStatus".into(),
        enum_type(
            "INTEGER",
            &[
                (1, "active"),
                (2, "notInService"),
                (3, "notReady"),
                (4, "createAndGo"),
                (5, "createAndWait"),
                (6, "destroy"),
            ],
        ),
    );
    tc.types.insert(
        "StorageType".into(),
        enum_type(
            "INTEGER",
            &[
                (1, "other"),
                (2, "volatile"),
                (3, "nonVolatile"),
                (4, "permanent"),
                (5, "readOnly"),
            ],
        ),
    );
    for plain in [
        "DisplayString",
        "PhysAddress",
        "MacAddress",
        "DateAndTime",
        "TimeStamp",
        "TimeInterval",
        "AutonomousType",
        "TestAndIncr",
        "VariablePointer",
        "RowPointer",
        "TimeStamp",
    ] {
        tc.types.insert(
            plain.into(),
            TypeInfo {
                base: plain.to_owned(),
                ..TypeInfo::default()
            },
        );
    }
    known
}

/// What a channel reading `object` should be: its kind and unit, guessed
/// from its type and units.
pub fn suggest(object: &Object) -> (crate::custom::ValueKind, String) {
    use crate::custom::ValueKind;
    let syntax = object.syntax.as_deref().unwrap_or("");
    let kind = if syntax.starts_with("Counter") || syntax == "ZeroBasedCounter64" {
        ValueKind::Counter
    } else if object.enumeration.is_empty()
        && (syntax.contains("String")
            || syntax == "OBJECT IDENTIFIER"
            || syntax == "IpAddress"
            || syntax == "DateAndTime"
            || syntax == "PhysAddress"
            || syntax == "MacAddress")
    {
        ValueKind::Text
    } else {
        ValueKind::Gauge
    };
    let unit = object
        .units
        .clone()
        .unwrap_or_default()
        .chars()
        .take(crate::custom::MAX_UNIT)
        .collect();
    (kind, unit)
}

#[cfg(test)]
#[path = "mib_tests.rs"]
mod tests;
