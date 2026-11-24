//! The UDP side of a probe: listens on the flow ports while it has
//! exporters, takes datagrams only from their addresses, and sends the
//! window's reports out. NetFlow v9 and IPFIX templates of an address
//! that is no longer an exporter are forgotten.
//!
//! The listeners exist only while there are exporters, so a relay that
//! receives no flows opens no port. Every port accepts every format: the
//! datagram's version says which it is.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use monitorflow_common::proto::v1::FlowReports;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, MissedTickBehavior};

use crate::aggregate::Aggregator;
use crate::{decode, Templates, IPFIX_PORT, NETFLOW_PORT, SFLOW_PORT};

/// Datagrams waiting to be decoded; more are dropped and counted.
const QUEUE: usize = 8192;
/// Time between two resolutions of exporters' names, and two tries to
/// open a port that could not be.
const RESOLVE_EVERY: Duration = Duration::from_secs(600);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest UDP payload.
const MAX_DATAGRAM: usize = 65_535;

/// Where and how a probe listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The address to listen on; `None`: every address, IPv6 and IPv4.
    pub bind: Option<IpAddr>,
    /// UDP ports, each taking every format.
    pub ports: Vec<u16>,
    /// The length of a window.
    pub window: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bind: None,
            ports: vec![NETFLOW_PORT, SFLOW_PORT, IPFIX_PORT],
            window: Duration::from_secs(60),
        }
    }
}

/// A device whose flows are taken, and the addresses they come from: IP
/// addresses or DNS names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exporter {
    pub device: String,
    pub addresses: Vec<String>,
}

/// Counters since the start.
#[derive(Debug, Default)]
pub struct Stats {
    pub datagrams: AtomicU64,
    pub flows: AtomicU64,
    /// From an address that is no exporter.
    pub unknown: AtomicU64,
    pub malformed: AtomicU64,
    /// Datagrams dropped because the decoder was behind.
    pub overflow: AtomicU64,
    /// Reports dropped because the way up was full.
    pub reports_dropped: AtomicU64,
}

impl Stats {
    fn bump(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }
}

/// The receiving tasks of the open ports, stopped when dropped.
struct Listeners(Vec<JoinHandle<()>>);

impl Drop for Listeners {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Receives flows from `exporters` until it is closed, sending each
/// window's reports to `out`; a full `out` drops them, counted.
pub async fn run(
    settings: Settings,
    mut exporters: watch::Receiver<Vec<Exporter>>,
    out: mpsc::Sender<FlowReports>,
    stats: Arc<Stats>,
) {
    let (datagrams_tx, mut datagrams) = mpsc::channel::<(Vec<u8>, IpAddr)>(QUEUE);
    let mut aggregator = Aggregator::new(
        i64::try_from(settings.window.as_millis()).unwrap_or(60_000),
        now_ms(),
    );
    let mut templates = Templates::default();
    let mut known: HashMap<IpAddr, String> = HashMap::new();
    let mut listeners: Option<Listeners> = None;
    let mut flush = interval(Duration::from_secs(1));
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut again = interval(RESOLVE_EVERY);
    again.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut list = exporters.borrow_and_update().clone();
    let mut refresh = true;
    loop {
        if refresh {
            refresh = false;
            known = resolve(&list).await;
            templates.retain(|a| known.contains_key(a));
            if known.is_empty() {
                if listeners.take().is_some() {
                    tracing::info!("no flow exporters left; flow ports closed");
                }
            } else if listeners.is_none() {
                listeners = listen(&settings, &datagrams_tx, &stats).await;
            }
        }
        tokio::select! {
            changed = exporters.changed() => {
                if changed.is_err() {
                    break;
                }
                list = exporters.borrow_and_update().clone();
                refresh = true;
            }
            _ = again.tick() => refresh = true,
            _ = flush.tick() => {
                let reports = aggregator.flush(now_ms());
                if !reports.is_empty() && out.try_send(FlowReports { reports }).is_err() {
                    Stats::bump(&stats.reports_dropped, 1);
                    tracing::debug!("flow reports dropped: the way up is full");
                }
            }
            Some((data, from)) = datagrams.recv() => {
                Stats::bump(&stats.datagrams, 1);
                let device = match known.get(&from) {
                    Some(device) => Some(device.as_str()),
                    // An sFlow agent may send from an address that is not
                    // its own; sFlow has no templates to spoil.
                    None if data.get(..4) == Some(&[0, 0, 0, 5]) => None,
                    None => {
                        Stats::bump(&stats.unknown, 1);
                        continue;
                    }
                };
                let decoded = decode(&data, from, &mut templates);
                let device = match device {
                    Some(device) => device,
                    None => match decoded.agent.and_then(|a| known.get(&a.to_canonical())) {
                        Some(device) => device.as_str(),
                        None => {
                            Stats::bump(&stats.unknown, 1);
                            continue;
                        }
                    },
                };
                if decoded.malformed {
                    Stats::bump(&stats.malformed, 1);
                }
                Stats::bump(&stats.flows, decoded.flows.len() as u64);
                for flow in &decoded.flows {
                    aggregator.add(device, flow);
                }
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The addresses of `exporters`, names resolved; a name that does not
/// resolve is left out until the next try, logged.
pub async fn resolve(exporters: &[Exporter]) -> HashMap<IpAddr, String> {
    let mut known = HashMap::new();
    for exporter in exporters {
        for address in &exporter.addresses {
            if let Ok(ip) = address.parse::<IpAddr>() {
                known.insert(ip.to_canonical(), exporter.device.clone());
                continue;
            }
            match timeout(
                RESOLVE_TIMEOUT,
                tokio::net::lookup_host((address.as_str(), 0)),
            )
            .await
            {
                Ok(Ok(found)) => {
                    for a in found {
                        known.insert(a.ip().to_canonical(), exporter.device.clone());
                    }
                }
                _ => {
                    tracing::warn!(device = %exporter.device, name = %address.escape_debug(), "cannot resolve a flow exporter's name; its flows are dropped until it does")
                }
            }
        }
    }
    known
}

/// Opens the ports; `None` when none could be, retried later.
async fn listen(
    settings: &Settings,
    to: &mpsc::Sender<(Vec<u8>, IpAddr)>,
    stats: &Arc<Stats>,
) -> Option<Listeners> {
    let mut tasks = Vec::new();
    for &port in &settings.ports {
        let socket = match settings.bind {
            Some(ip) => UdpSocket::bind(SocketAddr::new(ip, port)).await,
            // Dual-stack where the host has IPv6, IPv4 otherwise.
            None => {
                match UdpSocket::bind(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port)).await {
                    Ok(socket) => Ok(socket),
                    Err(_) => {
                        UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port)).await
                    }
                }
            }
        };
        match socket {
            Ok(socket) => {
                tracing::info!(port, "listening for traffic flows");
                tasks.push(tokio::spawn(receive(socket, to.clone(), stats.clone())));
            }
            Err(error) => {
                tracing::warn!(port, %error, "cannot listen for traffic flows on this port")
            }
        }
    }
    (!tasks.is_empty()).then_some(Listeners(tasks))
}

async fn receive(socket: UdpSocket, to: mpsc::Sender<(Vec<u8>, IpAddr)>, stats: Arc<Stats>) {
    let mut buffer = vec![0u8; MAX_DATAGRAM];
    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((n, from)) => {
                let datagram = (buffer[..n].to_vec(), from.ip().to_canonical());
                match to.try_send(datagram) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => Stats::bump(&stats.overflow, 1),
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
            Err(error) => {
                tracing::debug!(%error, "flow socket error");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netflow5::tests::datagram;

    fn free_port() -> u16 {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn receives_only_from_exporters_and_reports_each_window() {
        let port = free_port();
        let settings = Settings {
            bind: Some(Ipv4Addr::LOCALHOST.into()),
            ports: vec![port],
            window: Duration::from_secs(1),
        };
        let (set, exporters) = watch::channel(Vec::new());
        let (out, mut reports) = mpsc::channel(8);
        let stats = Arc::new(Stats::default());
        let task = tokio::spawn(run(settings, exporters, out, stats.clone()));
        set.send(vec![Exporter {
            device: "router".into(),
            addresses: vec!["127.0.0.1".into()],
        }])
        .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let d = datagram(
            &[([192, 0, 2, 1], [198, 51, 100, 1], 6, 50_000, 443, 1500, 1)],
            0,
        );
        let report = timeout(Duration::from_secs(10), async {
            loop {
                let _ = sender.send_to(&d, ("127.0.0.1", port)).await;
                if let Ok(Some(r)) = timeout(Duration::from_millis(300), reports.recv()).await {
                    break r;
                }
            }
        })
        .await
        .expect("a report");
        assert_eq!(report.reports[0].device, "router");
        assert!(report.reports[0].bytes >= 1500);
        // No exporter left: the port closes.
        set.send(Vec::new()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok(),
            "port released"
        );
        drop(set);
        timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(stats.flows.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn names_resolve_and_unknown_names_are_left_out() {
        let known = resolve(&[
            Exporter {
                device: "a".into(),
                addresses: vec!["localhost".into(), "::ffff:192.0.2.9".into()],
            },
            Exporter {
                device: "b".into(),
                addresses: vec!["no-such-name.invalid".into()],
            },
        ])
        .await;
        assert_eq!(
            known
                .get(&"192.0.2.9".parse::<IpAddr>().unwrap())
                .map(String::as_str),
            Some("a")
        );
        assert!(known.values().all(|d| d == "a"));
    }
}
