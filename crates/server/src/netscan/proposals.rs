//! What a scan proposes for each address that answered (ADR-0154),
//! computed when read from the latest run and the site's inventory, so a
//! device added by hand or an agent that enrolls is recognised at once.

use std::collections::BTreeSet;
use std::net::IpAddr;

use serde::Serialize;
use uuid::Uuid;

use super::results::Host;
use crate::snmp;

/// SSH and RDP: a machine the agent can be installed on.
pub const SSH: u16 = 22;
pub const RDP: u16 = 3389;
/// WinRM over HTTP: Windows too.
pub const WINRM: u16 = 5985;
/// Templates of devices that never get the agent, even with SSH open.
const APPLIANCES: [&str; 5] = ["switch", "router", "firewall", "printer", "ups"];

/// A server or device of the site, as matching needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub id: Uuid,
    pub name: String,
    /// `agent` or `device`.
    pub kind: String,
    pub address: Option<String>,
    pub macs: Vec<String>,
}

/// Which server or device already watches an address, and how it was
/// recognised.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct MonitoredBy {
    pub server_id: Uuid,
    pub name: String,
    /// `address` (a device at it), `mac` (a server whose agent reports
    /// the MAC address the probe saw) or `name` (a server of that host
    /// name).
    #[schema(example = "mac")]
    pub matched_by: String,
}

/// A check a device added from the proposal gets.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = NetworkScanProposedCheck)]
pub struct ProposedCheck {
    /// `ping`, `port` or `snmp`.
    pub kind: String,
    /// The check's name: `ping`, `port_443`, `snmp_if`...
    pub name: String,
    /// The port of a `port` check.
    pub port: Option<u16>,
}

/// What a scan proposes for one address.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[schema(as = NetworkScanProposal)]
pub struct Proposal {
    /// `agent` (install the agent: SSH or RDP is open), `snmp_device`
    /// (a device with the checks of its template), `device` (a device
    /// with its ping and ports), `monitored` (the site watches it
    /// already) or `dismissed`.
    #[schema(example = "snmp_device")]
    pub action: String,
    /// For `agent`: `linux` when SSH is open, `windows` when RDP or WinRM
    /// is.
    pub os: Option<String>,
    /// For `monitored`.
    pub monitored_by: Option<MonitoredBy>,
    /// The name a device added from it gets: the SNMP name, else the
    /// reverse DNS name, else the address.
    pub name: String,
    /// For an SNMP device: the template detection suggests.
    pub template: Option<String>,
    /// What a device added from it is checked with.
    pub checks: Vec<ProposedCheck>,
}

fn first_label(name: &str) -> String {
    name.split('.').next().unwrap_or("").to_ascii_lowercase()
}

/// The server or device of `known` that watches `host`, if any.
pub fn monitored_by(host: &Host, known: &[Known]) -> Option<MonitoredBy> {
    let address = host.address.to_string();
    let by = |k: &Known, how: &str| MonitoredBy {
        server_id: k.id,
        name: k.name.clone(),
        matched_by: how.into(),
    };
    if let Some(k) = known.iter().find(|k| {
        k.address
            .as_deref()
            .and_then(|a| a.parse::<IpAddr>().ok())
            .is_some_and(|a| a == host.address)
            || k.address.as_deref() == Some(address.as_str())
    }) {
        return Some(by(k, "address"));
    }
    if let Some(mac) = &host.mac {
        if let Some(k) = known.iter().find(|k| k.macs.iter().any(|m| m == mac)) {
            return Some(by(k, "mac"));
        }
    }
    let names: BTreeSet<String> = [host.name.as_deref(), host.snmp_name.as_deref()]
        .into_iter()
        .flatten()
        .map(first_label)
        .filter(|n| !n.is_empty())
        .collect();
    known
        .iter()
        .find(|k| k.kind != "device" && names.contains(&first_label(&k.name)))
        .map(|k| by(k, "name"))
}

/// The name a device added from `host` gets.
pub fn device_name(host: &Host) -> String {
    host.snmp_name
        .clone()
        .filter(|n| n.chars().count() <= 128)
        .or_else(|| host.name.clone().filter(|n| n.chars().count() <= 128))
        .unwrap_or_else(|| host.address.to_string())
}

/// The checks of a device added from `host`, with `template` when it
/// answered SNMP.
pub fn checks(host: &Host, template: Option<&str>) -> Vec<ProposedCheck> {
    let mut checks = Vec::new();
    if host.ping {
        checks.push(ProposedCheck {
            kind: "ping".into(),
            name: "ping".into(),
            port: None,
        });
    }
    match template.and_then(snmp::template) {
        Some(template) => checks.extend(template.groups.iter().map(|g| ProposedCheck {
            kind: "snmp".into(),
            name: snmp::check_name(*g).into(),
            port: None,
        })),
        None => checks.extend(host.ports.iter().map(|p| ProposedCheck {
            kind: "port".into(),
            name: format!("port_{p}"),
            port: Some(*p),
        })),
    }
    checks
}

/// What the scan proposes for `host`.
pub fn propose(host: &Host, known: &[Known], dismissed: bool) -> Proposal {
    let mut proposal = Proposal {
        action: String::new(),
        os: None,
        monitored_by: None,
        name: device_name(host),
        template: None,
        checks: Vec::new(),
    };
    if let Some(by) = monitored_by(host, known) {
        proposal.action = "monitored".into();
        proposal.monitored_by = Some(by);
        return proposal;
    }
    let template = host.template.clone().filter(|_| host.snmp_set.is_some());
    let appliance = template.as_deref().is_some_and(|t| APPLIANCES.contains(&t));
    let has = |p: u16| host.ports.contains(&p);
    proposal.action = if dismissed {
        "dismissed"
    } else if !appliance && (has(SSH) || has(RDP) || has(WINRM)) {
        proposal.os = Some(
            if has(RDP) || has(WINRM) {
                "windows"
            } else {
                "linux"
            }
            .into(),
        );
        "agent"
    } else if template.is_some() {
        "snmp_device"
    } else {
        "device"
    }
    .into();
    proposal.template = template;
    proposal.checks = checks(host, proposal.template.as_deref());
    proposal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(address: &str) -> Host {
        let mut host = Host::new(address.parse().unwrap());
        host.ping = true;
        host
    }

    fn snmp(mut host: Host, template: &str) -> Host {
        host.snmp_set = Some(1);
        host.snmp_name = Some("ups-1".into());
        host.template = Some(template.into());
        host
    }

    fn known(name: &str, kind: &str, address: Option<&str>, macs: &[&str]) -> Known {
        Known {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: kind.into(),
            address: address.map(Into::into),
            macs: macs.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    #[test]
    fn already_monitored_addresses_are_recognised() {
        let inventory = [
            known("Core switch", "device", Some("192.0.2.1"), &[]),
            known("web-01", "agent", None, &["02:00:00:00:00:02"]),
            known("db-01.example.com", "agent", None, &[]),
            known("printer", "device", Some("printer.lan"), &[]),
        ];
        let by_address = propose(&host("192.0.2.1"), &inventory, false);
        assert_eq!(by_address.action, "monitored");
        assert_eq!(by_address.monitored_by.unwrap().matched_by, "address");
        let mut by_mac = host("192.0.2.2");
        by_mac.mac = Some("02:00:00:00:00:02".into());
        let by_mac = propose(&by_mac, &inventory, true);
        assert_eq!(by_mac.action, "monitored");
        assert_eq!(by_mac.monitored_by.unwrap().name, "web-01");
        let mut by_name = host("192.0.2.3");
        by_name.name = Some("db-01.lan".into());
        assert_eq!(
            propose(&by_name, &inventory, false)
                .monitored_by
                .unwrap()
                .matched_by,
            "name"
        );
        // A device is matched by its address only.
        let mut named = host("192.0.2.4");
        named.name = Some("printer.lan".into());
        assert_eq!(propose(&named, &inventory, false).action, "device");
    }

    #[test]
    fn what_is_proposed() {
        let mut ssh = host("192.0.2.10");
        ssh.ports = vec![22, 80];
        let agent = propose(&ssh, &[], false);
        assert_eq!(
            (agent.action.as_str(), agent.os.as_deref()),
            ("agent", Some("linux"))
        );
        let mut rdp = host("192.0.2.11");
        rdp.ports = vec![22, 3389];
        assert_eq!(propose(&rdp, &[], false).os.as_deref(), Some("windows"));

        // A switch with SSH is a switch.
        let mut switch = snmp(host("192.0.2.12"), "switch");
        switch.ports = vec![22];
        let proposal = propose(&switch, &[], false);
        assert_eq!(proposal.action, "snmp_device");
        assert_eq!(proposal.template.as_deref(), Some("switch"));
        assert_eq!(proposal.name, "ups-1");
        let names: Vec<&str> = proposal.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ping", "snmp", "snmp_if", "snmp_temp"]);

        // A Linux server with snmpd gets the agent.
        let mut nas = snmp(host("192.0.2.13"), "nas");
        nas.ports = vec![22];
        assert_eq!(propose(&nas, &[], false).action, "agent");

        let mut web = host("192.0.2.14");
        web.ports = vec![80, 443];
        web.name = Some("shop.lan".into());
        let device = propose(&web, &[], false);
        assert_eq!(device.action, "device");
        assert_eq!(device.name, "shop.lan");
        let names: Vec<&str> = device.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ping", "port_80", "port_443"]);

        assert_eq!(propose(&web, &[], true).action, "dismissed");
        let mut quiet = Host::new("192.0.2.15".parse().unwrap());
        quiet.ports = vec![8080];
        let quiet = propose(&quiet, &[], false);
        assert_eq!(quiet.name, "192.0.2.15");
        assert_eq!(quiet.checks.len(), 1);
    }
}
