//! Configuration exports (`zabbix_export`) in YAML, XML or JSON, as Zabbix
//! 5.0 to 7.4 write them. Several files are merged: a hosts export, a
//! templates export and a media types export make one configuration.
//!
//! Exports leave out values equal to the default (an item's type, a
//! trigger's priority, a delay of one minute), so absent fields read as
//! Zabbix's defaults.

use serde_json::Value;

use super::config::{
    Config, Host, HttpAgent, Interface, InterfaceKind, Item, ItemKind, Macro, MediaKind, MediaType,
    Priority, Template, Trigger, TriggerRef, WebAuth, WebRequest, WebScenario,
};
use super::expression;
use crate::source::{File, ReadError};
use crate::xml;

/// The configuration the files describe together.
pub fn read(files: &[File]) -> Result<Config, ReadError> {
    let mut config = Config::default();
    // Triggers at the top of an export, which name their hosts in their
    // expression: placed once every file is read.
    let mut loose = Vec::new();
    for file in files {
        let root = parse(file)?;
        let export = root
            .get("zabbix_export")
            .filter(|e| e.is_object())
            .ok_or_else(|| ReadError::file(&file.name, "not a Zabbix configuration export"))?;
        for host in list(export, "hosts") {
            config.hosts.push(host_of(host));
        }
        for template in list(export, "templates") {
            let template = template_of(template);
            config.templates.insert(template.host.clone(), template);
        }
        for media in list(export, "media_types")
            .into_iter()
            .chain(list(export, "mediaTypes"))
        {
            config.media_types.push(media_type_of(media));
        }
        loose.extend(list(export, "triggers").into_iter().map(trigger_of));
    }
    for trigger in loose {
        place(&mut config, trigger);
    }
    Ok(config)
}

/// One file's content as a JSON value, by extension or by its first
/// character.
fn parse(file: &File) -> Result<Value, ReadError> {
    let text = std::str::from_utf8(&file.content)
        .map_err(|_| ReadError::file(&file.name, "not UTF-8 text"))?;
    let text = text.trim_start_matches('\u{feff}');
    let extension = file
        .name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let first = text.trim_start().chars().next();
    let result = match (extension.as_str(), first) {
        ("xml", _) | (_, Some('<')) => xml::to_json(text),
        ("json", _) | (_, Some('{')) => serde_json::from_str(text).map_err(|e| e.to_string()),
        _ => serde_norway::from_str(text).map_err(|e| e.to_string()),
    };
    result.map_err(|reason| ReadError::file(&file.name, reason))
}

/// The value of `key` as text: strings as they are, numbers written out,
/// anything else empty.
pub(super) fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// The list under `key`. XML gives an object for a single child and an
/// empty string for an empty element; both are read as lists.
pub(super) fn list<'a>(value: &'a Value, key: &str) -> Vec<&'a Value> {
    match value.get(key) {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(object @ Value::Object(_)) => vec![object],
        _ => vec![],
    }
}

fn names(value: &Value, key: &str) -> Vec<String> {
    list(value, key)
        .into_iter()
        .map(|v| text(v, "name"))
        .filter(|n| !n.is_empty())
        .collect()
}

fn disabled(value: &Value) -> bool {
    text(value, "status") == "DISABLED"
}

fn macros_of(value: &Value) -> Vec<Macro> {
    list(value, "macros")
        .into_iter()
        .map(|m| {
            // Secret and vault macros are exported without their value.
            let kind = text(m, "type");
            let value = (kind != "SECRET_TEXT" && kind != "VAULT").then(|| text(m, "value"));
            Macro {
                name: text(m, "macro"),
                value,
            }
        })
        .collect()
}

fn host_of(value: &Value) -> Host {
    let interfaces = list(value, "interfaces")
        .into_iter()
        .map(|i| Interface {
            kind: match text(i, "type").as_str() {
                "SNMP" => InterfaceKind::Snmp,
                "IPMI" => InterfaceKind::Ipmi,
                "JMX" => InterfaceKind::Jmx,
                _ => InterfaceKind::Agent,
            },
            ip: text(i, "ip"),
            dns: text(i, "dns"),
            use_ip: text(i, "useip") != "NO",
        })
        .collect();
    let os = value
        .get("inventory")
        .map(|inventory| text(inventory, "os"))
        .filter(|os| !os.trim().is_empty());
    let (items, mut triggers) = items_of(value);
    triggers.extend(
        list(value, "triggers")
            .into_iter()
            .map(trigger_of)
            .collect::<Vec<_>>(),
    );
    Host {
        host: text(value, "host"),
        name: text(value, "name"),
        enabled: !disabled(value),
        interfaces,
        os,
        groups: names(value, "groups"),
        templates: names(value, "templates"),
        macros: macros_of(value),
        items,
        triggers,
        web: list(value, "httptests").into_iter().map(web_of).collect(),
    }
}

fn template_of(value: &Value) -> Template {
    let (items, triggers) = items_of(value);
    Template {
        host: text(value, "template"),
        templates: names(value, "templates"),
        macros: macros_of(value),
        items,
        triggers,
        web: list(value, "httptests").into_iter().map(web_of).collect(),
    }
}

/// Items and the triggers under them, discovery rules' prototypes
/// included.
fn items_of(value: &Value) -> (Vec<Item>, Vec<Trigger>) {
    let mut items = Vec::new();
    let mut triggers = Vec::new();
    for item in list(value, "items") {
        items.push(item_of(item, false));
        triggers.extend(list(item, "triggers").into_iter().map(trigger_of));
    }
    for rule in list(value, "discovery_rules") {
        let enabled = !disabled(rule);
        for item in list(rule, "item_prototypes") {
            let mut prototype = item_of(item, true);
            prototype.enabled &= enabled;
            items.push(prototype);
            for trigger in list(item, "trigger_prototypes") {
                let mut trigger = trigger_of(trigger);
                trigger.prototype = true;
                trigger.enabled &= enabled;
                triggers.push(trigger);
            }
        }
        for trigger in list(rule, "trigger_prototypes") {
            let mut trigger = trigger_of(trigger);
            trigger.prototype = true;
            trigger.enabled &= enabled;
            triggers.push(trigger);
        }
    }
    (items, triggers)
}

/// An export's item type, `ZABBIX_PASSIVE` when absent.
pub(super) fn item_kind(text: &str) -> ItemKind {
    match text {
        "" | "ZABBIX_PASSIVE" | "ZABBIX_ACTIVE" => ItemKind::Agent,
        "SIMPLE" => ItemKind::Simple,
        "SNMP_AGENT" | "SNMPV1" | "SNMPV2" | "SNMPV3" => ItemKind::Snmp,
        "IPMI" => ItemKind::Ipmi,
        "JMX" => ItemKind::Jmx,
        "INTERNAL" => ItemKind::Internal,
        "DEPENDENT" => ItemKind::Dependent,
        "CALCULATED" => ItemKind::Calculated,
        "HTTP_AGENT" => ItemKind::HttpAgent,
        "EXTERNAL" | "SSH" | "TELNET" | "SCRIPT" | "BROWSER" => ItemKind::Command,
        _ => ItemKind::Other,
    }
}

fn item_of(value: &Value, prototype: bool) -> Item {
    let kind = item_kind(&text(value, "type"));
    let delay = Some(text(value, "delay"))
        .filter(|d| !d.is_empty())
        .or_else(|| (!matches!(kind, ItemKind::Dependent | ItemKind::Other)).then(|| "1m".into()));
    Item {
        name: text(value, "name"),
        key: text(value, "key"),
        kind,
        delay,
        enabled: !disabled(value),
        prototype,
        http: (kind == ItemKind::HttpAgent).then(|| Box::new(http_agent_of(value))),
        snmp_oid: Some(text(value, "snmp_oid")).filter(|o| !o.is_empty()),
        template: None,
    }
}

fn trigger_of(value: &Value) -> Trigger {
    // Zabbix 5.0 and later name it `name`; 4.x `description`.
    let mut name = text(value, "name");
    if name.is_empty() {
        name = text(value, "description");
    }
    let recovery = (text(value, "recovery_mode") == "RECOVERY_EXPRESSION")
        .then(|| text(value, "recovery_expression"))
        .filter(|r| !r.trim().is_empty());
    Trigger {
        name,
        expression: text(value, "expression"),
        recovery,
        priority: Priority::parse(&text(value, "priority")),
        enabled: !disabled(value),
        prototype: false,
        template: None,
        tags: list(value, "tags")
            .into_iter()
            .map(|t| (text(t, "tag"), text(t, "value")))
            .collect(),
        id: None,
        dependencies: list(value, "dependencies")
            .into_iter()
            .map(|d| {
                let mut name = text(d, "name");
                if name.is_empty() {
                    name = text(d, "description");
                }
                TriggerRef {
                    id: None,
                    name,
                    expression: text(d, "expression"),
                }
            })
            .filter(|d| !d.name.is_empty())
            .collect(),
    }
}

/// `name` and `value` pairs, as exports write headers and query fields.
fn pairs(value: &Value, key: &str) -> Vec<(String, String)> {
    list(value, key)
        .into_iter()
        .map(|p| (text(p, "name"), text(p, "value")))
        .filter(|(n, _)| !n.is_empty())
        .collect()
}

/// Whether `key` holds anything: a non-empty text or list.
fn has(value: &Value, key: &str) -> bool {
    !text(value, key).trim().is_empty() || !list(value, key).is_empty()
}

/// The authentication of a scenario or an item, by its export keys.
fn auth_of(value: &Value, kind: &str, user: &str, password: &str) -> WebAuth {
    let kind = match text(value, kind).as_str() {
        "BASIC" => "basic",
        "NTLM" => "ntlm",
        "KERBEROS" => "kerberos",
        "DIGEST" => "digest",
        _ => "",
    };
    WebAuth {
        kind: kind.into(),
        user: if kind.is_empty() {
            String::new()
        } else {
            text(value, user)
        },
        password: !kind.is_empty() && !text(value, password).is_empty(),
    }
}

/// A request as exports write a step or an HTTP agent item; `status`
/// is the default of its required codes, which exports leave out.
fn request_of(value: &Value, status: &str) -> WebRequest {
    let status_codes = match value.get("status_codes") {
        Some(_) => text(value, "status_codes"),
        None => status.to_owned(),
    };
    let method = match text(value, "request_method").as_str() {
        "" => "GET".to_owned(),
        other => other.to_owned(),
    };
    WebRequest {
        name: text(value, "name"),
        url: text(value, "url"),
        query: pairs(value, "query_fields"),
        method,
        body: has(value, "posts"),
        status_codes,
        required: text(value, "required"),
        follow_redirects: text(value, "follow_redirects") != "NO",
        headers_only: text(value, "retrieve_mode") == "HEADERS",
        headers: pairs(value, "headers"),
        variables: has(value, "variables"),
        timeout: Some(text(value, "timeout")).filter(|t| !t.is_empty()),
    }
}

/// An HTTP agent item's request: its codes default to `200`.
fn http_agent_of(value: &Value) -> HttpAgent {
    HttpAgent {
        request: request_of(value, "200"),
        auth: auth_of(value, "authtype", "username", "password"),
    }
}

fn web_of(value: &Value) -> WebScenario {
    WebScenario {
        name: text(value, "name"),
        delay: Some(text(value, "delay"))
            .filter(|d| !d.is_empty())
            .or(Some("1m".into())),
        enabled: !disabled(value),
        // A step without codes passes any.
        steps: list(value, "steps")
            .into_iter()
            .map(|s| request_of(s, ""))
            .collect(),
        headers: pairs(value, "headers"),
        variables: has(value, "variables"),
        auth: auth_of(value, "authentication", "http_user", "http_password"),
    }
}

fn media_type_of(value: &Value) -> MediaType {
    let kind = match text(value, "type").as_str() {
        "SCRIPT" => MediaKind::Script,
        "SMS" => MediaKind::Sms,
        "WEBHOOK" => MediaKind::Webhook,
        _ => MediaKind::Email,
    };
    let authenticated = text(value, "smtp_authentication") == "PASSWORD";
    let password = text(value, "password");
    MediaType {
        name: text(value, "name"),
        kind,
        enabled: !disabled(value),
        smtp_server: text(value, "smtp_server"),
        smtp_port: text(value, "smtp_port").parse().unwrap_or(25),
        smtp_email: text(value, "smtp_email"),
        username: if authenticated {
            text(value, "username")
        } else {
            String::new()
        },
        password: (authenticated && !password.is_empty()).then_some(password),
        parameters: list(value, "parameters")
            .into_iter()
            .map(|p| (text(p, "name"), text(p, "value")))
            .collect(),
    }
}

/// Puts a top-level trigger on the host or template its expression names
/// first; one naming nothing known stays on the first host, where the
/// mapping says what it misses.
fn place(config: &mut Config, trigger: Trigger) {
    let owner = expression::parse(&trigger.expression)
        .ok()
        .and_then(|e| first_host(&e));
    if let Some(owner) = owner {
        if let Some(template) = config.templates.get_mut(&owner) {
            template.triggers.push(trigger);
            return;
        }
        if let Some(host) = config.hosts.iter_mut().find(|h| h.host == owner) {
            host.triggers.push(trigger);
            return;
        }
    }
    if let Some(host) = config.hosts.first_mut() {
        host.triggers.push(trigger);
    }
}

fn first_host(expr: &expression::Expr) -> Option<String> {
    use expression::Expr;
    match expr {
        Expr::Call(call) => call.item.as_ref().map(|i| i.host.clone()),
        Expr::Not(e) | Expr::Negative(e) => first_host(e),
        Expr::Binary(_, a, b) => first_host(a).or_else(|| first_host(b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, content: &str) -> File {
        File {
            name: name.into(),
            content: content.as_bytes().to_vec(),
        }
    }

    #[test]
    fn defaults_fill_what_exports_leave_out() {
        let yaml = r#"
zabbix_export:
  version: '7.0'
  hosts:
    - host: web-01
      name: 'Web server 01'
      templates:
        - name: 'Linux by Zabbix agent'
      groups:
        - name: Web
      interfaces:
        - ip: 192.0.2.10
          interface_ref: if1
        - type: SNMP
          useip: 'NO'
          dns: web-01.example.com
      items:
        - name: Port
          key: 'net.tcp.port[,5432]'
          triggers:
            - expression: 'last(/web-01/net.tcp.port[,5432])=0'
              name: Down
        - name: Off
          type: DEPENDENT
          key: x.y
          status: DISABLED
      macros:
        - macro: '{$A}'
          value: '1'
        - macro: '{$SECRET}'
          type: SECRET_TEXT
      inventory:
        os: Debian 12
  triggers:
    - expression: 'min(/Linux by Zabbix agent/system.cpu.util,5m)>90'
      name: Busy
      priority: HIGH
      recovery_mode: RECOVERY_EXPRESSION
      recovery_expression: 'max(/Linux by Zabbix agent/system.cpu.util,5m)<80'
"#;
        let templates = r#"<?xml version="1.0" encoding="UTF-8"?>
<zabbix_export><version>7.0</version><templates><template>
<template>Linux by Zabbix agent</template><name>Linux by Zabbix agent</name>
<macros><macro><macro>{$CPU.UTIL.CRIT}</macro><value>90</value></macro></macros>
</template></templates></zabbix_export>"#;
        let config = read(&[file("hosts.yaml", yaml), file("t.xml", templates)]).unwrap();
        let host = &config.hosts[0];
        assert_eq!(host.host, "web-01");
        assert_eq!(host.name, "Web server 01");
        assert_eq!(host.groups, vec!["Web"]);
        assert_eq!(host.templates, vec!["Linux by Zabbix agent"]);
        assert_eq!(host.os.as_deref(), Some("Debian 12"));
        assert_eq!(host.interfaces[0].kind, InterfaceKind::Agent);
        assert!(host.interfaces[0].use_ip);
        assert_eq!(host.interfaces[1].kind, InterfaceKind::Snmp);
        assert!(!host.interfaces[1].use_ip);
        assert_eq!(host.items[0].kind, ItemKind::Agent);
        assert_eq!(host.items[0].delay.as_deref(), Some("1m"));
        assert!(host.items[0].enabled);
        assert_eq!(host.items[1].kind, ItemKind::Dependent);
        assert_eq!(host.items[1].delay, None);
        assert!(!host.items[1].enabled);
        assert_eq!(host.triggers.len(), 1);
        assert_eq!(host.triggers[0].priority, Priority::NotClassified);
        assert_eq!(
            host.macros,
            vec![
                Macro {
                    name: "{$A}".into(),
                    value: Some("1".into())
                },
                Macro {
                    name: "{$SECRET}".into(),
                    value: None
                }
            ]
        );
        // The top-level trigger went to the template it names.
        let template = &config.templates["Linux by Zabbix agent"];
        assert_eq!(template.triggers.len(), 1);
        assert_eq!(template.triggers[0].priority, Priority::High);
        assert_eq!(
            template.triggers[0].recovery.as_deref(),
            Some("max(/Linux by Zabbix agent/system.cpu.util,5m)<80")
        );
        assert_eq!(template.macros[0].value.as_deref(), Some("90"));
    }

    #[test]
    fn other_files_are_refused() {
        let error = read(&[file("x.json", r#"{"a": 1}"#)]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "x.json: not a Zabbix configuration export"
        );
        assert!(read(&[file("x.yaml", "a: [")]).is_err());
        assert!(read(&[file("x.xml", "<a><b></a>")]).is_err());
    }
}
