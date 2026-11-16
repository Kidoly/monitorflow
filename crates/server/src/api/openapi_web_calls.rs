//! What the web interface calls is in the public API (ADR-0143): every
//! path under `/api/` that a string or template literal of `web/src`
//! names must match a route, and each route it matches must be in the
//! OpenAPI document or left out of v1 with a reason.
//!
//! Paths are read from the sources, not from a list kept by hand, so a
//! new call to a new route fails here until the route is documented.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::super::v1;
use super::tests::{documented, routes};

/// Stands for a `${...}` of a template literal.
const HOLE: char = '\0';

/// The string and template literals of a TypeScript source that start
/// with `/api/`, outside comments, `${...}` written [`HOLE`]. The paths of
/// the public API itself (`/api/v1/`) are left aside.
fn api_literals(source: &str) -> Vec<String> {
    let mut lexer = Lexer {
        chars: source.chars().collect(),
        at: 0,
        found: Vec::new(),
    };
    lexer.scan(false);
    lexer.found
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    found: Vec<String>,
}

impl Lexer {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    /// Code up to the end, or up to the `}` that closes a `${` when
    /// `in_hole`.
    fn scan(&mut self, in_hole: bool) {
        let mut depth = 0;
        while let Some(c) = self.peek(0) {
            match (c, self.peek(1)) {
                ('/', Some('/')) => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                ('/', Some('*')) => {
                    self.at += 2;
                    while self.peek(0).is_some()
                        && (self.peek(0), self.peek(1)) != (Some('*'), Some('/'))
                    {
                        self.at += 1;
                    }
                    self.at += 2;
                }
                ('"' | '\'', _) => {
                    let text = self.quoted(c);
                    self.record(text);
                }
                ('`', _) => {
                    let text = self.template();
                    self.record(text);
                }
                ('{', _) if in_hole => {
                    depth += 1;
                    self.at += 1;
                }
                ('}', _) if in_hole => {
                    self.at += 1;
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                _ => self.at += 1,
            }
        }
    }

    /// A `'` or `"` string. It ends at the end of its line too, so a quote
    /// in the text of a TSX element costs one line at most.
    fn quoted(&mut self, quote: char) -> String {
        let mut text = String::new();
        self.at += 1;
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\\' => {
                    text.extend(self.peek(0));
                    self.at += 1;
                }
                '\n' => break,
                c if c == quote => break,
                c => text.push(c),
            }
        }
        text
    }

    fn template(&mut self) -> String {
        let mut text = String::new();
        self.at += 1;
        while let Some(c) = self.peek(0) {
            self.at += 1;
            match c {
                '\\' => {
                    text.extend(self.peek(0));
                    self.at += 1;
                }
                '`' => break,
                '$' if self.peek(0) == Some('{') => {
                    self.at += 1;
                    self.scan(true);
                    text.push(HOLE);
                }
                c => text.push(c),
            }
        }
        text
    }

    fn record(&mut self, text: String) {
        if text.starts_with("/api/") && !text.starts_with("/api/v1/") {
            self.found.push(text);
        }
    }
}

/// The segments of a literal's path: its query cut off, a segment that is
/// only a `${...}` as [`HOLE`], one appended to text (a query built
/// elsewhere, like `summary${query}`) dropped.
fn segments(literal: &str) -> Vec<String> {
    let path = literal.split('?').next().unwrap_or_default();
    path.trim_matches('/')
        .split('/')
        .map(|s| match s {
            "\0" => HOLE.to_string(),
            s => s.replace(HOLE, ""),
        })
        .collect()
}

/// Whether the literal's path is the route `template`'s: a `${...}`
/// stands for any segment, a `{param}` only for a `${...}`.
fn matches(literal: &[String], template: &str) -> bool {
    let template: Vec<&str> = template.trim_matches('/').split('/').collect();
    literal.len() == template.len()
        && literal
            .iter()
            .zip(template)
            .all(|(l, t)| l.starts_with(HOLE) || (l == t && !t.starts_with('{')))
}

fn shown(literal: &str) -> String {
    literal.replace(HOLE, "${…}")
}

/// The `.ts` and `.tsx` files under `dir`, tests left out.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "test" {
                sources(&path, out);
            }
        } else if (name.ends_with(".ts") || name.ends_with(".tsx")) && !name.contains(".test.") {
            out.push(path);
        }
    }
}

#[test]
fn every_path_the_web_interface_calls_is_in_the_public_api() {
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/src");
    let mut files = Vec::new();
    sources(&web, &mut files);
    // Each path with a file that calls it.
    let mut calls: BTreeMap<String, String> = BTreeMap::new();
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap();
        let name = file.strip_prefix(&web).unwrap().display().to_string();
        for literal in api_literals(&source) {
            calls.entry(literal).or_insert_with(|| name.clone());
        }
    }
    let from_client = calls.values().filter(|f| *f == "lib/api.ts").count();
    assert!(
        from_client > 150,
        "only {from_client} paths found in lib/api.ts: teach api_literals() its new syntax"
    );

    let routes = routes();
    let documented = documented();
    let mut used = BTreeSet::new();
    for (literal, file) in &calls {
        let path = segments(literal);
        let matched: Vec<&(String, String)> = routes
            .iter()
            .filter(|(_, template)| matches(&path, template))
            .collect();
        assert!(
            !matched.is_empty(),
            "web/src/{file} calls {}, which no route serves",
            shown(literal)
        );
        for (method, template) in matched {
            assert!(
                documented.contains(&(method.clone(), template.clone()))
                    || v1::left_out(method, template),
                "web/src/{file} calls {method} {template}, which is neither in the OpenAPI document \
                 nor left out of v1 with a reason"
            );
            used.insert((method, template));
        }
    }
    // The router's own test covers the routes no page calls, kept for
    // scripts; this one only adds that the pages' calls are among them.
    assert!(used.len() > 150, "{used:?}");
}

#[test]
fn literals_are_read_like_typescript() {
    let source = r#"
        // request("/api/commented")
        /* "/api/commented-too" */
        const a = request<X>(`/api/servers/${encodeURIComponent(id)}/logs${search ? `?${search}` : ""}`);
        const b = request("/api/servers/summary" + q, { method: "POST" });
        const c = '/api/sites';
        <p>Don't stop at an apostrophe</p>
        const d = `/api/${kind}/${id}/revoke`;
        const e = "/api/v1/openapi.json";
        const f = "https://example.com/api/x";
    "#;
    let found: Vec<String> = api_literals(source).iter().map(|l| shown(l)).collect();
    assert_eq!(
        found,
        [
            "/api/servers/${…}/logs${…}",
            "/api/servers/summary",
            "/api/sites",
            "/api/${…}/${…}/revoke",
        ]
    );
    let logs = segments(&api_literals(source)[0]);
    assert!(matches(&logs, "/api/servers/{id}/logs"));
    assert!(!matches(&logs, "/api/servers/{id}/commands"));
    let revoke = segments(&api_literals(source)[3]);
    assert!(matches(&revoke, "/api/relays/{id}/revoke"));
    // A literal segment is not a parameter.
    let summary = segments("/api/servers/summary?site_id=1");
    assert!(matches(&summary, "/api/servers/summary"));
    assert!(!matches(&summary, "/api/servers/{id}"));
}
