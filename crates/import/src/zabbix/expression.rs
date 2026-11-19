//! Trigger expressions, parsed into a tree the mapper reads.
//!
//! Zabbix 5.4 and later write `min(/host/key,5m)>{$MAX}`; older versions
//! wrote `{host:key.min(5m)}>{$MAX}`, which [`parse`] rewrites into the
//! new form first. Operators, by increasing precedence: `or`, `and`,
//! `not`, `= <>`, `< <= > >=`, `+ -`, `* /`, unary minus.

use std::fmt;

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A number as written, with its suffix: `90`, `1.5`, `20M`, `5m`.
    Number(String),
    /// A quoted string, unescaped.
    Text(String),
    /// A macro: `{$CPU.UTIL.CRIT}`, `{$X:"ctx"}`, `{#FSNAME}`,
    /// `{TRIGGER.VALUE}`.
    Macro(String),
    /// A function: `min(/host/key,5m)`, `last(...)`.
    Call(Call),
    Not(Box<Expr>),
    Negative(Box<Expr>),
    Binary(Op, Box<Expr>, Box<Expr>),
}

/// A function call.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    /// The item it reads, when its first argument is `/host/key`.
    pub item: Option<ItemRef>,
    /// The other arguments as written (strings unquoted).
    pub args: Vec<String>,
}

/// `/host/key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRef {
    pub host: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn text(self) -> &'static str {
        match self {
            Self::Or => " or ",
            Self::And => " and ",
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
        }
    }

    /// The comparison seen from the other side: `a < b` is `b > a`.
    pub fn flipped(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            other => other,
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }
}

impl fmt::Display for Expr {
    /// The expression written back, the way Zabbix shows it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => f.write_str(n),
            Self::Text(t) => write!(f, "\"{}\"", t.replace('"', "\\\"")),
            Self::Macro(m) => f.write_str(m),
            Self::Call(call) => {
                write!(f, "{}(", call.name)?;
                let mut first = true;
                if let Some(item) = &call.item {
                    write!(f, "/{}/{}", item.host, item.key)?;
                    first = false;
                }
                for arg in &call.args {
                    if !first {
                        f.write_str(",")?;
                    }
                    first = false;
                    f.write_str(arg)?;
                }
                f.write_str(")")
            }
            Self::Not(e) => write!(f, "not {e}"),
            Self::Negative(e) => write!(f, "-{e}"),
            Self::Binary(op, a, b) => {
                let wrap = |e: &Expr, f: &mut fmt::Formatter<'_>| match e {
                    Expr::Binary(inner, ..) if precedence(*inner) < precedence(*op) => {
                        write!(f, "({e})")
                    }
                    _ => write!(f, "{e}"),
                };
                wrap(a, f)?;
                f.write_str(op.text())?;
                wrap(b, f)
            }
        }
    }
}

fn precedence(op: Op) -> u8 {
    match op {
        Op::Or => 1,
        Op::And => 2,
        Op::Eq | Op::Ne => 4,
        Op::Lt | Op::Le | Op::Gt | Op::Ge => 5,
        Op::Add | Op::Sub => 6,
        Op::Mul | Op::Div => 7,
    }
}

/// Most tokens in an expression. Zabbix keeps at most 2048 characters;
/// the cap bounds the depth of the syntax tree, which is walked
/// recursively, so a hostile export cannot exhaust the stack.
const MAX_TOKENS: usize = 2048;
/// Deepest nesting of parentheses, `not` and minus signs.
const MAX_NESTING: usize = 128;

/// Parses an expression of either syntax. `Err` says what was not
/// understood.
pub fn parse(text: &str) -> Result<Expr, String> {
    parse_new(&rewrite_old(text))
}

fn parse_new(text: &str) -> Result<Expr, String> {
    let tokens = tokenize(text)?;
    if tokens.len() > MAX_TOKENS {
        return Err("the expression is too long".into());
    }
    let mut parser = Parser {
        tokens,
        at: 0,
        depth: 0,
    };
    let expr = parser.or()?;
    if parser.at != parser.tokens.len() {
        return Err(format!("unexpected {:?}", parser.tokens[parser.at]));
    }
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(String),
    Text(String),
    Macro(String),
    Call(Call),
    Op(Op),
    Not,
    Minus,
    Open,
    Close,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Op(Op::Add));
                i += 1;
            }
            '*' => {
                tokens.push(Token::Op(Op::Mul));
                i += 1;
            }
            '/' => {
                tokens.push(Token::Op(Op::Div));
                i += 1;
            }
            '-' => {
                // Binary after an operand, unary otherwise.
                let operand_before = matches!(
                    tokens.last(),
                    Some(
                        Token::Number(_)
                            | Token::Text(_)
                            | Token::Macro(_)
                            | Token::Call(_)
                            | Token::Close
                    )
                );
                tokens.push(if operand_before {
                    Token::Op(Op::Sub)
                } else {
                    Token::Minus
                });
                i += 1;
            }
            '=' => {
                tokens.push(Token::Op(Op::Eq));
                i += 1;
            }
            '<' => match chars.get(i + 1) {
                Some('>') => {
                    tokens.push(Token::Op(Op::Ne));
                    i += 2;
                }
                Some('=') => {
                    tokens.push(Token::Op(Op::Le));
                    i += 2;
                }
                _ => {
                    tokens.push(Token::Op(Op::Lt));
                    i += 1;
                }
            },
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    tokens.push(Token::Op(Op::Ge));
                    i += 2;
                } else {
                    tokens.push(Token::Op(Op::Gt));
                    i += 1;
                }
            }
            '"' => {
                let (text, next) = quoted(&chars, i)?;
                tokens.push(Token::Text(text));
                i = next;
            }
            '{' => {
                let end = closing_brace(&chars, i)?;
                tokens.push(Token::Macro(chars[i..=end].iter().collect()));
                i = end + 1;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                if i < chars.len() && "KMGTsmhdw".contains(chars[i]) {
                    i += 1;
                }
                tokens.push(Token::Number(chars[start..i].iter().collect()));
            }
            c if c.is_ascii_alphabetic() => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                match word.as_str() {
                    "and" => tokens.push(Token::Op(Op::And)),
                    "or" => tokens.push(Token::Op(Op::Or)),
                    "not" => tokens.push(Token::Not),
                    _ => {
                        let mut j = i;
                        while j < chars.len() && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if chars.get(j) != Some(&'(') {
                            return Err(format!("unknown word {word}"));
                        }
                        let (call, next) = call(word, &chars, j)?;
                        tokens.push(Token::Call(call));
                        i = next;
                    }
                }
            }
            other => return Err(format!("unexpected character {other}")),
        }
    }
    Ok(tokens)
}

/// A quoted string starting at `start`, and the index after it.
fn quoted(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '"' => return Ok((out, i + 1)),
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Err("unterminated string".into())
}

/// The index of the `}` closing the `{` at `start`, quotes and nested
/// macros skipped.
fn closing_brace(chars: &[char], start: usize) -> Result<usize, String> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i = quoted(chars, i)?.1;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err("unterminated macro".into())
}

/// A call whose `(` is at `open`: its arguments, the first one possibly
/// an item `/host/key`.
fn call(name: String, chars: &[char], open: usize) -> Result<(Call, usize), String> {
    let mut args = Vec::new();
    let mut item = None;
    let mut i = open + 1;
    let mut first = true;
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            return Err("unterminated function".into());
        }
        if chars[i] == ')' && args.is_empty() && item.is_none() && first {
            return Ok((Call { name, item, args }, i + 1));
        }
        // One argument, up to `,` or `)` outside brackets and quotes.
        let start = i;
        let mut brackets = 0usize;
        let mut parens = 0usize;
        let mut text = String::new();
        let mut was_quoted = false;
        while i < chars.len() {
            match chars[i] {
                '"' => {
                    let (inner, next) = quoted(chars, i)?;
                    if brackets == 0 && parens == 0 && text.trim().is_empty() {
                        was_quoted = true;
                        text = inner;
                    } else {
                        text.extend(&chars[i..next]);
                    }
                    i = next;
                    continue;
                }
                '{' => {
                    let end = closing_brace(chars, i)?;
                    text.extend(&chars[i..=end]);
                    i = end + 1;
                    continue;
                }
                '[' => brackets += 1,
                ']' => brackets = brackets.saturating_sub(1),
                '(' => parens += 1,
                ')' if parens > 0 => parens -= 1,
                ',' | ')' if brackets == 0 && parens == 0 => break,
                _ => {}
            }
            text.push(chars[i]);
            i += 1;
        }
        if i >= chars.len() {
            return Err("unterminated function".into());
        }
        let raw: String = chars[start..i].iter().collect();
        if first && raw.trim_start().starts_with('/') {
            item = Some(item_ref(raw.trim())?);
        } else if was_quoted {
            args.push(text);
        } else {
            args.push(text.trim().to_owned());
        }
        first = false;
        let end = chars[i];
        i += 1;
        if end == ')' {
            return Ok((Call { name, item, args }, i));
        }
    }
}

/// `/host/key`, `//key` (the host itself) or `/host/key?[filter]`.
fn item_ref(text: &str) -> Result<ItemRef, String> {
    let rest = text.strip_prefix('/').ok_or("item without /")?;
    let (host, key) = rest.split_once('/').ok_or("item without host")?;
    let key = match key.find("?[") {
        Some(at) if !key[..at].contains('[') || key[..at].ends_with(']') => &key[..at],
        _ => key,
    };
    if key.is_empty() {
        return Err("item without key".into());
    }
    Ok(ItemRef {
        host: host.to_owned(),
        key: key.to_owned(),
    })
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    /// How deeply [`Parser::not`] and [`Parser::unary`] are nested.
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn binary(
        &mut self,
        ops: &[Op],
        next: fn(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        let mut left = next(self)?;
        while let Some(Token::Op(op)) = self.peek() {
            let op = *op;
            if !ops.contains(&op) {
                break;
            }
            self.at += 1;
            let right = next(self)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn or(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::Or], Self::and)
    }

    fn and(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::And], Self::not)
    }

    /// Runs `parse` one level deeper; refuses beyond [`MAX_NESTING`].
    fn nested(&mut self, parse: fn(&mut Self) -> Result<Expr, String>) -> Result<Expr, String> {
        if self.depth >= MAX_NESTING {
            return Err("the expression is nested too deeply".into());
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn not(&mut self) -> Result<Expr, String> {
        self.nested(Self::negation)
    }

    fn negation(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Token::Not) {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::Eq, Op::Ne], Self::relation)
    }

    fn relation(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::Lt, Op::Le, Op::Gt, Op::Ge], Self::sum)
    }

    fn sum(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::Add, Op::Sub], Self::product)
    }

    fn product(&mut self) -> Result<Expr, String> {
        self.binary(&[Op::Mul, Op::Div], Self::unary)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        self.nested(Self::operand)
    }

    fn operand(&mut self) -> Result<Expr, String> {
        let token = self.peek().cloned().ok_or("expression ends too early")?;
        self.at += 1;
        match token {
            Token::Minus => Ok(Expr::Negative(Box::new(self.unary()?))),
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::Text(t) => Ok(Expr::Text(t)),
            Token::Macro(m) => Ok(Expr::Macro(m)),
            Token::Call(c) => Ok(Expr::Call(c)),
            Token::Open => {
                let inner = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err("missing )".into());
                }
                self.at += 1;
                Ok(inner)
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

/// Rewrites the functions of the old syntax, `{host:key.func(params)}`,
/// into `func(/host/key,params)`; user and LLD macros stay as they are.
fn rewrite_old(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            match quoted(&chars, i) {
                Ok((_, next)) => {
                    out.extend(&chars[i..next]);
                    i = next;
                    continue;
                }
                Err(_) => {
                    out.extend(&chars[i..]);
                    break;
                }
            }
        }
        if chars[i] != '{' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let Ok(end) = closing_brace(&chars, i) else {
            out.extend(&chars[i..]);
            break;
        };
        let inner: String = chars[i + 1..end].iter().collect();
        match old_function(&inner) {
            Some(new) => out.push_str(&new),
            None => out.extend(&chars[i..=end]),
        }
        i = end + 1;
    }
    out
}

/// `host:key.func(params)` as `func(/host/key,params)`.
fn old_function(inner: &str) -> Option<String> {
    if inner.starts_with('$') || inner.starts_with('#') || !inner.ends_with(')') {
        return None;
    }
    let (host, rest) = inner.split_once(':')?;
    if host.is_empty() || host.contains(['[', '"']) {
        return None;
    }
    // The `(` matching the final `)`.
    let chars: Vec<char> = rest.chars().collect();
    let mut depth = 0;
    let mut open = None;
    for (at, c) in chars.iter().enumerate().rev() {
        match c {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open?;
    let head: String = chars[..open].iter().collect();
    let params: String = chars[open + 1..chars.len() - 1].iter().collect();
    let (key, function) = head.rsplit_once('.')?;
    if function.is_empty() || !function.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let params = params.trim();
    // Old `last(0)` and `last()` read the latest value.
    let params = if function == "last" && (params == "0" || params == "#1") {
        ""
    } else {
        params
    };
    Some(if params.is_empty() {
        format!("{function}(/{host}/{key})")
    } else {
        format!("{function}(/{host}/{key},{params})")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, host: &str, key: &str, args: &[&str]) -> Expr {
        Expr::Call(Call {
            name: name.into(),
            item: Some(ItemRef {
                host: host.into(),
                key: key.into(),
            }),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        })
    }

    fn bin(op: Op, a: Expr, b: Expr) -> Expr {
        Expr::Binary(op, Box::new(a), Box::new(b))
    }

    #[test]
    fn a_comparison_with_a_macro() {
        assert_eq!(
            parse("min(/Linux by Zabbix agent/system.cpu.util,5m)>{$CPU.UTIL.CRIT}").unwrap(),
            bin(
                Op::Gt,
                call("min", "Linux by Zabbix agent", "system.cpu.util", &["5m"]),
                Expr::Macro("{$CPU.UTIL.CRIT}".into())
            )
        );
    }

    #[test]
    fn keys_with_quotes_brackets_and_commas() {
        let expr = parse(
            r#"last(/Nginx/net.tcp.service[http,"{$NGINX.STUB_STATUS.HOST}","{$NGINX.STUB_STATUS.PORT}"])=0"#,
        )
        .unwrap();
        assert_eq!(
            expr,
            bin(
                Op::Eq,
                call(
                    "last",
                    "Nginx",
                    r#"net.tcp.service[http,"{$NGINX.STUB_STATUS.HOST}","{$NGINX.STUB_STATUS.PORT}"]"#,
                    &[]
                ),
                Expr::Number("0".into())
            )
        );
        let expr = parse(r#"find(/db-01/log[/var/log/syslog,error],,"regexp","error")=1"#).unwrap();
        assert_eq!(
            expr,
            bin(
                Op::Eq,
                call(
                    "find",
                    "db-01",
                    "log[/var/log/syslog,error]",
                    &["", "regexp", "error"]
                ),
                Expr::Number("1".into())
            )
        );
    }

    #[test]
    fn precedence_and_context_macros() {
        let expr = parse(
            "min(/L/system.cpu.load[all,avg1],5m)/last(/L/system.cpu.num)>{$LOAD:\"x\"}\nand last(/L/system.cpu.load[all,avg5])>0",
        )
        .unwrap();
        let Expr::Binary(Op::And, left, right) = expr else {
            panic!("and expected")
        };
        assert_eq!(
            *left,
            bin(
                Op::Gt,
                bin(
                    Op::Div,
                    call("min", "L", "system.cpu.load[all,avg1]", &["5m"]),
                    call("last", "L", "system.cpu.num", &[])
                ),
                Expr::Macro("{$LOAD:\"x\"}".into())
            )
        );
        assert_eq!(
            *right,
            bin(
                Op::Gt,
                call("last", "L", "system.cpu.load[all,avg5]", &[]),
                Expr::Number("0".into())
            )
        );
        let expr = parse(r#"{$IFCONTROL:"{#IFNAME}"}=1 and not nodata(/h/agent.ping,5m)"#).unwrap();
        assert_eq!(
            expr,
            bin(
                Op::And,
                bin(
                    Op::Eq,
                    Expr::Macro(r#"{$IFCONTROL:"{#IFNAME}"}"#.into()),
                    Expr::Number("1".into())
                ),
                Expr::Not(Box::new(call("nodata", "h", "agent.ping", &["5m"])))
            )
        );
    }

    #[test]
    fn strings_numbers_and_negatives() {
        assert_eq!(
            parse(r#"last(/db-01/systemd.unit.info[postgresql.service])<>"active""#).unwrap(),
            bin(
                Op::Ne,
                call(
                    "last",
                    "db-01",
                    "systemd.unit.info[postgresql.service]",
                    &[]
                ),
                Expr::Text("active".into())
            )
        );
        assert_eq!(
            parse("last(/h/k)>-1.5 or last(/h/k)<20M").unwrap(),
            bin(
                Op::Or,
                bin(
                    Op::Gt,
                    call("last", "h", "k", &[]),
                    Expr::Negative(Box::new(Expr::Number("1.5".into())))
                ),
                bin(
                    Op::Lt,
                    call("last", "h", "k", &[]),
                    Expr::Number("20M".into())
                )
            )
        );
    }

    #[test]
    fn the_old_syntax_is_rewritten() {
        assert_eq!(
            parse("{Template OS Linux:system.cpu.util[,idle].avg(5m)}<{$CPU.IDLE}").unwrap(),
            bin(
                Op::Lt,
                call(
                    "avg",
                    "Template OS Linux",
                    "system.cpu.util[,idle]",
                    &["5m"]
                ),
                Expr::Macro("{$CPU.IDLE}".into())
            )
        );
        assert_eq!(
            parse("{web-01:vfs.fs.size[/,pused].last(0)}>90").unwrap(),
            bin(
                Op::Gt,
                call("last", "web-01", "vfs.fs.size[/,pused]", &[]),
                Expr::Number("90".into())
            )
        );
    }

    #[test]
    fn expressions_are_written_back() {
        let text = "min(/h/system.cpu.util,5m)>90 and (last(/h/a)=0 or last(/h/b)=1)";
        assert_eq!(parse(text).unwrap().to_string(), text);
    }

    #[test]
    fn broken_expressions_are_refused() {
        assert!(parse("last(/h/k").is_err());
        assert!(parse("last(/h/k)>").is_err());
        assert!(parse("foo > 1").is_err());
        assert!(parse("(1").is_err());
        assert!(parse("\"x").is_err());
    }

    /// A hostile export cannot exhaust the stack: nesting and length are
    /// bounded, and what fits within the bounds is parsed and dropped on
    /// a stack as small as a runtime worker's.
    #[test]
    fn hostile_expressions_are_refused_without_exhausting_the_stack() {
        let deep = format!("{}1{}", "(".repeat(100_000), ")".repeat(100_000));
        assert!(parse(&deep).is_err());
        assert!(parse(&format!("{}1", "-".repeat(100_000))).is_err());
        assert!(parse(&format!("{}1", "not ".repeat(100_000))).is_err());
        assert!(parse(&format!("1{}", "+1".repeat(100_000))).is_err());

        let within = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let nested = format!("{}last(/h/k){}>1", "(".repeat(60), ")".repeat(60));
                assert!(parse(&nested).is_ok());
                let chain = format!("last(/h/k){}>1", "+1".repeat(1000));
                let expr = parse(&chain).unwrap();
                assert!(!expr.to_string().is_empty());
            })
            .unwrap()
            .join();
        assert!(within.is_ok());
    }
}
