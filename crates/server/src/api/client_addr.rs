//! The client's address, which sign-in and screen limits count by
//! (ADR-0110).
//!
//! The API speaks plain HTTP behind a reverse proxy (ADR-0006), so the TCP
//! peer is usually the proxy. Its `X-Forwarded-For` is believed only when
//! the peer is one of `api_trusted_proxies`; any other peer, like a local
//! user reaching the listener directly, counts by its own address.

use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::Extension;
use monitorflow_common::config::TrustedProxy;

use crate::enroll::limiter::UNKNOWN_SOURCE;

/// How often host names are looked up again when a peer matches none of
/// the proxies: a proxy container restarted with a new address is known
/// within a second, and a stranger cannot cause more lookups than that.
const RESOLVE_AT_MOST_EVERY: Duration = Duration::from_secs(1);

/// The peer address axum records when the API is served with
/// `into_make_service_with_connect_info`; absent in tests that call the
/// router directly.
pub type Peer = Option<Extension<ConnectInfo<SocketAddr>>>;

/// `api_trusted_proxies`, with the addresses of its host names.
#[derive(Debug, Default)]
pub struct TrustedProxies {
    networks: Vec<TrustedProxy>,
    hosts: Vec<String>,
    resolved: Mutex<Resolved>,
}

#[derive(Debug, Default)]
struct Resolved {
    addrs: Vec<IpAddr>,
    at: Option<Instant>,
}

impl TrustedProxies {
    /// From the configuration, which has checked every entry; one that
    /// does not parse is left out.
    pub fn new(entries: &[String]) -> Self {
        let (mut networks, mut hosts) = (Vec::new(), Vec::new());
        for entry in entries {
            match entry.parse() {
                Ok(TrustedProxy::Host(host)) => hosts.push(host),
                Ok(network) => networks.push(network),
                Err(error) => tracing::warn!(%error, "ignoring a trusted proxy"),
            }
        }
        Self {
            networks,
            hosts,
            resolved: Mutex::default(),
        }
    }

    /// Whether `peer` is a trusted proxy. Looks host names up again when
    /// it matches nothing and the last lookup is old enough.
    pub async fn trusts(&self, peer: IpAddr) -> bool {
        let peer = peer.to_canonical();
        if self.networks.iter().any(|n| n.contains(peer)) {
            return true;
        }
        if self.hosts.is_empty() {
            return false;
        }
        {
            let resolved = self.resolved.lock().expect("lock poisoned");
            if resolved.addrs.contains(&peer) {
                return true;
            }
            if resolved
                .at
                .is_some_and(|at| at.elapsed() < RESOLVE_AT_MOST_EVERY)
            {
                return false;
            }
        }
        let addrs = self.resolve().await;
        let trusted = addrs.contains(&peer);
        *self.resolved.lock().expect("lock poisoned") = Resolved {
            addrs,
            at: Some(Instant::now()),
        };
        trusted
    }

    async fn resolve(&self) -> Vec<IpAddr> {
        let mut addrs = Vec::new();
        for host in &self.hosts {
            match tokio::net::lookup_host((host.as_str(), 0)).await {
                Ok(found) => addrs.extend(found.map(|a| a.ip().to_canonical())),
                Err(error) => tracing::debug!(%host, %error, "trusted proxy not resolved"),
            }
        }
        addrs
    }

    /// The client's address: the last `X-Forwarded-For` entry, which a
    /// trusted proxy appends, when `peer` is one; otherwise the peer's own.
    /// A trusted proxy that sends no usable entry, or no recorded peer,
    /// gives [`UNKNOWN_SOURCE`], one bucket for all.
    pub async fn source(&self, peer: &Peer, headers: &HeaderMap) -> IpAddr {
        let Some(Extension(ConnectInfo(peer))) = peer else {
            return UNKNOWN_SOURCE;
        };
        let peer = peer.ip().to_canonical();
        if !self.trusts(peer).await {
            return peer;
        }
        forwarded_for(headers).unwrap_or(UNKNOWN_SOURCE)
    }
}

/// The last `X-Forwarded-For` entry, the one the nearest proxy appended.
fn forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn peer(addr: &str) -> Peer {
        Some(Extension(ConnectInfo(SocketAddr::new(
            addr.parse().unwrap(),
            50_000,
        ))))
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn forwarded(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static(value));
        headers
    }

    fn proxies(entries: &[&str]) -> TrustedProxies {
        TrustedProxies::new(&entries.iter().map(|e| e.to_string()).collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn a_trusted_proxy_gives_the_address_it_appended() {
        let trusted = proxies(&["127.0.0.1", "::1"]);
        let headers = forwarded("203.0.113.9, 198.51.100.4");
        assert_eq!(
            trusted.source(&peer("127.0.0.1"), &headers).await,
            ip("198.51.100.4")
        );
        assert_eq!(
            trusted.source(&peer("::1"), &headers).await,
            ip("198.51.100.4")
        );
        // A v4 proxy on a dual-stack socket.
        assert_eq!(
            trusted.source(&peer("::ffff:127.0.0.1"), &headers).await,
            ip("198.51.100.4")
        );
        // Nothing usable: one bucket, not the proxy's address.
        let none = HeaderMap::new();
        assert_eq!(
            trusted.source(&peer("127.0.0.1"), &none).await,
            UNKNOWN_SOURCE
        );
        let nonsense = forwarded("nonsense");
        assert_eq!(
            trusted.source(&peer("127.0.0.1"), &nonsense).await,
            UNKNOWN_SOURCE
        );
    }

    #[tokio::test]
    async fn another_peer_counts_by_its_own_address() {
        let trusted = proxies(&["10.1.0.0/16"]);
        let headers = forwarded("198.51.100.4");
        assert_eq!(
            trusted.source(&peer("10.1.2.3"), &headers).await,
            ip("198.51.100.4")
        );
        assert_eq!(
            trusted.source(&peer("10.2.0.1"), &headers).await,
            ip("10.2.0.1")
        );
        assert_eq!(
            trusted.source(&peer("127.0.0.1"), &headers).await,
            ip("127.0.0.1")
        );
        assert_eq!(
            trusted.source(&peer("::ffff:10.2.0.1"), &headers).await,
            ip("10.2.0.1")
        );
        let nobody = proxies(&[]);
        assert_eq!(
            nobody.source(&peer("127.0.0.1"), &headers).await,
            ip("127.0.0.1")
        );
    }

    #[tokio::test]
    async fn without_a_recorded_peer_every_client_shares_one_bucket() {
        let trusted = proxies(&["127.0.0.1"]);
        let headers = forwarded("198.51.100.4");
        assert_eq!(trusted.source(&None, &headers).await, UNKNOWN_SOURCE);
    }

    #[tokio::test]
    async fn host_names_are_resolved_and_looked_up_again_at_most_every_second() {
        let trusted = proxies(&["localhost"]);
        let headers = forwarded("198.51.100.4");
        assert_eq!(
            trusted.source(&peer("127.0.0.1"), &headers).await,
            ip("198.51.100.4")
        );
        // A stranger right after: no second lookup, its own address.
        assert_eq!(
            trusted.source(&peer("192.0.2.7"), &headers).await,
            ip("192.0.2.7")
        );
        let at = trusted.resolved.lock().unwrap().at.unwrap();
        assert!(trusted.trusts(ip("127.0.0.1")).await);
        assert!(!trusted.trusts(ip("192.0.2.8")).await);
        assert_eq!(trusted.resolved.lock().unwrap().at.unwrap(), at);
    }
}
