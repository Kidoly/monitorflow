//! User macros (`{$NAME}`, `{$NAME:"context"}`) resolved as Zabbix does
//! for one host: the host's own, then its templates' in link order, then
//! global ones. A macro with a context is looked for with that context on
//! every level first, then with a regular expression context matching it,
//! then without context. Host macros (`{HOST.CONN}` and the like) come
//! from the host's main interface.

use regex::Regex;

use super::config::Macro;

/// What `{HOST.*}` macros give.
#[derive(Debug, Clone, Default)]
pub struct HostValues {
    /// `{HOST.HOST}`.
    pub host: String,
    /// `{HOST.NAME}`.
    pub name: String,
    pub ip: String,
    pub dns: String,
    /// `{HOST.CONN}`: the IP or the DNS name Zabbix connects to.
    pub conn: String,
}

/// The macros one host sees.
#[derive(Debug, Clone, Default)]
pub struct Macros {
    /// Host, templates, global: the first level that has a macro wins.
    levels: Vec<Vec<Macro>>,
    host: HostValues,
}

/// `{$NAME:context}` split, the context unquoted.
fn split(name: &str) -> Option<(&str, Option<Context>)> {
    let inner = name.strip_prefix("{$")?.strip_suffix('}')?;
    match inner.split_once(':') {
        None => Some((inner, None)),
        Some((base, context)) => {
            let context = context.trim();
            if let Some(pattern) = context.strip_prefix("regex:") {
                return Some((base, Some(Context::Regex(unquote(pattern.trim())))));
            }
            Some((base, Some(Context::Static(unquote(context)))))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Context {
    Static(String),
    Regex(String),
}

fn unquote(text: &str) -> String {
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\""),
        None => text.to_owned(),
    }
}

/// How a lookup ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(String),
    /// Defined, but its value is secret.
    Hidden,
    Missing,
}

impl Macros {
    pub fn new(levels: Vec<Vec<Macro>>, host: HostValues) -> Self {
        Self { levels, host }
    }

    /// The value of a user macro as written in an expression or a key.
    pub fn get(&self, name: &str) -> Lookup {
        let Some((base, context)) = split(name) else {
            return Lookup::Missing;
        };
        let found = |m: &Macro| match &m.value {
            Some(v) => Lookup::Found(v.clone()),
            None => Lookup::Hidden,
        };
        if let Some(Context::Static(wanted)) = &context {
            // Exact context, on every level.
            for level in &self.levels {
                for m in level {
                    if let Some((b, Some(Context::Static(c)))) = split(&m.name) {
                        if b == base && c == *wanted {
                            return found(m);
                        }
                    }
                }
            }
            // A regular expression context matching it.
            for level in &self.levels {
                for m in level {
                    if let Some((b, Some(Context::Regex(pattern)))) = split(&m.name) {
                        if b == base && Regex::new(&pattern).is_ok_and(|r| r.is_match(wanted)) {
                            return found(m);
                        }
                    }
                }
            }
        }
        for level in &self.levels {
            for m in level {
                if let Some((b, None)) = split(&m.name) {
                    if b == base {
                        return found(m);
                    }
                }
            }
        }
        Lookup::Missing
    }

    /// Macros of this name with a fixed context, on any level: the
    /// overrides a host makes for some file systems or disks, as
    /// `(context, value)`, the first level winning.
    pub fn contexts(&self, base_name: &str) -> Vec<(String, String)> {
        let Some((base, _)) = split(base_name) else {
            return vec![];
        };
        let mut out: Vec<(String, String)> = Vec::new();
        for level in &self.levels {
            for m in level {
                if let (Some((b, Some(Context::Static(c)))), Some(value)) =
                    (split(&m.name), &m.value)
                {
                    if b == base && !c.contains("{#") && !out.iter().any(|(o, _)| *o == c) {
                        out.push((c, value.clone()));
                    }
                }
            }
        }
        out
    }

    /// `text` with its user and host macros replaced; LLD macros
    /// (`{#FSNAME}`) and unknown built-in ones stay. `Err` names the first
    /// user macro without a value.
    pub fn expand(&self, text: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find('{') {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            let Some(end) = closing(tail) else {
                out.push_str(tail);
                return Ok(out);
            };
            let name = &tail[..=end];
            if name.starts_with("{$") {
                match self.get(name) {
                    Lookup::Found(value) => out.push_str(&value),
                    _ => return Err(name.to_owned()),
                }
            } else {
                let host = &self.host;
                let value = match name {
                    "{HOST.HOST}" | "{HOSTNAME}" => Some(&host.host),
                    "{HOST.NAME}" => Some(&host.name),
                    "{HOST.IP}" | "{IPADDRESS}" => Some(&host.ip),
                    "{HOST.DNS}" => Some(&host.dns),
                    "{HOST.CONN}" => Some(&host.conn),
                    _ => None,
                };
                out.push_str(value.map_or(name, String::as_str));
            }
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// The index of the `}` closing the macro at the start of `text`.
fn closing(text: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if quoted {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => quoted = false,
                _ => escaped = false,
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str, value: &str) -> Macro {
        Macro {
            name: name.into(),
            value: Some(value.into()),
        }
    }

    fn macros() -> Macros {
        Macros::new(
            vec![
                // Host.
                vec![
                    m("{$CPU.UTIL.CRIT}", "80"),
                    m("{$VFS.FS.PUSED.MAX.CRIT:\"/var/lib/postgresql\"}", "95"),
                    Macro {
                        name: "{$PASSWORD}".into(),
                        value: None,
                    },
                ],
                // Template.
                vec![
                    m("{$CPU.UTIL.CRIT}", "90"),
                    m("{$VFS.FS.PUSED.MAX.CRIT}", "90"),
                    m("{$VFS.FS.PUSED.MAX.CRIT:regex:\"^/srv\"}", "97"),
                    m("{$NGINX.PORT}", "80"),
                ],
                // Global.
                vec![m("{$DB.CPU.HIGH}", "85"), m("{$CPU.UTIL.CRIT}", "70")],
            ],
            HostValues {
                host: "web-01".into(),
                name: "Web server 01".into(),
                ip: "192.0.2.10".into(),
                dns: "web-01.example.com".into(),
                conn: "192.0.2.10".into(),
            },
        )
    }

    #[test]
    fn the_nearest_level_wins() {
        let macros = macros();
        assert_eq!(macros.get("{$CPU.UTIL.CRIT}"), Lookup::Found("80".into()));
        assert_eq!(macros.get("{$NGINX.PORT}"), Lookup::Found("80".into()));
        assert_eq!(macros.get("{$DB.CPU.HIGH}"), Lookup::Found("85".into()));
        assert_eq!(macros.get("{$NONE}"), Lookup::Missing);
        assert_eq!(macros.get("{$PASSWORD}"), Lookup::Hidden);
    }

    #[test]
    fn contexts_then_regular_expressions_then_the_default() {
        let macros = macros();
        assert_eq!(
            macros.get("{$VFS.FS.PUSED.MAX.CRIT:\"/var/lib/postgresql\"}"),
            Lookup::Found("95".into())
        );
        assert_eq!(
            macros.get("{$VFS.FS.PUSED.MAX.CRIT:/var/lib/postgresql}"),
            Lookup::Found("95".into())
        );
        assert_eq!(
            macros.get("{$VFS.FS.PUSED.MAX.CRIT:\"/srv/data\"}"),
            Lookup::Found("97".into())
        );
        assert_eq!(
            macros.get("{$VFS.FS.PUSED.MAX.CRIT:\"/\"}"),
            Lookup::Found("90".into())
        );
        assert_eq!(
            macros.get("{$VFS.FS.PUSED.MAX.CRIT:\"{#FSNAME}\"}"),
            Lookup::Found("90".into())
        );
        assert_eq!(
            macros.contexts("{$VFS.FS.PUSED.MAX.CRIT:\"{#FSNAME}\"}"),
            vec![("/var/lib/postgresql".to_owned(), "95".to_owned())]
        );
    }

    #[test]
    fn text_is_expanded() {
        let macros = macros();
        assert_eq!(
            macros
                .expand("net.tcp.service[http,\"{HOST.CONN}\",\"{$NGINX.PORT}\"]")
                .unwrap(),
            "net.tcp.service[http,\"192.0.2.10\",\"80\"]"
        );
        assert_eq!(
            macros.expand("vfs.fs.size[{#FSNAME},pused]").unwrap(),
            "vfs.fs.size[{#FSNAME},pused]"
        );
        assert_eq!(
            macros.expand("{HOST.NAME} {EVENT.ID}").unwrap(),
            "Web server 01 {EVENT.ID}"
        );
        assert_eq!(
            macros.expand("x[{$MISSING}]").unwrap_err(),
            "{$MISSING}".to_owned()
        );
        assert_eq!(
            macros.expand("x[{$PASSWORD}]").unwrap_err(),
            "{$PASSWORD}".to_owned()
        );
    }
}
