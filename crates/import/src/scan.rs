//! What readers share to describe a network scan (ADR-0154): the source's
//! address ranges as MonitorFlow takes them, the ports to try and the
//! nearest schedule. The server checks the result again with its own
//! rules; these only keep within them.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};

use crate::args;
use crate::model::Every;
use crate::notes::Note;

/// Most addresses a scan tries.
pub const MAX_ADDRESSES: usize = 1024;
/// Most entries in a scan's list.
pub const MAX_ENTRIES: usize = 64;
/// Most ports a scan tries.
pub const MAX_PORTS: usize = 32;

/// A scan's targets, from the source's ranges.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Targets {
    pub entries: Vec<String>,
    /// The addresses the source's ranges hold, repeats included.
    pub total: u64,
    /// Whether only the first [`MAX_ADDRESSES`] were kept.
    pub cut: bool,
    /// Entries that are not IP addresses, blocks or ranges, like names.
    pub skipped: Vec<String>,
}

impl Targets {
    /// What the reader says of what it left out, if anything.
    pub fn note(&self) -> Option<Note> {
        if let Some(entry) = self.skipped.first() {
            return Some(Note::new("scan_skipped", args! {"entries" => entry}));
        }
        self.cut
            .then(|| Note::new("scan_cut", args! {"count" => self.total}))
    }
}

/// One entry: IPv4 addresses by octet ranges, a run of addresses, or an
/// IPv6 address kept as is.
enum Range {
    Octets([(u8, u8); 4]),
    Run(u32, u32),
    V6(String),
}

impl Range {
    fn count(&self) -> u64 {
        match self {
            Self::Octets(o) => o.iter().map(|(a, b)| u64::from(b - a) + 1).product(),
            Self::Run(a, b) => u64::from(b - a) + 1,
            Self::V6(_) => 1,
        }
    }

    /// Its IPv4 addresses in order, until `keep` says stop.
    fn each(&self, keep: &mut impl FnMut(u32) -> bool) {
        match self {
            Self::Run(a, b) => {
                for n in *a..=*b {
                    if !keep(n) {
                        return;
                    }
                }
            }
            Self::Octets([o0, o1, o2, o3]) => {
                for a in o0.0..=o0.1 {
                    for b in o1.0..=o1.1 {
                        for c in o2.0..=o2.1 {
                            for d in o3.0..=o3.1 {
                                if !keep(u32::from(Ipv4Addr::new(a, b, c, d))) {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            Self::V6(_) => {}
        }
    }
}

fn octet(text: &str) -> Option<(u8, u8)> {
    let (a, b) = match text.split_once('-') {
        Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
        None => {
            let n = text.trim().parse().ok()?;
            (n, n)
        }
    };
    (a <= b).then_some((a, b))
}

/// The prefix of a mask like `255.255.255.0`, or of `24`.
fn prefix(text: &str) -> Option<u32> {
    let text = text.trim();
    if let Ok(n) = text.parse::<u32>() {
        return (n <= 32).then_some(n);
    }
    let mask = u32::from(text.parse::<Ipv4Addr>().ok()?);
    (mask.leading_ones() + mask.trailing_zeros() == 32).then(|| mask.leading_ones())
}

/// An entry read, and how MonitorFlow writes it when it can as is.
fn parse(entry: &str) -> Option<(Range, Option<String>)> {
    if let Some((base, bits)) = entry.split_once('/') {
        let base: Ipv4Addr = base.trim().parse().ok()?;
        let bits = prefix(bits)?;
        let size = 32 - bits;
        let first = u32::from(base) & if bits == 0 { 0 } else { u32::MAX << size };
        let last = first
            | if size == 32 {
                u32::MAX
            } else {
                (1 << size) - 1
            };
        // Network and broadcast addresses are not hosts, as the server says.
        let run = if bits < 31 {
            Range::Run(first + 1, last - 1)
        } else {
            Range::Run(first, last)
        };
        return Some((run, Some(format!("{}/{bits}", Ipv4Addr::from(first)))));
    }
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(match ip {
            IpAddr::V4(v4) => (Range::Run(v4.into(), v4.into()), Some(v4.to_string())),
            IpAddr::V6(v6) => (Range::V6(v6.to_string()), Some(v6.to_string())),
        });
    }
    // `192.168.1.10-192.168.1.50`.
    if let Some((a, b)) = entry.split_once('-') {
        if let (Ok(a), Ok(b)) = (a.trim().parse::<Ipv4Addr>(), b.trim().parse::<Ipv4Addr>()) {
            let (a, b) = (u32::from(a), u32::from(b));
            return (a <= b).then(|| (Range::Run(a, b), Some(entry.to_owned())));
        }
    }
    // Zabbix's `192.168.1-10.1-255`: a range in any octet.
    let parts: Vec<&str> = entry.split('.').collect();
    let [a, b, c, d] = parts.as_slice() else {
        return None;
    };
    let octets = [octet(a)?, octet(b)?, octet(c)?, octet(d)?];
    // MonitorFlow writes a range of the last octet as it is.
    let written = (octets[..3].iter().all(|(x, y)| x == y)).then(|| {
        let (from, to) = octets[3];
        let base = format!("{}.{}.{}", octets[0].0, octets[1].0, octets[2].0);
        if from == to {
            format!("{base}.{from}")
        } else {
            format!("{base}.{from}-{to}")
        }
    });
    Some((Range::Octets(octets), written))
}

/// `entries` as a scan's targets: kept as they are when MonitorFlow
/// writes them so and they hold at most [`MAX_ADDRESSES`]; otherwise
/// their first addresses, as ranges.
pub fn targets<S: AsRef<str>>(entries: &[S]) -> Targets {
    let mut out = Targets::default();
    let mut ranges = Vec::new();
    for entry in entries {
        let entry = entry.as_ref().trim();
        if entry.is_empty() {
            continue;
        }
        match parse(entry) {
            Some(range) => ranges.push(range),
            None => out.skipped.push(entry.to_owned()),
        }
    }
    out.total = ranges.iter().map(|(r, _)| r.count()).sum();
    let as_is = ranges.len() <= MAX_ENTRIES
        && out.total <= MAX_ADDRESSES as u64
        && ranges.iter().all(|(_, w)| w.is_some());
    if as_is {
        out.entries = ranges.into_iter().filter_map(|(_, w)| w).collect();
        return out;
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut kept: Vec<u32> = Vec::new();
    let mut v6 = Vec::new();
    for (range, _) in &ranges {
        if let Range::V6(address) = range {
            v6.push(address.clone());
            continue;
        }
        range.each(&mut |n| {
            if seen.contains(&n) {
                return true;
            }
            if kept.len() + v6.len() >= MAX_ADDRESSES {
                out.cut = true;
                return false;
            }
            seen.insert(n);
            kept.push(n);
            true
        });
    }
    // Runs of consecutive addresses within one /24.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for n in kept {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == n && *end >> 8 == n >> 8 => *end = n,
            _ => runs.push((n, n)),
        }
    }
    out.entries = runs
        .into_iter()
        .map(|(a, b)| {
            if a == b {
                Ipv4Addr::from(a).to_string()
            } else {
                format!("{}-{}", Ipv4Addr::from(a), b & 0xff)
            }
        })
        .chain(v6)
        .collect();
    if out.entries.len() > MAX_ENTRIES {
        out.entries.truncate(MAX_ENTRIES);
        out.cut = true;
    }
    out
}

/// Ports to try, in the order added, at most [`MAX_PORTS`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ports {
    pub kept: Vec<u16>,
    /// How many were asked in all, repeats left out.
    pub asked: u64,
}

impl Ports {
    /// Adds the ports from `from` to `to`.
    pub fn add(&mut self, from: u16, to: u16) {
        for port in from.max(1)..=to {
            if self.kept.contains(&port) {
                continue;
            }
            self.asked += 1;
            if self.kept.len() < MAX_PORTS {
                self.kept.push(port);
            }
        }
    }

    /// Adds ports written like `22` or `8000-8010,80`; `false` when the
    /// text is not that, like a macro.
    pub fn add_text(&mut self, text: &str) -> bool {
        let mut ranges = Vec::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let range = match part.split_once('-') {
                Some((a, b)) => a.trim().parse().ok().zip(b.trim().parse().ok()),
                None => part.parse().ok().map(|p| (p, p)),
            };
            match range {
                Some((a, b)) if a <= b && a > 0 => ranges.push((a, b)),
                _ => return false,
            }
        }
        for (a, b) in ranges {
            self.add(a, b);
        }
        true
    }

    /// The note when some were left out.
    pub fn note(&self) -> Option<Note> {
        (self.asked > self.kept.len() as u64)
            .then(|| Note::new("scan_ports_cut", args! {"count" => self.asked}))
    }
}

/// The nearest of an hour, a day and a week to `seconds`, by ratio, and
/// the note saying so when it is not the same.
pub fn every(seconds: u64, delay: &str) -> (Every, Option<Note>) {
    let wanted = (seconds.max(1) as f64).ln();
    let (every, length) = [
        (Every::Hour, 3_600u64),
        (Every::Day, 86_400),
        (Every::Week, 604_800),
    ]
    .into_iter()
    .min_by(|a, b| {
        let far = |s: u64| ((s as f64).ln() - wanted).abs();
        far(a.1).total_cmp(&far(b.1))
    })
    .expect("three choices");
    let note = (length != seconds).then(|| {
        Note::new(
            &format!("scan_every_{}", every.as_str()),
            args! {"delay" => delay.trim()},
        )
    });
    (every, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(entries: &[&str]) -> Targets {
        targets(entries)
    }

    #[test]
    fn ranges_monitorflow_writes_stay_as_they_are() {
        let t = of(&[
            "192.168.1.1-254",
            " 10.0.0.0/24 ",
            "10.1.2.3",
            "2001:db8::1",
        ]);
        assert_eq!(
            t.entries,
            ["192.168.1.1-254", "10.0.0.0/24", "10.1.2.3", "2001:db8::1"]
        );
        assert_eq!((t.total, t.cut), (254 + 254 + 1 + 1, false));
        assert_eq!(t.note(), None);
        // A block written from an address inside it, or with a mask.
        assert_eq!(of(&["10.0.0.77/24"]).entries, ["10.0.0.0/24"]);
        assert_eq!(of(&["10.0.0.0/255.255.255.0"]).entries, ["10.0.0.0/24"]);
        assert_eq!(
            of(&["10.0.0.0/255.0.255.0"]).skipped,
            ["10.0.0.0/255.0.255.0"]
        );
    }

    #[test]
    fn other_ranges_become_runs_of_their_addresses() {
        let t = of(&["192.168.1-2.1-3"]);
        assert_eq!(t.entries, ["192.168.1.1-3", "192.168.2.1-3"]);
        assert_eq!((t.total, t.cut), (6, false));
        let t = of(&["10.0.0.250-10.0.1.2"]);
        // A range of whole addresses the server reads as it is.
        assert_eq!(t.entries, ["10.0.0.250-10.0.1.2"]);
        assert_eq!(t.total, 9);
    }

    #[test]
    fn more_than_1024_addresses_keep_the_first() {
        let t = of(&["10.0.0.0/16"]);
        assert!(t.cut);
        assert_eq!(t.total, 65_534);
        assert_eq!(
            t.entries,
            [
                "10.0.0.1-255",
                "10.0.1.0-255",
                "10.0.2.0-255",
                "10.0.3.0-255",
                "10.0.4.0"
            ]
        );
        assert_eq!(t.note().unwrap().args["count"], "65534");
        // Repeats count once once rewritten.
        let t = of(&[
            "10.0.0.1-200",
            "10.0.0.1-200",
            "10.0.1.1-200",
            "10.0.2.1-200",
            "10.0.3.1-200",
            "10.0.4.1-200",
        ]);
        assert_eq!(
            t.entries,
            [
                "10.0.0.1-200",
                "10.0.1.1-200",
                "10.0.2.1-200",
                "10.0.3.1-200",
                "10.0.4.1-200"
            ]
        );
        assert!(!t.cut);
        // A huge range is not walked to its end.
        assert!(of(&["10.0-255.0-255.0-255"]).cut);
    }

    #[test]
    fn names_are_left_out() {
        let t = of(&["printer.example.com", "192.0.2.5", "fe80::1-ff"]);
        assert_eq!(t.entries, ["192.0.2.5"]);
        assert_eq!(t.skipped, ["printer.example.com", "fe80::1-ff"]);
        assert_eq!(t.note().unwrap().code, "scan_skipped");
    }

    #[test]
    fn ports_expand_and_stop_at_32() {
        let mut ports = Ports::default();
        assert!(ports.add_text("22, 8000-8002,80,22"));
        assert_eq!(ports.kept, [22, 8000, 8001, 8002, 80]);
        assert_eq!(ports.note(), None);
        assert!(!ports.add_text("{$PORT}"));
        assert!(!ports.add_text("0"));
        ports.add(1, 100);
        assert_eq!(ports.kept.len(), MAX_PORTS);
        // 5 first, then 98 more but 22 and 80 again.
        assert_eq!(ports.note().unwrap().args["count"], "103");
    }

    #[test]
    fn schedules_round_to_the_nearest() {
        assert_eq!(every(3600, "1h"), (Every::Hour, None));
        assert_eq!(every(86_400, "1d"), (Every::Day, None));
        let (e, note) = every(300, "5m");
        assert_eq!(e, Every::Hour);
        assert_eq!(note.unwrap().code, "scan_every_hour");
        assert_eq!(every(12 * 3600, "12h").0, Every::Day);
        assert_eq!(every(4 * 3600, "4h").0, Every::Hour);
        assert_eq!(every(3 * 86_400, "3d").0, Every::Week);
        assert_eq!(every(30 * 86_400, "30d").0, Every::Week);
    }
}
