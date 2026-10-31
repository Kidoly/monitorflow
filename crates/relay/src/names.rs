//! The names of a relay (ADR-0066). At enrollment it reports what it finds
//! on its host: the host name and the addresses of its interfaces. These
//! are only proposals: agents can verify the relay once an administrator
//! confirmed names and the CA wrote them in its certificate, so the relay
//! starts its listener and mirrors after that.

use std::net::IpAddr;

use monitorflow_agent::os;
use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::client::shutdown::Shutdown;
use tokio::sync::watch;

use crate::listener;

/// Interfaces of containers, virtual machines and tunnels, which agents of
/// the site do not reach the relay on.
const VIRTUAL: [&str; 7] = ["docker", "br-", "veth", "virbr", "vnet", "cni", "flannel"];
/// Names reported, what a certificate takes; the enrollment request is
/// small, and a host can have many addresses.
const MAX_REPORTED: usize = 16;

/// The host name and the names to report, from the host. Blocking.
pub fn local() -> (String, Vec<String>) {
    let hostname = os::hostname();
    let addresses = if_addrs::get_if_addrs().unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot list the network interfaces");
        Vec::new()
    });
    let addresses = addresses.iter().map(|i| (i.name.as_str(), i.ip()));
    let names = report(&hostname, addresses);
    (hostname, names)
}

/// What agents can likely use, best first, as agents' commands and the
/// mirror URL take the first name: a full DNS name, addresses, then a bare
/// host name, which often resolves only on the host itself.
fn report<'a>(hostname: &str, addresses: impl Iterator<Item = (&'a str, IpAddr)>) -> Vec<String> {
    let mut addresses: Vec<IpAddr> = addresses
        .filter(|(interface, ip)| {
            routable(ip) && !VIRTUAL.iter().any(|name| interface.starts_with(name))
        })
        .map(|(_, ip)| ip)
        .collect();
    addresses.sort_by_key(IpAddr::is_ipv6);
    addresses.dedup();
    let hostname = hostname.trim().to_ascii_lowercase();
    let host = (!hostname.is_empty() && hostname != "localhost").then_some(hostname);
    let (full, bare) = match host {
        Some(host) if host.contains('.') => (Some(host), None),
        host => (None, host),
    };
    full.into_iter()
        .chain(addresses.iter().map(IpAddr::to_string))
        .chain(bare)
        .take(MAX_REPORTED)
        .collect()
}

fn routable(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => !ip.is_link_local(),
        IpAddr::V6(ip) => !ip.is_unicast_link_local(),
    }
}

/// Whether the certificate names nothing yet, or cannot be read.
pub fn lacks_names(identity: &StoredIdentity) -> bool {
    listener::server_names(identity).map_or(true, |names| names.is_empty())
}

/// The relay's names, waiting until a certificate carries some; `None` on
/// shutdown.
pub async fn confirmed(
    mut identities: watch::Receiver<StoredIdentity>,
    mut stop: Shutdown,
) -> Option<Vec<String>> {
    let mut told = false;
    loop {
        match listener::server_names(&identities.borrow_and_update()) {
            Ok(names) if !names.is_empty() => return Some(names),
            Ok(_) if told => {}
            Ok(_) => {
                told = true;
                tracing::info!("waiting for an administrator to confirm this relay's server names under Relays in the web interface; agents are refused until then");
            }
            Err(error) => tracing::error!(%error, "cannot read the relay's server names"),
        }
        tokio::select! {
            changed = identities.changed() => {
                changed.ok()?;
            }
            () = stop.wait() => return None,
        }
    }
}

/// URL of the mirror listener on `port` at the first of `names`.
pub fn mirror_url(names: &[String], port: u16) -> Option<String> {
    let name = names.first()?;
    Some(match name.parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => format!("http://[{ip}]:{port}"),
        _ => format!("http://{name}:{port}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_full_name_leads_and_a_bare_one_comes_last() {
        let addresses = [("eth0", ip("2001:db8::5")), ("eth1", ip("10.0.0.5"))];
        assert_eq!(
            report("Relay1.Paris.example", addresses.into_iter()),
            ["relay1.paris.example", "10.0.0.5", "2001:db8::5"]
        );
        assert_eq!(
            report("relay1", addresses.into_iter()),
            ["10.0.0.5", "2001:db8::5", "relay1"]
        );
    }

    #[test]
    fn local_only_and_virtual_addresses_are_left_out() {
        let addresses = [
            ("lo", ip("127.0.0.1")),
            ("lo", ip("::1")),
            ("eth0", ip("169.254.0.3")),
            ("eth0", ip("fe80::1")),
            ("docker0", ip("172.17.0.1")),
            ("br-1234", ip("172.18.0.1")),
            ("veth99", ip("10.1.0.1")),
            ("wg0", ip("10.8.0.1")),
        ];
        assert_eq!(report("localhost", addresses.into_iter()), ["10.8.0.1"]);
    }

    #[test]
    fn an_address_on_two_interfaces_is_reported_once() {
        let addresses = [("eth0", ip("10.0.0.5")), ("eth1", ip("10.0.0.5"))];
        assert_eq!(report("", addresses.into_iter()), ["10.0.0.5"]);
    }

    #[test]
    fn reports_stay_small_on_a_host_with_many_addresses() {
        let addresses: Vec<(&str, IpAddr)> = (1..=200u8)
            .map(|n| ("eth0", IpAddr::from([10, 0, 0, n])))
            .collect();
        assert_eq!(report("relay1", addresses.into_iter()).len(), MAX_REPORTED);
    }

    #[test]
    fn the_mirror_url_uses_the_first_name() {
        let url = |name: &str| mirror_url(&[name.to_owned(), "other".to_owned()], 7480);
        assert_eq!(
            url("relay1.example").as_deref(),
            Some("http://relay1.example:7480")
        );
        assert_eq!(url("10.0.0.5").as_deref(), Some("http://10.0.0.5:7480"));
        assert_eq!(
            url("2001:db8::5").as_deref(),
            Some("http://[2001:db8::5]:7480")
        );
        assert_eq!(mirror_url(&[], 7480), None);
    }
}
