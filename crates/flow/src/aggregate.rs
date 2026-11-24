//! Flows added up per exporter and window. At the end of a window each
//! exporter gives one [`FlowReport`]: its totals, the [`TOP`] largest keys
//! of each dimension, and the [`CONVERSATIONS`] largest conversations.
//! The rest of a dimension is its total minus its keys, so totals stay
//! exact however many addresses there were.
//!
//! Memory is bounded: past [`MAX_KEYS`] keys in one dimension of one
//! exporter, new keys of the window count in its total only.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;

use monitorflow_common::proto::v1::{FlowConversation, FlowReport, FlowTop};

use crate::apps::{app_key, protocol_key};
use crate::Flow;

/// Keys kept per dimension in a report.
pub const TOP: usize = 20;
/// Conversations kept in a report.
pub const CONVERSATIONS: usize = 100;
/// Keys counted per dimension, exporter and window.
pub const MAX_KEYS: usize = 50_000;

/// The dimensions of a report, by their names on the wire and in the API.
pub const DIMENSIONS: [&str; 6] = ["src", "dst", "app", "protocol", "in", "out"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Count {
    bytes: u64,
    packets: u64,
}

impl Count {
    fn add(&mut self, flow: &Flow) {
        self.bytes = self.bytes.saturating_add(flow.bytes);
        self.packets = self.packets.saturating_add(flow.packets);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Conversation {
    src: IpAddr,
    dst: IpAddr,
    protocol: u8,
    port: AppPort,
    input: u32,
    output: u32,
}

/// The protocol and service port of a flow, before it becomes a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AppPort(u8, u16, u16);

impl AppPort {
    fn of(flow: &Flow) -> Self {
        // The same key whichever way the packets went.
        let (a, b) = if flow.src_port <= flow.dst_port {
            (flow.src_port, flow.dst_port)
        } else {
            (flow.dst_port, flow.src_port)
        };
        Self(flow.protocol, a, b)
    }

    fn key(self) -> String {
        app_key(self.0, self.1, self.2)
    }
}

#[derive(Debug, Default)]
struct Totals {
    all: Count,
    src: HashMap<IpAddr, Count>,
    dst: HashMap<IpAddr, Count>,
    app: HashMap<AppPort, Count>,
    protocol: HashMap<u8, Count>,
    input: HashMap<u32, Count>,
    output: HashMap<u32, Count>,
    conversations: HashMap<Conversation, Count>,
}

fn add<K: Eq + Hash>(map: &mut HashMap<K, Count>, key: K, flow: &Flow) {
    let len = map.len();
    match map.get_mut(&key) {
        Some(count) => count.add(flow),
        None if len < MAX_KEYS => map.entry(key).or_default().add(flow),
        None => {}
    }
}

fn top<K>(map: HashMap<K, Count>, n: usize, key: impl Fn(K) -> String) -> Vec<(String, Count)> {
    let mut all: Vec<(K, Count)> = map.into_iter().collect();
    let n = n.min(all.len());
    if n == 0 {
        return Vec::new();
    }
    all.select_nth_unstable_by(n - 1, |a, b| b.1.bytes.cmp(&a.1.bytes));
    all.truncate(n);
    // Keys of the same name (applications seen both ways) merge.
    let mut merged: HashMap<String, Count> = HashMap::new();
    for (k, count) in all {
        let entry = merged.entry(key(k)).or_default();
        entry.bytes = entry.bytes.saturating_add(count.bytes);
        entry.packets = entry.packets.saturating_add(count.packets);
    }
    let mut out: Vec<(String, Count)> = merged.into_iter().collect();
    out.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then_with(|| a.0.cmp(&b.0)));
    out
}

impl Totals {
    fn add(&mut self, flow: &Flow) {
        self.all.add(flow);
        add(&mut self.src, flow.src, flow);
        add(&mut self.dst, flow.dst, flow);
        let port = AppPort::of(flow);
        add(&mut self.app, port, flow);
        add(&mut self.protocol, flow.protocol, flow);
        add(&mut self.input, flow.input, flow);
        add(&mut self.output, flow.output, flow);
        add(
            &mut self.conversations,
            Conversation {
                src: flow.src,
                dst: flow.dst,
                protocol: flow.protocol,
                port,
                input: flow.input,
                output: flow.output,
            },
            flow,
        );
    }

    fn report(self, device: String, start_unix_ms: i64, seconds: u32) -> FlowReport {
        let mut tops = Vec::new();
        let mut push = |dimension: &str, keys: Vec<(String, Count)>| {
            tops.extend(keys.into_iter().map(|(key, c)| FlowTop {
                dimension: dimension.into(),
                key,
                bytes: c.bytes,
                packets: c.packets,
            }));
        };
        push("src", top(self.src, TOP, |a| a.to_string()));
        push("dst", top(self.dst, TOP, |a| a.to_string()));
        push("app", top(self.app, TOP, AppPort::key));
        push("protocol", top(self.protocol, TOP, protocol_key));
        push("in", top(self.input, TOP, |i| i.to_string()));
        push("out", top(self.output, TOP, |i| i.to_string()));
        let mut conversations: Vec<(Conversation, Count)> =
            self.conversations.into_iter().collect();
        let n = CONVERSATIONS.min(conversations.len());
        if n > 0 {
            conversations.select_nth_unstable_by(n - 1, |a, b| b.1.bytes.cmp(&a.1.bytes));
            conversations.truncate(n);
        }
        conversations.sort_by_key(|c| std::cmp::Reverse(c.1.bytes));
        FlowReport {
            device,
            start_unix_ms,
            seconds,
            bytes: self.all.bytes,
            packets: self.all.packets,
            tops,
            conversations: conversations
                .into_iter()
                .map(|(c, count)| FlowConversation {
                    src: c.src.to_string(),
                    dst: c.dst.to_string(),
                    app: c.port.key(),
                    input: c.input,
                    output: c.output,
                    bytes: count.bytes,
                    packets: count.packets,
                })
                .collect(),
        }
    }
}

/// Windows of flows, per exporter device.
#[derive(Debug)]
pub struct Aggregator {
    window_ms: i64,
    start_ms: i64,
    devices: HashMap<String, Totals>,
}

impl Aggregator {
    /// Windows of `window_ms`, the first one holding `now_ms`.
    pub fn new(window_ms: i64, now_ms: i64) -> Self {
        let window_ms = window_ms.max(1_000);
        Self {
            window_ms,
            start_ms: now_ms - now_ms.rem_euclid(window_ms),
            devices: HashMap::new(),
        }
    }

    /// Counts `flow` from `device` in the current window.
    pub fn add(&mut self, device: &str, flow: &Flow) {
        match self.devices.get_mut(device) {
            Some(totals) => totals.add(flow),
            None => self.devices.entry(device.to_owned()).or_default().add(flow),
        }
    }

    /// When the current window ends.
    pub fn ends_at(&self) -> i64 {
        self.start_ms + self.window_ms
    }

    /// The reports of the window, if it ended by `now_ms`; the next one
    /// then holds `now_ms`. Exporters that sent nothing give no report.
    pub fn flush(&mut self, now_ms: i64) -> Vec<FlowReport> {
        if now_ms < self.ends_at() {
            return Vec::new();
        }
        let start = self.start_ms;
        let seconds = u32::try_from(self.window_ms / 1000).unwrap_or(u32::MAX);
        self.start_ms = now_ms - now_ms.rem_euclid(self.window_ms);
        let mut reports: Vec<FlowReport> = std::mem::take(&mut self.devices)
            .into_iter()
            .map(|(device, totals)| totals.report(device, start, seconds))
            .collect();
        reports.sort_by(|a, b| a.device.cmp(&b.device));
        reports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(src: &str, dst: &str, sport: u16, dport: u16, bytes: u64) -> Flow {
        Flow {
            src: src.parse().unwrap(),
            dst: dst.parse().unwrap(),
            protocol: 6,
            src_port: sport,
            dst_port: dport,
            input: 1,
            output: 2,
            bytes,
            packets: 1,
        }
    }

    fn keys<'a>(r: &'a FlowReport, dimension: &str) -> Vec<(&'a str, u64)> {
        r.tops
            .iter()
            .filter(|t| t.dimension == dimension)
            .map(|t| (t.key.as_str(), t.bytes))
            .collect()
    }

    #[test]
    fn windows_add_up_and_rank() {
        let mut a = Aggregator::new(60_000, 120_500);
        a.add("sw", &flow("192.0.2.1", "198.51.100.1", 50_000, 443, 1_000));
        a.add("sw", &flow("198.51.100.1", "192.0.2.1", 443, 50_000, 9_000));
        a.add("sw", &flow("192.0.2.2", "198.51.100.1", 40_000, 22, 500));
        assert!(a.flush(179_999).is_empty(), "the window is still open");
        let reports = a.flush(180_000);
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(
            (r.start_unix_ms, r.seconds, r.bytes, r.packets),
            (120_000, 60, 10_500, 3)
        );
        assert_eq!(keys(r, "src")[0], ("198.51.100.1", 9_000));
        assert_eq!(keys(r, "app"), vec![("tcp/443", 10_000), ("tcp/22", 500)]);
        assert_eq!(keys(r, "protocol"), vec![("tcp", 10_500)]);
        assert_eq!(keys(r, "in"), vec![("1", 10_500)]);
        assert_eq!(r.conversations[0].app, "tcp/443");
        assert_eq!(r.conversations[0].bytes, 9_000);
        assert_eq!(a.ends_at(), 240_000);
        assert!(a.flush(240_000).is_empty(), "nothing sent: no report");
    }

    #[test]
    fn tops_are_bounded_and_totals_exact() {
        let mut a = Aggregator::new(60_000, 0);
        for i in 0..200u32 {
            let src = std::net::Ipv4Addr::from(0xc000_0200 + i).to_string();
            a.add(
                "sw",
                &flow(&src, "203.0.113.1", 1_024 + i as u16, 80, u64::from(i) + 1),
            );
        }
        let r = &a.flush(60_000)[0];
        let src = keys(r, "src");
        assert_eq!(src.len(), TOP);
        assert_eq!(src[0].1, 200, "the largest first");
        assert_eq!(r.bytes, (1..=200).sum::<u64>());
        assert_eq!(r.conversations.len(), CONVERSATIONS);
    }

    #[test]
    fn keys_past_the_limit_count_in_the_total_only() {
        let mut t = Totals::default();
        for i in 0..(MAX_KEYS as u32 + 5) {
            let src = std::net::Ipv4Addr::from(i).to_string();
            t.add(&flow(&src, "203.0.113.1", 1, 2, 1));
        }
        assert_eq!(t.src.len(), MAX_KEYS);
        assert_eq!(t.all.bytes, MAX_KEYS as u64 + 5);
    }
}
