//! Which names a relay may have (ADR-0066). A relay enrolled without names
//! reports its own, which are untrusted: an administrator confirms them,
//! and whatever could let a relay pose as something else is refused here,
//! so that an administrator of one customer cannot be tricked into it, or
//! do it on purpose.

use std::net::IpAddr;

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::token::MAX_SERVER_NAMES;
use crate::api::public_address::certifies;

/// Serialises confirmations, so two of them cannot claim one name.
const NAMES_LOCK: i64 = 0x6d66_726e; // "mfrn"

/// Why names cannot be confirmed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("`{0}` is not a DNS name or an IP address agents can reach")]
    Unusable(String),
    #[error("`{0}` is this server's name: a relay cannot have it")]
    ServerName(String),
    #[error("`{0}` is already the name of a relay of another site")]
    Taken(String),
    #[error("confirm at least one name, at most {MAX_SERVER_NAMES}")]
    Count,
}

/// A DNS name or address fit for a relay's certificate, in lower case. No
/// wildcard, and nothing that means "this host" to whoever connects.
pub fn usable(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    match name.parse::<IpAddr>() {
        Ok(ip) => reachable(&ip).then_some(ip.to_string()),
        Err(_) => {
            let local = name == "localhost" || name.ends_with(".localhost");
            let dns = !name.is_empty()
                && !name.contains('*')
                && rustls::pki_types::DnsName::try_from(name.as_str()).is_ok();
            (dns && !local).then_some(name)
        }
    }
}

fn reachable(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_broadcast(),
        IpAddr::V6(ip) => !ip.is_unicast_link_local(),
    }
}

/// What a relay reported, bounded: the input is untrusted, and whatever is
/// not usable just does not reach the administrator.
pub fn reported(names: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for name in names.iter().filter_map(|name| usable(name)) {
        if !kept.contains(&name) && kept.len() < MAX_SERVER_NAMES {
            kept.push(name);
        }
    }
    kept
}

/// The confirmed form of `names`, or why it cannot be. `server_names` are
/// those of the server's certificate and its public address: agents that
/// trust the server by one of them would accept this relay as the server.
pub fn check(names: &[String], server_names: &[String]) -> Result<Vec<String>, NameError> {
    if names.is_empty() || names.len() > MAX_SERVER_NAMES {
        return Err(NameError::Count);
    }
    let mut kept = Vec::with_capacity(names.len());
    for name in names {
        let usable = usable(name).ok_or_else(|| NameError::Unusable(name.clone()))?;
        if certifies(server_names, &usable) {
            return Err(NameError::ServerName(usable));
        }
        if !kept.contains(&usable) {
            kept.push(usable);
        }
    }
    Ok(kept)
}

/// A private address means something else on each isolated network, so two
/// sites may well both have a relay at `10.0.0.5`. Its agents reach only
/// their own.
fn only_local(name: &str) -> bool {
    match name.parse() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_private() || ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64
        }
        Ok(IpAddr::V6(ip)) => ip.segments()[0] & 0xfe00 == 0xfc00,
        Err(_) => false,
    }
}

/// Refuses a name that an unrevoked relay of another site has, by DNS or
/// by public address: its agents would accept this relay as theirs. Relays
/// of one site may share a name, as they may speak for the same agents.
/// Holds a lock until the end of `tx`.
pub async fn check_free(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
    names: &[String],
) -> Result<Result<(), NameError>, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(NAMES_LOCK)
        .execute(&mut **tx)
        .await?;
    let global: Vec<&String> = names.iter().filter(|name| !only_local(name)).collect();
    let taken: Option<String> = sqlx::query_scalar(
        "SELECT lower(held.name)
         FROM relays r
         LEFT JOIN LATERAL (SELECT t.server_names FROM enrollment_tokens t
                            WHERE t.role = 'relay' AND t.peer_id = r.peer_id
                              AND t.used_at IS NOT NULL
                            ORDER BY t.used_at DESC LIMIT 1) t ON true
         CROSS JOIN LATERAL unnest(coalesce(r.confirmed_names, t.server_names, '{}')) AS held (name)
         WHERE r.revoked_at IS NULL AND r.site_id <> $1 AND lower(held.name) = ANY($2)
         LIMIT 1",
    )
    .bind(site_id)
    .bind(global)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(taken.map_or(Ok(()), |name| Err(NameError::Taken(name))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn what_cannot_be_trusted_on_a_certificate_is_not_usable() {
        for name in [
            "",
            "*.example.com",
            "*",
            "localhost",
            "foo.localhost",
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "169.254.1.1",
            "fe80::1",
            "224.0.0.1",
            "255.255.255.255",
            "not a name",
        ] {
            assert_eq!(usable(name), None, "{name}");
        }
        assert_eq!(usable("Relay1.Example.").as_deref(), Some("relay1.example"));
        assert_eq!(usable("10.0.0.5").as_deref(), Some("10.0.0.5"));
        assert_eq!(usable("2001:DB8::5").as_deref(), Some("2001:db8::5"));
    }

    #[test]
    fn reports_are_cleaned_and_bounded() {
        assert_eq!(
            reported(&names(&["A.example", "a.example", "*.example", "10.0.0.5"])),
            ["a.example", "10.0.0.5"]
        );
        let many: Vec<String> = (0..100).map(|n| format!("r{n}.example")).collect();
        assert_eq!(reported(&many).len(), MAX_SERVER_NAMES);
    }

    #[test]
    fn the_servers_names_are_refused() {
        let server = names(&["mf.example", "10.0.0.2", "*.corp.example"]);
        let refused = |name: &str| check(&names(&[name]), &server).unwrap_err();
        assert_eq!(
            refused("MF.example"),
            NameError::ServerName("mf.example".into())
        );
        assert_eq!(
            refused("10.0.0.2"),
            NameError::ServerName("10.0.0.2".into())
        );
        assert_eq!(
            refused("relay.corp.example"),
            NameError::ServerName("relay.corp.example".into())
        );
        assert_eq!(
            check(&names(&["R1.example", "r1.example", "10.0.0.5"]), &server).unwrap(),
            ["r1.example", "10.0.0.5"]
        );
    }

    #[test]
    fn a_confirmation_names_between_one_and_the_limit() {
        assert_eq!(check(&[], &[]), Err(NameError::Count));
        let many: Vec<String> = (0..17).map(|n| format!("r{n}.example")).collect();
        assert_eq!(check(&many, &[]), Err(NameError::Count));
        assert_eq!(
            check(&names(&["*.example"]), &[]),
            Err(NameError::Unusable("*.example".into()))
        );
    }

    #[test]
    fn only_private_addresses_may_repeat_across_sites() {
        for name in [
            "10.0.0.5",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "fd00::1",
        ] {
            assert!(only_local(name), "{name}");
        }
        for name in ["relay.example", "8.8.8.8", "100.128.0.1", "2001:db8::5"] {
            assert!(!only_local(name), "{name}");
        }
    }
}
