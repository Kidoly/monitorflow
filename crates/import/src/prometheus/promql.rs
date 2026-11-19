//! Just enough PromQL to read the usual alerting rules: a tokenizer, a
//! parser into a small syntax tree, and a reading of a comparison as a
//! condition on one known measure.
//!
//! The reading does not match strings. It turns both sides of a
//! comparison into a sum of terms, each a constant times a product of
//! known series ([`Leaf`]): `100 - avail / total * 100 > 90` and
//! `(1 - avail / total) * 100 > 90` both become `avail / total < 0.1`.
//! Grouping clauses (`by`, `without`, `on`, `group_left`) and the order of
//! label matchers make no difference. Anything it does not know, such as
//! `predict_linear`, `absent` or a series it has no name for, makes the
//! whole expression unreadable rather than guessed.

use std::collections::BTreeMap;
use std::fmt;

/// A token of an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Number(f64),
    Str(String),
    /// The text between `[` and `]`: a range or a subquery.
    Range(String),
    /// An operator or punctuation: `+ - * / % ^ == != > < >= <= =~ !~ =
    /// ( ) { } , @`.
    Op(&'static str),
}

/// Why an expression could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SyntaxError(String);

const OPS: &[&str] = &[
    "==", "!=", ">=", "<=", "=~", "!~", "+", "-", "*", "/", "%", "^", ">", "<", "=", "(", ")", "{",
    "}", ",", "@",
];

/// The tokens of `text`.
pub fn tokenize(text: &str) -> Result<Vec<Token>, SyntaxError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            // A comment runs to the end of the line.
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            let mut value = String::new();
            i += 1;
            loop {
                let Some(&next) = chars.get(i) else {
                    return Err(SyntaxError("a string is not closed".into()));
                };
                i += 1;
                if next == c {
                    break;
                }
                if next == '\\' && c != '`' {
                    let escaped = chars
                        .get(i)
                        .ok_or_else(|| SyntaxError("a string is not closed".into()))?;
                    i += 1;
                    value.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        other => *other,
                    });
                } else {
                    value.push(next);
                }
            }
            tokens.push(Token::Str(value));
            continue;
        }
        if c == '[' {
            let start = i + 1;
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
            if i == chars.len() {
                return Err(SyntaxError("a range is not closed".into()));
            }
            tokens.push(Token::Range(chars[start..i].iter().collect()));
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while let Some(&next) = chars.get(i) {
                // A sign belongs to the number only as an exponent's:
                // `1e-3`, not `5m-1` nor `2-1`.
                let exponent_sign = (next == '+' || next == '-')
                    && matches!(chars[i - 1], 'e' | 'E')
                    && chars[start..i - 1]
                        .iter()
                        .all(|c| c.is_ascii_digit() || *c == '.');
                if next.is_ascii_alphanumeric() || next == '.' || exponent_sign {
                    i += 1;
                } else {
                    break;
                }
            }
            let word: String = chars[start..i].iter().collect();
            let value = word
                .parse::<f64>()
                .ok()
                .or_else(|| crate::text::seconds(&word).map(|s| s as f64))
                .ok_or_else(|| SyntaxError(format!("{word} is not a number")))?;
            tokens.push(Token::Number(value));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == ':' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == ':')
            {
                i += 1;
            }
            tokens.push(Token::Ident(chars[start..i].iter().collect()));
            continue;
        }
        let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
        let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) else {
            return Err(SyntaxError(format!("unexpected {c}")));
        };
        tokens.push(Token::Op(op));
        i += op.chars().count();
    }
    Ok(tokens)
}

/// How a label matcher compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchOp {
    Eq,
    Ne,
    Re,
    Nre,
}

/// A label matcher such as `mountpoint="/"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Matcher {
    pub label: String,
    pub op: MatchOp,
    pub value: String,
}

impl Matcher {
    /// Whether `value` passes this matcher, as Prometheus anchors regular
    /// expressions. An invalid regular expression matches nothing.
    pub fn matches(&self, value: &str) -> bool {
        match self.op {
            MatchOp::Eq => value == self.value,
            MatchOp::Ne => value != self.value,
            MatchOp::Re | MatchOp::Nre => {
                let found = regex::Regex::new(&format!("^(?:{})$", self.value))
                    .is_ok_and(|re| re.is_match(value));
                found == (self.op == MatchOp::Re)
            }
        }
    }

    /// The literal values a positive matcher accepts: one for `=`, the
    /// alternatives of a regular expression made of plain words only
    /// (`nginx.service|sshd.service`). `None` otherwise.
    pub fn literals(&self) -> Option<Vec<String>> {
        match self.op {
            MatchOp::Eq => Some(vec![self.value.clone()]),
            MatchOp::Re => {
                let parts: Vec<String> = self
                    .value
                    .split('|')
                    .map(|p| p.replace("\\.", "."))
                    .collect();
                let plain = parts.iter().all(|p| {
                    !p.is_empty()
                        && p.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "._-@:".contains(c))
                });
                // An unescaped dot in a regex matches any character; a
                // unit name read with it as a dot is what the author meant.
                plain.then_some(parts)
            }
            MatchOp::Ne | MatchOp::Nre => None,
        }
    }
}

impl fmt::Display for Matcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.op {
            MatchOp::Eq => "=",
            MatchOp::Ne => "!=",
            MatchOp::Re => "=~",
            MatchOp::Nre => "!~",
        };
        write!(f, "{}{op}{:?}", self.label, self.value)
    }
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    And,
    Or,
    Unless,
    Atan2,
}

impl BinOp {
    fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Gt | Self::Lt | Self::Ge | Self::Le
        )
    }

    /// Binding power: higher binds tighter.
    fn power(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And | Self::Unless => 2,
            Self::Eq | Self::Ne | Self::Gt | Self::Lt | Self::Ge | Self::Le => 3,
            Self::Add | Self::Sub => 4,
            Self::Mul | Self::Div | Self::Mod | Self::Atan2 => 5,
            Self::Pow => 6,
        }
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Str(String),
    /// A series selector, with its range when it has one. `exotic` is set
    /// by `offset`, `@` and subqueries, which the reading refuses.
    Selector {
        metric: Option<String>,
        matchers: Vec<Matcher>,
        range: Option<String>,
        exotic: bool,
    },
    Call {
        func: String,
        args: Vec<Expr>,
    },
    /// An aggregation; its grouping is kept only as `by` or `without`.
    Agg {
        op: String,
        without: bool,
        labels: Vec<String>,
        param: Option<Box<Expr>>,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        /// The `bool` modifier of a comparison.
        bool_modifier: bool,
    },
    Neg(Box<Expr>),
}

const AGGREGATIONS: &[&str] = &[
    "sum",
    "avg",
    "min",
    "max",
    "count",
    "stddev",
    "stdvar",
    "group",
    "topk",
    "bottomk",
    "quantile",
    "count_values",
    "limitk",
    "limit_ratio",
];

/// Parses an expression.
pub fn parse(text: &str) -> Result<Expr, SyntaxError> {
    let tokens = tokenize(text)?;
    let mut parser = Parser { tokens, at: 0 };
    let expr = parser.expr(0)?;
    if parser.at != parser.tokens.len() {
        return Err(SyntaxError(format!(
            "unexpected {:?}",
            parser.tokens[parser.at]
        )));
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Token::Op(o)) if *o == op) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<(), SyntaxError> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(SyntaxError(format!("expected {op}")))
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Ident(w)) if w.eq_ignore_ascii_case(word)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// The binary operator at the cursor, without moving.
    fn binary_op(&self) -> Option<BinOp> {
        Some(match self.peek()? {
            Token::Op("+") => BinOp::Add,
            Token::Op("-") => BinOp::Sub,
            Token::Op("*") => BinOp::Mul,
            Token::Op("/") => BinOp::Div,
            Token::Op("%") => BinOp::Mod,
            Token::Op("^") => BinOp::Pow,
            Token::Op("==") => BinOp::Eq,
            Token::Op("!=") => BinOp::Ne,
            Token::Op(">") => BinOp::Gt,
            Token::Op("<") => BinOp::Lt,
            Token::Op(">=") => BinOp::Ge,
            Token::Op("<=") => BinOp::Le,
            Token::Ident(w) => match w.to_ascii_lowercase().as_str() {
                "and" => BinOp::And,
                "or" => BinOp::Or,
                "unless" => BinOp::Unless,
                "atan2" => BinOp::Atan2,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// A label list `(a, b)`, possibly empty.
    fn labels(&mut self) -> Result<Vec<String>, SyntaxError> {
        self.expect_op("(")?;
        let mut labels = Vec::new();
        loop {
            if self.eat_op(")") {
                return Ok(labels);
            }
            match self.next() {
                Some(Token::Ident(l)) => labels.push(l),
                _ => return Err(SyntaxError("expected a label name".into())),
            }
            if !self.eat_op(",") {
                self.expect_op(")")?;
                return Ok(labels);
            }
        }
    }

    fn expr(&mut self, min_power: u8) -> Result<Expr, SyntaxError> {
        let mut lhs = self.unary()?;
        while let Some(op) = self.binary_op() {
            let power = op.power();
            if power < min_power {
                break;
            }
            self.at += 1;
            let bool_modifier = self.eat_word("bool");
            // Vector matching changes nothing for one series per host.
            if self.eat_word("on") || self.eat_word("ignoring") {
                self.labels()?;
            }
            if (self.eat_word("group_left") || self.eat_word("group_right"))
                && matches!(self.peek(), Some(Token::Op("(")))
            {
                self.labels()?;
            }
            // `^` is right-associative: the right side may hold another.
            let next = if op == BinOp::Pow { power } else { power + 1 };
            let rhs = self.expr(next)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                bool_modifier,
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, SyntaxError> {
        if self.eat_op("-") {
            // `-a ^ b` is `-(a ^ b)`.
            let inner = self.power()?;
            return Ok(Expr::Neg(Box::new(inner)));
        }
        if self.eat_op("+") {
            return self.power();
        }
        self.power()
    }

    fn power(&mut self) -> Result<Expr, SyntaxError> {
        let base = self.postfix()?;
        if matches!(self.peek(), Some(Token::Op("^"))) {
            self.at += 1;
            let exponent = self.unary()?;
            return Ok(Expr::Binary {
                op: BinOp::Pow,
                lhs: Box::new(base),
                rhs: Box::new(exponent),
                bool_modifier: false,
            });
        }
        Ok(base)
    }

    /// A primary with what may follow it: a range, a subquery, `offset`
    /// or `@`.
    fn postfix(&mut self) -> Result<Expr, SyntaxError> {
        let mut expr = self.primary()?;
        loop {
            match self.peek() {
                Some(Token::Range(range)) => {
                    let range = range.clone();
                    self.at += 1;
                    expr = match expr {
                        Expr::Selector {
                            metric,
                            matchers,
                            range: None,
                            exotic,
                        } => Expr::Selector {
                            metric,
                            matchers,
                            exotic: exotic || range.contains(':'),
                            range: Some(range),
                        },
                        // A subquery: kept as an unknown call.
                        other => Expr::Call {
                            func: "subquery".into(),
                            args: vec![other],
                        },
                    };
                }
                Some(Token::Ident(w)) if w.eq_ignore_ascii_case("offset") => {
                    self.at += 1;
                    let negative = self.eat_op("-");
                    let seconds = match self.next() {
                        Some(Token::Number(n)) => n,
                        _ => return Err(SyntaxError("expected a duration after offset".into())),
                    };
                    // Kept with its duration, for comparisons with the past
                    // (ADR-0153); every other reading refuses it.
                    let seconds = if negative { -seconds } else { seconds };
                    expr = Expr::Call {
                        func: "offset".into(),
                        args: vec![mark_exotic(expr), Expr::Number(seconds)],
                    };
                }
                Some(Token::Op("@")) => {
                    self.at += 1;
                    match self.next() {
                        Some(Token::Number(_)) => {}
                        Some(Token::Ident(_)) => {
                            self.expect_op("(")?;
                            self.expect_op(")")?;
                        }
                        _ => return Err(SyntaxError("expected a time after @".into())),
                    }
                    expr = mark_exotic(expr);
                }
                _ => return Ok(expr),
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, SyntaxError> {
        match self.next() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Str(s)) => Ok(Expr::Str(s)),
            Some(Token::Op("(")) => {
                let inner = self.expr(0)?;
                self.expect_op(")")?;
                Ok(inner)
            }
            Some(Token::Op("{")) => {
                let matchers = self.matchers()?;
                let metric = matchers
                    .iter()
                    .find(|m| m.label == "__name__" && m.op == MatchOp::Eq)
                    .map(|m| m.value.clone());
                Ok(Expr::Selector {
                    metric,
                    matchers: matchers
                        .into_iter()
                        .filter(|m| !(m.label == "__name__" && m.op == MatchOp::Eq))
                        .collect(),
                    range: None,
                    exotic: false,
                })
            }
            Some(Token::Ident(name)) => {
                let lower = name.to_ascii_lowercase();
                if lower == "inf" || lower == "nan" {
                    return Ok(Expr::Number(if lower == "inf" {
                        f64::INFINITY
                    } else {
                        f64::NAN
                    }));
                }
                if AGGREGATIONS.contains(&lower.as_str()) && self.is_aggregation_ahead() {
                    return self.aggregation(lower);
                }
                if matches!(self.peek(), Some(Token::Op("("))) {
                    self.at += 1;
                    let mut args = Vec::new();
                    if !self.eat_op(")") {
                        loop {
                            args.push(self.expr(0)?);
                            if self.eat_op(")") {
                                break;
                            }
                            self.expect_op(",")?;
                        }
                    }
                    return Ok(Expr::Call { func: lower, args });
                }
                let matchers = if self.eat_op("{") {
                    self.matchers()?
                } else {
                    Vec::new()
                };
                Ok(Expr::Selector {
                    metric: Some(name),
                    matchers,
                    range: None,
                    exotic: false,
                })
            }
            Some(other) => Err(SyntaxError(format!("unexpected {other:?}"))),
            None => Err(SyntaxError("the expression ends too early".into())),
        }
    }

    /// Whether the word just read starts an aggregation: followed by `(`,
    /// `by` or `without`.
    fn is_aggregation_ahead(&self) -> bool {
        match self.peek() {
            Some(Token::Op("(")) => true,
            Some(Token::Ident(w)) => {
                w.eq_ignore_ascii_case("by") || w.eq_ignore_ascii_case("without")
            }
            _ => false,
        }
    }

    fn aggregation(&mut self, op: String) -> Result<Expr, SyntaxError> {
        let mut without = false;
        let mut labels = Vec::new();
        let mut grouping = |parser: &mut Self| -> Result<(), SyntaxError> {
            if parser.eat_word("by") {
                labels = parser.labels()?;
            } else if parser.eat_word("without") {
                without = true;
                labels = parser.labels()?;
            }
            Ok(())
        };
        grouping(self)?;
        self.expect_op("(")?;
        let mut args = vec![self.expr(0)?];
        while self.eat_op(",") {
            args.push(self.expr(0)?);
        }
        self.expect_op(")")?;
        grouping(self)?;
        let expr = args.pop().unwrap_or(Expr::Number(0.0));
        let param = args.pop().map(Box::new);
        Ok(Expr::Agg {
            op,
            without,
            labels,
            param,
            expr: Box::new(expr),
        })
    }

    /// Label matchers after `{`, up to and with `}`.
    fn matchers(&mut self) -> Result<Vec<Matcher>, SyntaxError> {
        let mut matchers = Vec::new();
        loop {
            if self.eat_op("}") {
                return Ok(matchers);
            }
            let label = match self.next() {
                Some(Token::Ident(l)) => l,
                // A quoted metric name, as Prometheus 3 allows.
                Some(Token::Str(s))
                    if matches!(self.peek(), Some(Token::Op(",") | Token::Op("}"))) =>
                {
                    matchers.push(Matcher {
                        label: "__name__".into(),
                        op: MatchOp::Eq,
                        value: s,
                    });
                    if !self.eat_op(",") {
                        self.expect_op("}")?;
                        return Ok(matchers);
                    }
                    continue;
                }
                Some(Token::Str(l)) => l,
                _ => return Err(SyntaxError("expected a label matcher".into())),
            };
            let op = match self.next() {
                Some(Token::Op("=")) | Some(Token::Op("==")) => MatchOp::Eq,
                Some(Token::Op("!=")) => MatchOp::Ne,
                Some(Token::Op("=~")) => MatchOp::Re,
                Some(Token::Op("!~")) => MatchOp::Nre,
                _ => return Err(SyntaxError(format!("expected an operator after {label}"))),
            };
            let Some(Token::Str(value)) = self.next() else {
                return Err(SyntaxError(format!("expected a string after {label}")));
            };
            matchers.push(Matcher { label, op, value });
            if !self.eat_op(",") {
                self.expect_op("}")?;
                return Ok(matchers);
            }
        }
    }
}

fn mark_exotic(expr: Expr) -> Expr {
    match expr {
        Expr::Selector {
            metric,
            matchers,
            range,
            ..
        } => Expr::Selector {
            metric,
            matchers,
            range,
            exotic: true,
        },
        other => Expr::Call {
            func: "offset".into(),
            args: vec![other],
        },
    }
}

/// A series the reading knows, with the matchers of its selector. Two
/// leaves are the same factor when their kinds are the same: the matchers
/// tell which hosts, mounts or devices, not what is measured.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LeafKind {
    MemAvailable,
    MemTotal,
    /// Space left for unprivileged users, or free space (`free_bytes`).
    FsAvail,
    FsSize,
    Load(u8),
    /// Average share of idle time of the processors, 0 to 1.
    CpuIdle,
    /// Share of busy time of the processors, 0 to 1; `exact` is false
    /// when some busy modes (such as iowait) are left out.
    CpuBusy {
        exact: bool,
    },
    /// The number of processors.
    CpuCount,
    DiskReadTime,
    DiskReads,
    DiskWriteTime,
    DiskWrites,
    DiskIoTimeWeighted,
    DiskIoTime,
    Up,
    ProbeSuccess,
    SystemdUnit,
    /// The status code of a probe's answer.
    ProbeStatusCode,
    /// How long a probe took, in seconds; `whole` is false for a phase
    /// of an HTTP probe (`probe_http_duration_seconds`).
    ProbeDuration {
        whole: bool,
    },
    /// When a probe's certificate expires, Unix seconds.
    CertExpiry,
    /// `time()`.
    Time,
    /// Seconds a probe's certificate has left: `CertExpiry - Time`.
    CertSecondsLeft,
}

/// What a function over a range of a gauge reads (ADR-0153).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Window {
    /// `avg_over_time`, `min_over_time`, `max_over_time` over seconds.
    Avg(u32),
    Min(u32),
    Max(u32),
    /// `delta`: the last value minus the first.
    Change(u32),
    /// `deriv`: the slope per second, of a least-squares line.
    Slope(u32),
    /// `predict_linear` over `over` seconds, `within` seconds ahead.
    Predict {
        over: u32,
        within: u32,
    },
}

/// A known series in an expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Leaf {
    pub kind: LeafKind,
    pub matchers: Vec<Matcher>,
    /// The function over a range it is read through, for a gauge.
    pub window: Option<Window>,
}

/// A product of leaves with their exponents, keyed by kind.
type Monomial = BTreeMap<LeafKind, i32>;

/// A sum of constant times product of leaves.
#[derive(Debug, Clone, Default)]
struct Poly {
    terms: Vec<(f64, Monomial)>,
}

impl Poly {
    fn constant(value: f64) -> Self {
        Self {
            terms: vec![(value, Monomial::new())],
        }
    }

    fn leaf(kind: LeafKind) -> Self {
        Self {
            terms: vec![(1.0, Monomial::from([(kind, 1)]))],
        }
    }

    /// Adds like terms and drops zero ones.
    fn normalized(mut self) -> Self {
        let mut out: Vec<(f64, Monomial)> = Vec::new();
        for (c, m) in self.terms.drain(..) {
            let m: Monomial = m.into_iter().filter(|(_, e)| *e != 0).collect();
            match out.iter_mut().find(|(_, o)| *o == m) {
                Some((oc, _)) => *oc += c,
                None => out.push((c, m)),
            }
        }
        out.retain(|(c, _)| c.abs() > 1e-12);
        Self { terms: out }
    }

    fn add(mut self, other: Self) -> Self {
        self.terms.extend(other.terms);
        self.normalized()
    }

    fn scale(mut self, k: f64) -> Self {
        for (c, _) in &mut self.terms {
            *c *= k;
        }
        self.normalized()
    }

    fn mul(self, other: &Self) -> Self {
        let mut terms = Vec::new();
        for (c1, m1) in &self.terms {
            for (c2, m2) in &other.terms {
                let mut m = m1.clone();
                for (k, e) in m2 {
                    *m.entry(k.clone()).or_default() += e;
                }
                terms.push((c1 * c2, m));
            }
        }
        Self { terms }.normalized()
    }

    /// Division by a single term only: `(a - b) / c` distributes, `a / (b
    /// + c)` does not.
    fn div(self, other: &Self) -> Option<Self> {
        let [(c, m)] = other.terms.as_slice() else {
            return None;
        };
        if c.abs() < 1e-12 {
            return None;
        }
        let inverse = Self {
            terms: vec![(1.0 / c, m.iter().map(|(k, e)| (k.clone(), -e)).collect())],
        };
        Some(self.mul(&inverse))
    }

    #[cfg(test)]
    fn as_constant(&self) -> Option<f64> {
        match self.terms.as_slice() {
            [] => Some(0.0),
            [(c, m)] if m.is_empty() => Some(*c),
            _ => None,
        }
    }
}

/// A comparison operator read as on its measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
}

impl Cmp {
    fn flipped(self) -> Self {
        match self {
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            other => other,
        }
    }

    fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Ge => value >= threshold,
            Self::Lt => value < threshold,
            Self::Le => value <= threshold,
            Self::Eq => (value - threshold).abs() < 1e-9,
            Self::Ne => (value - threshold).abs() >= 1e-9,
        }
    }
}

/// A condition on one product of known series: `monomial cmp value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    /// The factors and their exponents, `{MemAvailable: 1, MemTotal: -1}`.
    pub factors: Vec<(LeafKind, i32)>,
    pub cmp: Cmp,
    pub value: f64,
}

impl Condition {
    /// The factors, if they are exactly `kinds` with exponents.
    pub fn is(&self, kinds: &[(LeafKind, i32)]) -> bool {
        self.factors.len() == kinds.len() && kinds.iter().all(|k| self.factors.contains(k))
    }

    /// Whether the condition holds when a 0-or-1 series is 0 and not when
    /// it is 1: `up == 0`, `up < 1`, `probe_success != 1`.
    pub fn is_false_check(&self) -> bool {
        self.cmp.holds(0.0, self.value) && !self.cmp.holds(1.0, self.value)
    }

    /// The opposite: holds at 1, not at 0 (`state="failed"} == 1`).
    pub fn is_true_check(&self) -> bool {
        self.cmp.holds(1.0, self.value) && !self.cmp.holds(0.0, self.value)
    }
}

/// What an alerting rule's expression says.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub condition: Condition,
    /// Every known series of the condition, with its matchers.
    pub leaves: Vec<Leaf>,
    /// Whether the expression adds a condition the reading dropped (an
    /// `and` with more than a harmless filter).
    pub extra_condition: bool,
    /// Other conditions joined with `and` that could be read (ADR-0153).
    pub also: Vec<Reading>,
}

impl Reading {
    /// The function over a range the condition reads through, if any.
    pub fn window(&self) -> Option<Window> {
        self.leaves.iter().find_map(|l| l.window)
    }

    /// The matchers on `label`, from every series of the condition.
    pub fn matchers(&self, label: &str) -> Vec<&Matcher> {
        let mut out: Vec<&Matcher> = Vec::new();
        for leaf in &self.leaves {
            for m in &leaf.matchers {
                if m.label == label && !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// The value of an exact matcher on `label`, if one series has one.
    pub fn exact(&self, label: &str) -> Option<String> {
        self.matchers(label)
            .into_iter()
            .find(|m| m.op == MatchOp::Eq)
            .map(|m| m.value.clone())
    }
}

/// Reads `text` as a condition on one known measure, or `None`.
pub fn read(text: &str) -> Option<Reading> {
    let expr = parse(text).ok()?;
    read_expr(&expr)
}

/// Reads `text` as conditions joined by `or`, each on one known measure:
/// `probe_http_status_code <= 199 or probe_http_status_code >= 400`.
pub fn read_alternatives(text: &str) -> Option<Vec<Reading>> {
    fn split<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
        match expr {
            Expr::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                split(lhs, out);
                split(rhs, out);
            }
            other => out.push(other),
        }
    }
    let expr = parse(text).ok()?;
    let mut parts = Vec::new();
    split(&expr, &mut parts);
    parts.into_iter().map(read_expr).collect()
}

fn read_expr(expr: &Expr) -> Option<Reading> {
    // `(...) * on(instance) group_left(nodename) node_uname_info` only adds
    // the host name to the alert: the value is the left side's.
    if let Expr::Binary {
        op: BinOp::Mul,
        lhs,
        rhs,
        ..
    } = expr
    {
        if is_uname(rhs) {
            return read_expr(lhs);
        }
        if is_uname(lhs) {
            return read_expr(rhs);
        }
    }
    if let Expr::Binary {
        op: BinOp::And,
        lhs,
        rhs,
        ..
    } = expr
    {
        let mut reading = read_expr(lhs)?;
        if !is_harmless_filter(rhs) {
            match read_expr(rhs) {
                Some(other) if other.also.is_empty() && !other.extra_condition => {
                    reading.also.push(other);
                }
                _ => reading.extra_condition = true,
            }
        }
        return Some(reading);
    }
    // `3 <= days < 20`: the first comparison only filters, the second is
    // the condition.
    if let Expr::Binary {
        op,
        lhs,
        rhs,
        bool_modifier: false,
    } = expr
    {
        if let Expr::Binary {
            op: filter,
            lhs: low,
            rhs: inner,
            bool_modifier: false,
        } = &**lhs
        {
            if op.is_comparison() && filter.is_comparison() && matches!(**low, Expr::Number(_)) {
                let condition = Expr::Binary {
                    op: *op,
                    lhs: inner.clone(),
                    rhs: rhs.clone(),
                    bool_modifier: false,
                };
                let mut reading = read_expr(&condition)?;
                reading.extra_condition = true;
                return Some(reading);
            }
        }
    }
    let Expr::Binary {
        op,
        lhs,
        rhs,
        bool_modifier: false,
    } = expr
    else {
        return None;
    };
    if !op.is_comparison() {
        return None;
    }
    let mut leaves = Vec::new();
    let left = poly(lhs, &mut leaves)?;
    let right = poly(rhs, &mut leaves)?;
    let difference = seconds_left(left.add(right.scale(-1.0)));
    // One product of series plus a constant.
    let (variable, constant): (Vec<_>, Vec<_>) =
        difference.terms.iter().partition(|(_, m)| !m.is_empty());
    let [(coefficient, monomial)] = variable.as_slice() else {
        return None;
    };
    let constant: f64 = constant.iter().map(|(c, _)| c).sum();
    let mut cmp = match op {
        BinOp::Gt => Cmp::Gt,
        BinOp::Ge => Cmp::Ge,
        BinOp::Lt => Cmp::Lt,
        BinOp::Le => Cmp::Le,
        BinOp::Eq => Cmp::Eq,
        BinOp::Ne => Cmp::Ne,
        _ => return None,
    };
    if *coefficient < 0.0 {
        cmp = cmp.flipped();
    }
    let value = round(-constant / coefficient);
    Some(Reading {
        condition: Condition {
            factors: monomial.iter().map(|(k, e)| (k.clone(), *e)).collect(),
            cmp,
            value,
        },
        leaves,
        extra_condition: false,
        also: Vec::new(),
    })
}

/// `c * expiry - c * time()` as `c * seconds left`: the one sum of two
/// series the reading takes, a certificate's time left.
fn seconds_left(poly: Poly) -> Poly {
    let one = |kind: LeafKind| Monomial::from([(kind, 1)]);
    let expiry = poly
        .terms
        .iter()
        .find(|(_, m)| *m == one(LeafKind::CertExpiry))
        .map(|(c, _)| *c);
    let time = poly
        .terms
        .iter()
        .find(|(_, m)| *m == one(LeafKind::Time))
        .map(|(c, _)| *c);
    match (expiry, time) {
        (Some(e), Some(t)) if (e + t).abs() < 1e-12 => {
            let mut terms: Vec<(f64, Monomial)> = poly
                .terms
                .into_iter()
                .filter(|(_, m)| *m != one(LeafKind::CertExpiry) && *m != one(LeafKind::Time))
                .collect();
            terms.push((e, one(LeafKind::CertSecondsLeft)));
            Poly { terms }
        }
        _ => poly,
    }
}

/// A value without the noise of floating point arithmetic.
pub fn round(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

fn is_uname(expr: &Expr) -> bool {
    matches!(expr, Expr::Selector { metric: Some(m), .. } if m == "node_uname_info")
}

/// A filter of an `and` that changes nothing worth saying: a file system
/// not read-only, or a guard that some activity exists (`rate(...) > 0`).
fn is_harmless_filter(expr: &Expr) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = expr else {
        return false;
    };
    let zero = matches!(**rhs, Expr::Number(n) if n == 0.0);
    match op {
        BinOp::Eq => {
            zero && matches!(&**lhs, Expr::Selector { metric: Some(m), .. } if m == "node_filesystem_readonly")
        }
        BinOp::Gt => zero,
        _ => false,
    }
}

/// `expr` as a polynomial of known series, collecting the leaves.
fn poly(expr: &Expr, leaves: &mut Vec<Leaf>) -> Option<Poly> {
    match expr {
        Expr::Number(n) if n.is_finite() => Some(Poly::constant(*n)),
        Expr::Neg(inner) => Some(poly(inner, leaves)?.scale(-1.0)),
        Expr::Binary {
            op,
            lhs,
            rhs,
            bool_modifier: false,
        } => {
            let l = poly(lhs, leaves)?;
            let r = poly(rhs, leaves)?;
            match op {
                BinOp::Add => Some(l.add(r)),
                BinOp::Sub => Some(l.add(r.scale(-1.0))),
                BinOp::Mul => Some(l.mul(&r)),
                BinOp::Div => l.div(&r),
                _ => None,
            }
        }
        // Rounding a value changes nothing worth saying.
        Expr::Call { func, args } if func == "round" && matches!(args.len(), 1 | 2) => {
            poly(&args[0], leaves)
        }
        _ => {
            let leaf = leaf(expr)?;
            let kind = leaf.kind.clone();
            leaves.push(leaf);
            Some(Poly::leaf(kind))
        }
    }
}

/// A node gauge read through a function over its range (ADR-0153):
/// `avg_over_time(node_load1[15m])`, `delta(...)`, `deriv(...)`,
/// `predict_linear(node_filesystem_avail_bytes[6h], 4 * 3600)`.
fn gauge_over_range(func: &str, args: &[Expr]) -> Option<Leaf> {
    let (arg, within) = match args {
        [arg] => (arg, None),
        [arg, ahead] if func == "predict_linear" => {
            let mut leaves = Vec::new();
            let value = match poly(ahead, &mut leaves)?.terms.as_slice() {
                [(v, m)] if m.is_empty() && *v > 0.0 => *v,
                _ => return None,
            };
            (arg, Some(value as u32))
        }
        _ => return None,
    };
    let Expr::Selector {
        range: Some(range), ..
    } = arg
    else {
        return None;
    };
    let seconds = u32::try_from(crate::text::seconds(range)?).ok()?;
    let (metric, matchers) = selector(arg)?;
    let kind = match metric {
        "node_memory_MemAvailable_bytes" => LeafKind::MemAvailable,
        "node_memory_MemTotal_bytes" => LeafKind::MemTotal,
        "node_filesystem_avail_bytes" | "node_filesystem_free_bytes" => LeafKind::FsAvail,
        "node_filesystem_size_bytes" => LeafKind::FsSize,
        "node_load1" => LeafKind::Load(1),
        "node_load5" => LeafKind::Load(5),
        "node_load15" => LeafKind::Load(15),
        _ => return None,
    };
    let window = match (func, within) {
        ("avg_over_time", None) => Window::Avg(seconds),
        ("min_over_time", None) => Window::Min(seconds),
        ("max_over_time", None) => Window::Max(seconds),
        ("delta", None) => Window::Change(seconds),
        ("deriv", None) => Window::Slope(seconds),
        ("predict_linear", Some(within)) => Window::Predict {
            over: seconds,
            within,
        },
        _ => return None,
    };
    let mut matchers = matchers.to_vec();
    matchers.sort();
    Some(Leaf {
        kind,
        matchers,
        window: Some(window),
    })
}

/// `lhs / lhs offset D cmp k` or `lhs - lhs offset D cmp k`: a comparison
/// with `D` seconds earlier (ADR-0153). Returns the reading of `lhs cmp k`,
/// whether it divides, and `D`.
pub fn read_compare(text: &str) -> Option<(Reading, bool, u32)> {
    let expr = parse(text).ok()?;
    let Expr::Binary {
        op,
        lhs,
        rhs,
        bool_modifier: false,
    } = &expr
    else {
        return None;
    };
    if !op.is_comparison() {
        return None;
    }
    let Expr::Number(_) = **rhs else {
        return None;
    };
    let Expr::Binary {
        op: inner,
        lhs: now,
        rhs: then,
        ..
    } = &**lhs
    else {
        return None;
    };
    let ratio = match inner {
        BinOp::Div => true,
        BinOp::Sub => false,
        _ => return None,
    };
    let (past, seconds) = without_offset(then)?;
    if !same_ignoring_offset(now, &past) || seconds <= 0.0 {
        return None;
    }
    let condition = Expr::Binary {
        op: *op,
        lhs: now.clone(),
        rhs: rhs.clone(),
        bool_modifier: false,
    };
    Some((read_expr(&condition)?, ratio, seconds as u32))
}

/// `absent(selector)` or `absent_over_time(selector[range])`: the series
/// it watches, and the range when there is one (ADR-0153).
pub fn read_absent(text: &str) -> Option<(Reading, Option<u32>)> {
    let expr = parse(text).ok()?;
    let Expr::Call { func, args } = &expr else {
        return None;
    };
    let [arg] = args.as_slice() else {
        return None;
    };
    let range = match (func.as_str(), arg) {
        ("absent", Expr::Selector { range: None, .. }) => None,
        (
            "absent_over_time",
            Expr::Selector {
                range: Some(range), ..
            },
        ) => Some(u32::try_from(crate::text::seconds(range)?).ok()?),
        _ => return None,
    };
    let leaf = leaf(arg)?;
    let reading = Reading {
        condition: Condition {
            factors: vec![(leaf.kind.clone(), 1)],
            cmp: Cmp::Eq,
            value: 0.0,
        },
        leaves: vec![leaf],
        extra_condition: false,
        also: Vec::new(),
    };
    Some((reading, range))
}

/// `expr` with its one `offset` taken out, and the offset in seconds.
fn without_offset(expr: &Expr) -> Option<(Expr, f64)> {
    match expr {
        Expr::Call { func, args } if func == "offset" => match args.as_slice() {
            [inner, Expr::Number(seconds)] => Some((unmarked(inner), *seconds)),
            _ => None,
        },
        Expr::Call { func, args } => {
            let mut found = None;
            let mut out = Vec::new();
            for arg in args {
                match (without_offset(arg), &found) {
                    (Some((inner, s)), None) => {
                        found = Some(s);
                        out.push(inner);
                    }
                    (Some(_), Some(_)) => return None,
                    (None, _) => out.push(arg.clone()),
                }
            }
            Some((
                Expr::Call {
                    func: func.clone(),
                    args: out,
                },
                found?,
            ))
        }
        Expr::Agg {
            op,
            without,
            labels,
            param,
            expr: inner,
        } => {
            let (inner, s) = without_offset(inner)?;
            Some((
                Expr::Agg {
                    op: op.clone(),
                    without: *without,
                    labels: labels.clone(),
                    param: param.clone(),
                    expr: Box::new(inner),
                },
                s,
            ))
        }
        _ => None,
    }
}

/// A selector as written before `offset` marked it.
fn unmarked(expr: &Expr) -> Expr {
    match expr {
        Expr::Selector {
            metric,
            matchers,
            range,
            ..
        } => Expr::Selector {
            metric: metric.clone(),
            matchers: matchers.clone(),
            exotic: range.as_deref().is_some_and(|r| r.contains(':')),
            range: range.clone(),
        },
        other => other.clone(),
    }
}

fn same_ignoring_offset(a: &Expr, b: &Expr) -> bool {
    a == b
}

/// The matchers of a selector other than its metric name.
fn selector(expr: &Expr) -> Option<(&str, &[Matcher])> {
    match expr {
        Expr::Selector {
            metric: Some(m),
            matchers,
            exotic: false,
            ..
        } => Some((m.as_str(), matchers.as_slice())),
        _ => None,
    }
}

/// The selector inside `rate(...)` or `irate(...)` over a range.
fn rate_of(expr: &Expr) -> Option<(&str, &[Matcher])> {
    let Expr::Call { func, args } = expr else {
        return None;
    };
    if func != "rate" && func != "irate" {
        return None;
    }
    let [arg] = args.as_slice() else { return None };
    match arg {
        Expr::Selector { range: Some(_), .. } => selector(arg),
        _ => None,
    }
}

/// Whether an aggregation keeps one series per host: grouping by labels
/// that include the instance (or nothing, where one host is selected), or
/// without the labels of `inner` that tell processors or modes apart.
fn per_host(without: bool, labels: &[String], inner: &[&str]) -> bool {
    if without {
        labels.iter().all(|l| inner.contains(&l.as_str()))
    } else {
        labels.is_empty() || labels.iter().any(|l| l == "instance")
    }
}

fn has(matchers: &[Matcher], label: &str, op: MatchOp, value: &str) -> bool {
    matchers
        .iter()
        .any(|m| m.label == label && m.op == op && m.value == value)
}

/// The series of a blackbox exporter's probe a website check measures
/// too.
fn probe_kind(metric: &str) -> Option<LeafKind> {
    Some(match metric {
        "probe_success" => LeafKind::ProbeSuccess,
        "probe_http_status_code" => LeafKind::ProbeStatusCode,
        "probe_duration_seconds" => LeafKind::ProbeDuration { whole: true },
        "probe_http_duration_seconds" => LeafKind::ProbeDuration { whole: false },
        "probe_ssl_earliest_cert_expiry" => LeafKind::CertExpiry,
        _ => return None,
    })
}

fn leaf(expr: &Expr) -> Option<Leaf> {
    let make = |kind: LeafKind, matchers: &[Matcher]| {
        let mut matchers = matchers.to_vec();
        matchers.sort();
        Some(Leaf {
            kind,
            matchers,
            window: None,
        })
    };
    if let Some((metric, matchers)) = selector(expr) {
        let kind = match metric {
            "node_memory_MemAvailable_bytes" => LeafKind::MemAvailable,
            "node_memory_MemTotal_bytes" => LeafKind::MemTotal,
            "node_filesystem_avail_bytes" | "node_filesystem_free_bytes" => LeafKind::FsAvail,
            "node_filesystem_size_bytes" => LeafKind::FsSize,
            "node_load1" => LeafKind::Load(1),
            "node_load5" => LeafKind::Load(5),
            "node_load15" => LeafKind::Load(15),
            "up" => LeafKind::Up,
            "probe_success" => LeafKind::ProbeSuccess,
            "node_systemd_unit_state" => LeafKind::SystemdUnit,
            other => probe_kind(other)?,
        };
        return make(kind, matchers);
    }
    if let Expr::Call { func, args } = expr {
        if func == "time" && args.is_empty() {
            return make(LeafKind::Time, &[]);
        }
        if let Some(leaf) = gauge_over_range(func, args) {
            return Some(leaf);
        }
        // A probe's value over a short range: `avg_over_time(...[1m])`.
        if matches!(
            func.as_str(),
            "avg_over_time" | "max_over_time" | "min_over_time" | "last_over_time"
        ) {
            if let [arg @ Expr::Selector { range: Some(_), .. }] = args.as_slice() {
                let (metric, matchers) = selector(arg)?;
                return make(probe_kind(metric)?, matchers);
            }
        }
    }
    if let Some((metric, matchers)) = rate_of(expr) {
        let kind = match metric {
            "node_disk_read_time_seconds_total" => LeafKind::DiskReadTime,
            "node_disk_reads_completed_total" => LeafKind::DiskReads,
            "node_disk_write_time_seconds_total" => LeafKind::DiskWriteTime,
            "node_disk_writes_completed_total" => LeafKind::DiskWrites,
            "node_disk_io_time_weighted_seconds_total" => LeafKind::DiskIoTimeWeighted,
            "node_disk_io_time_seconds_total" => LeafKind::DiskIoTime,
            _ => return None,
        };
        return make(kind, matchers);
    }
    let Expr::Agg {
        op,
        without,
        labels,
        param: None,
        expr: inner,
    } = expr
    else {
        return None;
    };
    let cpu = |m: &str| m == "node_cpu_seconds_total";
    match op.as_str() {
        // avg(rate(node_cpu_seconds_total{mode="idle"}[5m]))
        "avg" => {
            let (metric, matchers) = rate_of(inner)?;
            if cpu(metric)
                && has(matchers, "mode", MatchOp::Eq, "idle")
                && per_host(*without, labels, &["cpu", "mode"])
            {
                return make(LeafKind::CpuIdle, matchers);
            }
            None
        }
        // count(node_cpu_seconds_total{mode="idle"}), or the count of
        // processors as kube-prometheus writes it: count without (cpu)
        // (count without (mode) (node_cpu_seconds_total)).
        "count" => {
            if let Some((metric, matchers)) = selector(inner) {
                if cpu(metric)
                    && has(matchers, "mode", MatchOp::Eq, "idle")
                    && per_host(*without, labels, &["cpu", "mode"])
                {
                    return make(LeafKind::CpuCount, matchers);
                }
            }
            if let Expr::Agg {
                op: inner_op,
                without: true,
                labels: inner_labels,
                expr: innermost,
                ..
            } = &**inner
            {
                let (metric, matchers) = selector(innermost)?;
                if inner_op == "count"
                    && cpu(metric)
                    && inner_labels.iter().all(|l| l == "mode")
                    && *without
                    && labels.iter().all(|l| l == "cpu")
                {
                    return make(LeafKind::CpuCount, matchers);
                }
            }
            None
        }
        // sum without (mode) (avg without (cpu) (rate(...{mode!="idle"})))
        "sum" => {
            let Expr::Agg {
                op: inner_op,
                without: inner_without,
                labels: inner_labels,
                param: None,
                expr: innermost,
            } = &**inner
            else {
                return None;
            };
            let (metric, matchers) = rate_of(innermost)?;
            let sums_modes = if *without {
                labels.iter().any(|l| l == "mode")
            } else {
                !labels.iter().any(|l| l == "mode")
            };
            let averages_cpus = inner_op == "avg"
                && per_host(*inner_without, inner_labels, &["cpu"])
                && (*inner_without || !inner_labels.iter().any(|l| l == "cpu"));
            if !cpu(metric) || !sums_modes || !averages_cpus {
                return None;
            }
            if has(matchers, "mode", MatchOp::Ne, "idle") {
                return make(LeafKind::CpuBusy { exact: true }, matchers);
            }
            let leaves_out_idle = matchers.iter().any(|m| {
                m.label == "mode" && m.op == MatchOp::Nre && m.value.split('|').any(|v| v == "idle")
            });
            leaves_out_idle.then(|| make(LeafKind::CpuBusy { exact: false }, matchers))?
        }
        // max/min/avg by (instance) of a series with one value per host.
        "max" | "min" => {
            let inner_leaf = leaf(inner)?;
            matches!(
                inner_leaf.kind,
                LeafKind::MemAvailable
                    | LeafKind::MemTotal
                    | LeafKind::Load(_)
                    | LeafKind::Up
                    | LeafKind::ProbeSuccess
                    | LeafKind::ProbeStatusCode
                    | LeafKind::ProbeDuration { .. }
                    | LeafKind::CertExpiry
            )
            .then_some(inner_leaf)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::LeafKind::*;
    use super::*;

    fn cond(text: &str) -> Condition {
        read(text)
            .unwrap_or_else(|| panic!("{text} should be read"))
            .condition
    }

    #[test]
    fn tokens() {
        let tokens = tokenize(r#"rate(x{a="b\"c"}[5m]) >= 1e3 # note"#).unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Ident("rate".into()),
                Token::Op("("),
                Token::Ident("x".into()),
                Token::Op("{"),
                Token::Ident("a".into()),
                Token::Op("="),
                Token::Str("b\"c".into()),
                Token::Op("}"),
                Token::Range("5m".into()),
                Token::Op(")"),
                Token::Op(">="),
                Token::Number(1000.0),
            ]
        );
        assert_eq!(tokenize(".8").unwrap(), vec![Token::Number(0.8)]);
        assert!(tokenize("x{a=\"b}").is_err());
    }

    #[test]
    fn precedence() {
        // 1 + 2 * 3 is 7, and comparison binds weaker than arithmetic.
        let expr = parse("1 + 2 * 3 > 4").unwrap();
        let Expr::Binary {
            op: BinOp::Gt, lhs, ..
        } = expr
        else {
            panic!("{expr:?}")
        };
        let mut leaves = vec![];
        assert_eq!(poly(&lhs, &mut leaves).unwrap().as_constant(), Some(7.0));
        // `^` is right-associative: 2 ^ (3 ^ 2).
        let Expr::Binary {
            op: BinOp::Pow,
            lhs,
            rhs,
            ..
        } = parse("2 ^ 3 ^ 2").unwrap()
        else {
            panic!("a power");
        };
        assert_eq!(*lhs, Expr::Number(2.0));
        assert!(matches!(*rhs, Expr::Binary { op: BinOp::Pow, .. }));
        // `-` binds weaker than `^`: -(2 ^ 2).
        assert!(matches!(parse("-2 ^ 2").unwrap(), Expr::Neg(_)));
        // Powers are not read as measures.
        assert!(read("node_load1 ^ 2 > 4").is_none());
        let expr = parse("-2 * 3").unwrap();
        assert_eq!(poly(&expr, &mut leaves).unwrap().as_constant(), Some(-6.0));
        assert!(parse("sum(x) by (a").is_err());
        assert!(parse("x >").is_err());
    }

    #[test]
    fn matchers_compare_as_prometheus_does() {
        let m = |op, value: &str| Matcher {
            label: "job".into(),
            op,
            value: value.into(),
        };
        assert!(m(MatchOp::Re, "node.*").matches("node-exporter"));
        assert!(!m(MatchOp::Re, "node").matches("node-exporter"));
        assert!(m(MatchOp::Nre, "node").matches("node-exporter"));
        assert!(m(MatchOp::Ne, "a").matches("b"));
        assert_eq!(
            m(MatchOp::Re, "nginx\\.service|sshd.service").literals(),
            Some(vec!["nginx.service".into(), "sshd.service".into()])
        );
        assert_eq!(m(MatchOp::Re, "ng.*").literals(), None);
    }

    #[test]
    fn processor_usage_forms() {
        for text in [
            r#"100 - (avg by (instance) (rate(node_cpu_seconds_total{mode="idle"}[5m])) * 100) > 80"#,
            r#"100 - (avg by(instance) (irate(node_cpu_seconds_total{mode="idle"}[2m])) * 100) > 80"#,
            r#"(1 - avg without (cpu) (rate(node_cpu_seconds_total{mode="idle"}[5m]))) * 100 > 80"#,
            r#"1 - avg(rate(node_cpu_seconds_total{mode="idle",job="node"}[5m])) by (instance) > 0.8"#,
            r#"1 - (avg without (cpu, mode) (rate(node_cpu_seconds_total{mode="idle"}[5m]))) > .80"#,
        ] {
            let c = cond(text);
            assert!(c.is(&[(CpuIdle, 1)]), "{text}: {c:?}");
            assert_eq!((c.cmp, c.value), (Cmp::Lt, 0.2), "{text}");
        }
        let c = cond(
            r#"sum without(mode) (avg without (cpu) (rate(node_cpu_seconds_total{job="node", mode!="idle"}[2m]))) * 100 > 90"#,
        );
        assert!(c.is(&[(CpuBusy { exact: true }, 1)]));
        assert_eq!((c.cmp, c.value), (Cmp::Gt, 0.9));
        let c = cond(
            r#"sum without(mode) (avg without (cpu) (rate(node_cpu_seconds_total{mode!~"idle|iowait"}[2m]))) * 100 > 90"#,
        );
        assert!(c.is(&[(CpuBusy { exact: false }, 1)]));
    }

    #[test]
    fn processor_usage_near_misses() {
        // An average over processors and modes is no usage.
        assert!(
            read(r#"avg by (instance) (rate(node_cpu_seconds_total{mode!="idle"}[2m])) > .8"#)
                .is_none()
        );
        // A sum over processors grows with their number.
        assert!(
            read(r#"sum by (instance) (rate(node_cpu_seconds_total{mode="idle"}[5m])) < 0.2"#)
                .is_none()
        );
        // The iowait share alone.
        assert!(read(
            r#"avg by (instance) (rate(node_cpu_seconds_total{mode="iowait"}[5m])) * 100 > 10"#
        )
        .is_none());
        // Grouped by processor: one value per processor.
        assert!(
            read(r#"1 - avg by (cpu) (rate(node_cpu_seconds_total{mode="idle"}[5m])) > 0.8"#)
                .is_none()
        );
    }

    #[test]
    fn load_forms() {
        let c = cond("node_load1 > 4");
        assert!(c.is(&[(Load(1), 1)]));
        assert_eq!((c.cmp, c.value), (Cmp::Gt, 4.0));
        let c = cond(r#"max by (instance) (node_load15{job="node"}) >= 2.5"#);
        assert!(c.is(&[(Load(15), 1)]));
        assert_eq!((c.cmp, c.value), (Cmp::Ge, 2.5));
        // Per processor, as kube-prometheus and awesome-prometheus-alerts
        // compare it.
        let c = cond(
            r#"node_load5 / count without (cpu, mode) (node_cpu_seconds_total{mode="idle"}) > 2"#,
        );
        assert!(c.is(&[(Load(5), 1), (CpuCount, -1)]));
        let c = cond(
            r#"node_load1 / on(instance) count without(cpu) (count without(mode) (node_cpu_seconds_total)) > 1"#,
        );
        assert!(c.is(&[(Load(1), 1), (CpuCount, -1)]));
        // A load against another load reads as nothing.
        assert!(read("node_load1 > node_load15 * 2").is_none());
    }

    #[test]
    fn memory_forms() {
        for (text, cmp, value) in [
            (
                "node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes * 100 < 10",
                Cmp::Lt,
                0.1,
            ),
            (
                "(node_memory_MemAvailable_bytes * 100) / node_memory_MemTotal_bytes < 10",
                Cmp::Lt,
                0.1,
            ),
            (
                "(1 - (node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes)) * 100 > 90",
                Cmp::Lt,
                0.1,
            ),
            (
                "100 - (node_memory_MemAvailable_bytes{job=\"node\"} / node_memory_MemTotal_bytes{job=\"node\"} * 100) > 85",
                Cmp::Lt,
                0.15,
            ),
            (
                "(node_memory_MemTotal_bytes - node_memory_MemAvailable_bytes) / node_memory_MemTotal_bytes > 0.95",
                Cmp::Lt,
                0.05,
            ),
            (
                "(node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes < .10) * on(instance) group_left (nodename) node_uname_info{nodename=~\".+\"}",
                Cmp::Lt,
                0.1,
            ),
        ] {
            let c = cond(text);
            assert!(c.is(&[(MemAvailable, 1), (MemTotal, -1)]), "{text}: {c:?}");
            assert_eq!((c.cmp, c.value), (cmp, value), "{text}");
        }
        // Bytes, not a share.
        assert!(read("node_memory_MemAvailable_bytes < 100000000")
            .unwrap()
            .condition
            .is(&[(MemAvailable, 1)]));
        // A sum of two products is no single measure.
        assert!(read(
            "node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes + node_load1 > 1"
        )
        .is_none());
    }

    #[test]
    fn filesystem_forms() {
        let r = read(
            r#"(node_filesystem_avail_bytes{mountpoint="/",fstype!="tmpfs"} * 100) / node_filesystem_size_bytes{fstype!="tmpfs",mountpoint="/"} < 10 and ON (instance, device, mountpoint) node_filesystem_readonly == 0"#,
        )
        .unwrap();
        assert!(r.condition.is(&[(FsAvail, 1), (FsSize, -1)]));
        assert_eq!((r.condition.cmp, r.condition.value), (Cmp::Lt, 0.1));
        assert_eq!(r.exact("mountpoint").as_deref(), Some("/"));
        assert!(!r.extra_condition);
        let r = read(
            r#"node_filesystem_avail_bytes{fstype=~"ext4|xfs"} / node_filesystem_size_bytes{fstype=~"ext4|xfs"} < 0.2 and predict_linear(node_filesystem_avail_bytes[6h], 86400) < 0"#,
        )
        .unwrap();
        assert_eq!(r.condition.value, 0.2);
        assert_eq!(r.exact("mountpoint"), None);
        // The prediction is a condition of its own (ADR-0153).
        assert!(!r.extra_condition);
        assert_eq!(
            r.also[0].window(),
            Some(Window::Predict {
                over: 21_600,
                within: 86_400
            })
        );
        let alone = read("predict_linear(node_filesystem_avail_bytes[6h], 4 * 3600) < 0").unwrap();
        assert!(alone.condition.is(&[(FsAvail, 1)]));
        assert_eq!(
            alone.window(),
            Some(Window::Predict {
                over: 21_600,
                within: 14_400
            })
        );
        // Inodes are another measure.
        assert!(read("node_filesystem_files_free / node_filesystem_files * 100 < 10").is_none());
    }

    #[test]
    fn disk_wait_forms() {
        let r = read(
            r#"rate(node_disk_read_time_seconds_total{device="sda"}[1m]) / rate(node_disk_reads_completed_total{device="sda"}[1m]) > 0.1 and rate(node_disk_reads_completed_total[1m]) > 0"#,
        )
        .unwrap();
        assert!(r.condition.is(&[(DiskReadTime, 1), (DiskReads, -1)]));
        assert_eq!(r.condition.value, 0.1);
        assert_eq!(r.exact("device").as_deref(), Some("sda"));
        assert!(!r.extra_condition);
        let c = cond(
            "rate(node_disk_io_time_weighted_seconds_total[5m]) / rate(node_disk_io_time_seconds_total[5m]) > 0.5",
        );
        assert!(c.is(&[(DiskIoTimeWeighted, 1), (DiskIoTime, -1)]));
        // The queue length alone is not a wait.
        assert!(
            cond("rate(node_disk_io_time_weighted_seconds_total[5m]) > 10")
                .is(&[(DiskIoTimeWeighted, 1)])
        );
    }

    #[test]
    fn up_and_probes() {
        for text in [
            "up == 0",
            "up{job=\"node\"} < 1",
            "up != 1",
            "max by (instance) (up) == 0",
        ] {
            let c = cond(text);
            assert!(c.is(&[(Up, 1)]) && c.is_false_check(), "{text}");
        }
        let r = read(r#"probe_success{job="blackbox"} == 0"#).unwrap();
        assert!(r.condition.is(&[(ProbeSuccess, 1)]) && r.condition.is_false_check());
        assert_eq!(r.exact("job").as_deref(), Some("blackbox"));
        assert!(!cond("up == 1").is_false_check());
        assert!(!cond("up > 0").is_false_check());
        // An absent target is not a down one.
        assert!(read("absent(up{job=\"node\"})").is_none());
        // `bool` gives 0 or 1 for every series: no condition.
        assert!(read("up == bool 0").is_none());
    }

    #[test]
    fn systemd_units() {
        let r =
            read(r#"node_systemd_unit_state{state="active",name="nginx.service"} == 0"#).unwrap();
        assert!(r.condition.is(&[(SystemdUnit, 1)]) && r.condition.is_false_check());
        assert_eq!(r.exact("name").as_deref(), Some("nginx.service"));
        let r = read(r#"node_systemd_unit_state{state="failed"} == 1"#).unwrap();
        assert!(r.condition.is_true_check());
    }

    #[test]
    fn website_probes() {
        let c = cond("probe_ssl_earliest_cert_expiry - time() < 86400 * 7");
        assert!(c.is(&[(CertSecondsLeft, 1)]));
        assert_eq!((c.cmp, c.value), (Cmp::Lt, 604_800.0));
        let c = cond("(probe_ssl_earliest_cert_expiry - time()) / 86400 < 30");
        assert!(c.is(&[(CertSecondsLeft, 1)]));
        assert_eq!(c.value, 2_592_000.0);
        let c = cond("probe_ssl_earliest_cert_expiry < time() + 86400");
        assert!(c.is(&[(CertSecondsLeft, 1)]) && c.cmp == Cmp::Lt);
        let r = read(
            "3 <= round((last_over_time(probe_ssl_earliest_cert_expiry[10m]) - time()) / 86400, 0.1) < 20",
        )
        .unwrap();
        assert!(r.condition.is(&[(CertSecondsLeft, 1)]) && r.extra_condition);
        assert_eq!(r.condition.value, 1_728_000.0);
        let c = cond("avg_over_time(probe_duration_seconds[1m]) > 1");
        assert!(c.is(&[(ProbeDuration { whole: true }, 1)]));
        let c = cond(r#"max by (instance) (probe_http_duration_seconds{phase="processing"}) > 1"#);
        assert!(c.is(&[(ProbeDuration { whole: false }, 1)]));
        let parts =
            read_alternatives("probe_http_status_code <= 199 OR probe_http_status_code >= 400")
                .unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts
            .iter()
            .all(|p| p.condition.is(&[(ProbeStatusCode, 1)])));
        assert!(read_alternatives("probe_http_status_code > 399 or nothing_known > 1").is_none());
    }

    #[test]
    fn unknown_expressions() {
        for text in [
            "probe_ssl_earliest_cert_expiry - 2 * time() < 0",
            "probe_http_status_code <= 199 OR probe_http_status_code >= 400",
            "rate(node_network_receive_bytes_total[5m]) > 1e8",
            "increase(node_vmstat_oom_kill[1m]) > 0",
            "node_load1 offset 1h > 4",
            "max_over_time(node_load1[5m:1m]) > 4",
            "kube_pod_container_status_restarts_total > 3",
            "node_textfile_scrape_error != 0",
            "1 > 0",
            "node_load1",
        ] {
            assert!(read(text).is_none(), "{text}");
        }
    }

    #[test]
    fn functions_over_ranges_of_gauges() {
        let r = read("avg_over_time(node_load5[15m]) > 4").unwrap();
        assert!(r.condition.is(&[(Load(5), 1)]));
        assert_eq!(r.window(), Some(Window::Avg(900)));
        let r = read(
            "min_over_time(node_memory_MemAvailable_bytes[10m]) / node_memory_MemTotal_bytes < 0.1",
        )
        .unwrap();
        assert_eq!(r.window(), Some(Window::Min(600)));
        assert_eq!(
            read("delta(node_load1[5m]) > 2").unwrap().window(),
            Some(Window::Change(300))
        );
        assert_eq!(
            read("deriv(node_load1[5m]) > 0.1").unwrap().window(),
            Some(Window::Slope(300))
        );
        // Counters are not gauges.
        assert!(read("avg_over_time(node_cpu_seconds_total[5m]) > 1").is_none());
        // Conditions joined with and.
        let r = read("node_load1 > 8 and avg_over_time(node_load15[1h]) > 4").unwrap();
        assert_eq!(r.also.len(), 1);
        assert!(r.also[0].condition.is(&[(Load(15), 1)]));
    }

    #[test]
    fn comparisons_with_the_past() {
        let (r, ratio, offset) = read_compare(
            "avg_over_time(node_load1[1h]) / avg_over_time(node_load1[1h] offset 1w) > 2",
        )
        .unwrap();
        assert!(ratio && offset == 604_800 && r.condition.is(&[(Load(1), 1)]));
        assert_eq!(
            (r.condition.value, r.window()),
            (2.0, Some(Window::Avg(3_600)))
        );
        let (_, ratio, offset) = read_compare("node_load5 - node_load5 offset 1d > 3").unwrap();
        assert!(!ratio && offset == 86_400);
        // Different sides, or no offset: not a comparison with the past.
        assert!(read_compare("node_load1 / node_load5 offset 1d > 2").is_none());
        assert!(read_compare("node_load1 / node_load1 > 2").is_none());
        // The offset alone still cannot be read.
        assert!(read("node_load1 offset 1h > 4").is_none());
    }
}
