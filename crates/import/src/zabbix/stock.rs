//! Stock templates Zabbix ships, for exports that link them without
//! including them: their items, triggers and macros with default values,
//! read from `stock.yaml` (an export of Zabbix 7.0's templates, trimmed to
//! what the mapping reads). What comes from here is marked approximated:
//! the source may have changed the template.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::config::Template;
use super::export;
use crate::source::File;

/// Older and other names of the same templates.
const ALIASES: &[(&str, &str)] = &[
    ("Template OS Linux by Zabbix agent", "Linux by Zabbix agent"),
    (
        "Template OS Linux by Zabbix agent active",
        "Linux by Zabbix agent active",
    ),
    ("Template OS Linux", "Linux by Zabbix agent"),
    (
        "Template OS Windows by Zabbix agent",
        "Windows by Zabbix agent",
    ),
    (
        "Template OS Windows by Zabbix agent active",
        "Windows by Zabbix agent active",
    ),
    ("Template OS Windows", "Windows by Zabbix agent"),
    ("Template Module ICMP Ping", "ICMP Ping"),
    ("Template ICMP Ping", "ICMP Ping"),
    ("Template VM VMware", "VMware"),
    ("VMware FQDN", "VMware"),
];

fn all() -> &'static BTreeMap<String, Template> {
    static STOCK: OnceLock<BTreeMap<String, Template>> = OnceLock::new();
    STOCK.get_or_init(|| {
        let file = File {
            name: "stock.yaml".into(),
            content: include_bytes!("stock.yaml").to_vec(),
        };
        export::read(&[file])
            .map(|config| config.templates)
            .unwrap_or_default()
    })
}

/// The stock template of `name`, or of the name it had before.
pub fn template(name: &str) -> Option<Template> {
    let name = ALIASES
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new);
    all().get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_templates_are_known() {
        for name in [
            "Linux by Zabbix agent",
            "Linux by Zabbix agent active",
            "Windows by Zabbix agent",
            "Windows by Zabbix agent active",
            "ICMP Ping",
            "VMware",
        ] {
            let template = template(name).unwrap_or_else(|| panic!("{name}"));
            assert!(!template.items.is_empty(), "{name}");
            assert!(!template.triggers.is_empty(), "{name}");
        }
        let linux = template("Template OS Linux by Zabbix agent").unwrap();
        assert_eq!(linux.host, "Linux by Zabbix agent");
        assert!(linux
            .triggers
            .iter()
            .any(|t| t.expression.contains("system.cpu.util")));
        assert!(linux
            .macros
            .iter()
            .any(|m| m.name == "{$CPU.UTIL.CRIT}" && m.value.as_deref() == Some("90")));
        assert!(template("Some custom template").is_none());
    }
}
