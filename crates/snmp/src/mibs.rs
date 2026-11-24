//! What the common MIBs say, read from walked tables into samples. Pure
//! functions of the values, so they are tested without a device.

use std::collections::BTreeMap;

use monitorflow_sensors::Sample;

/// A group of MIB objects one check polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    System,
    Interfaces,
    Host,
    Ups,
    Temperature,
    Printer,
    Detect,
}

/// Every group but `detect`, in the order "Add a device" lists them.
pub const GROUPS: [Group; 6] = [
    Group::System,
    Group::Interfaces,
    Group::Host,
    Group::Ups,
    Group::Temperature,
    Group::Printer,
];

impl Group {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Interfaces => "interfaces",
            Self::Host => "host",
            Self::Ups => "ups",
            Self::Temperature => "temperature",
            Self::Printer => "printer",
            Self::Detect => "detect",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        GROUPS
            .into_iter()
            .chain([Self::Detect])
            .find(|g| g.as_str() == value)
    }

    /// The table or object whose presence says a device has the group.
    pub(crate) fn root(self) -> &'static [u32] {
        match self {
            Self::System | Self::Detect => SYS_DESCR,
            Self::Interfaces => IF_TABLE,
            Self::Host => HR_STORAGE,
            Self::Ups => UPS,
            Self::Temperature => ENT_SENSOR,
            Self::Printer => PRT_SUPPLIES,
        }
    }
}

pub(crate) const SYS_DESCR: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 1, 0];
pub(crate) const SYS_OBJECT_ID: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 2, 0];
pub(crate) const SYS_UPTIME: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 3, 0];
pub(crate) const SYS_NAME: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 5, 0];
pub(crate) const IF_TABLE: &[u32] = &[1, 3, 6, 1, 2, 1, 2, 2, 1];
pub(crate) const IFX_TABLE: &[u32] = &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1];
pub(crate) const HR_PROCESSOR_LOAD: &[u32] = &[1, 3, 6, 1, 2, 1, 25, 3, 3, 1, 2];
pub(crate) const HR_STORAGE: &[u32] = &[1, 3, 6, 1, 2, 1, 25, 2, 3, 1];
pub(crate) const UPS: &[u32] = &[1, 3, 6, 1, 2, 1, 33, 1];
pub(crate) const ENT_SENSOR: &[u32] = &[1, 3, 6, 1, 2, 1, 99, 1, 1, 1];
pub(crate) const ENT_PHYSICAL_NAME: &[u32] = &[1, 3, 6, 1, 2, 1, 47, 1, 1, 1, 1, 7];
pub(crate) const PRT_SUPPLIES: &[u32] = &[1, 3, 6, 1, 2, 1, 43, 11, 1, 1];

/// A value as the parsers need it.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Int(i64),
    /// Counter64, Counter32, Gauge32, TimeTicks.
    Uint(u64),
    Text(String),
    Oid(Vec<u32>),
}

impl Val {
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Uint(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// A walked table: column number, then the row index (the rest of the
/// OID, joined with dots), to the value.
pub type Table = BTreeMap<u32, BTreeMap<String, Val>>;

/// Rows of the walk of `root` as a [`Table`]: `(oid, value)` under it.
pub fn table(root: &[u32], rows: impl IntoIterator<Item = (Vec<u32>, Val)>) -> Table {
    let mut table = Table::new();
    for (oid, value) in rows {
        let Some(rest) = oid.strip_prefix(root) else {
            continue;
        };
        let Some((column, index)) = rest.split_first() else {
            continue;
        };
        let index = index
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        table.entry(*column).or_default().insert(index, value);
    }
    table
}

fn cell<'a>(table: &'a Table, column: u32, index: &str) -> Option<&'a Val> {
    table.get(&column)?.get(index)
}

fn number(table: &Table, column: u32, index: &str) -> Option<f64> {
    cell(table, column, index)?.number()
}

/// A name usable as an instance: printable, at most 256 bytes, trimmed.
pub fn instance_name(raw: &str) -> String {
    let clean: String = raw.chars().filter(|c| !c.is_control()).collect();
    monitorflow_sensors::validate::label_value(clean.trim())
}

/// `sysUpTime` in hundredths of a second, as seconds.
pub fn system(uptime_ticks: Option<f64>) -> Vec<Sample> {
    let mut samples = vec![Sample::metric("up", 1.0)];
    if let Some(ticks) = uptime_ticks {
        samples.push(Sample::metric("uptime_seconds", ticks / 100.0));
    }
    samples
}

/// One interface's counters at one collection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counters {
    pub at_ms: i64,
    pub values: [Option<f64>; 6],
    /// Whether the byte counters are 64-bit, which do not wrap.
    pub wide: bool,
}

/// The byte, error and discard counters, in and out, in this order.
const COUNTER_METRICS: [&str; 6] = [
    "in_bytes_per_second",
    "out_bytes_per_second",
    "in_errors_per_second",
    "out_errors_per_second",
    "in_discards_per_second",
    "out_discards_per_second",
];

/// The interfaces of `if_table` and `ifx_table`, and the rates since the
/// counters of `before`; returns the samples and the counters to keep.
/// Loopbacks are left out.
pub fn interfaces(
    if_table: &Table,
    ifx_table: &Table,
    before: &BTreeMap<String, Counters>,
    now_ms: i64,
) -> (Vec<Sample>, BTreeMap<String, Counters>) {
    let mut samples = Vec::new();
    let mut kept = BTreeMap::new();
    let Some(descrs) = if_table.get(&2) else {
        return (samples, kept);
    };
    let mut names_seen = BTreeMap::<String, u32>::new();
    for (index, descr) in descrs {
        // ifType 24: softwareLoopback.
        if number(if_table, 3, index) == Some(24.0) {
            continue;
        }
        let raw = cell(ifx_table, 1, index)
            .and_then(Val::text)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| descr.text())
            .unwrap_or(index);
        let mut name = instance_name(raw);
        // Two interfaces of one name keep apart by index.
        let seen = names_seen.entry(name.clone()).or_default();
        *seen += 1;
        if *seen > 1 {
            name = instance_name(&format!("{name} ({index})"));
        }
        let sample = |metric: &str, value: f64| Sample::metric(metric, value).with_instance(&name);
        if let Some(oper) = number(if_table, 8, index) {
            samples.push(sample("oper_up", f64::from(u8::from(oper == 1.0))));
        }
        if let Some(admin) = number(if_table, 7, index) {
            samples.push(sample("admin_up", f64::from(u8::from(admin == 1.0))));
        }
        // Traffic flows name interfaces by ifIndex (ADR-0184).
        if let Ok(if_index) = index.parse::<u32>() {
            samples.push(sample("if_index", f64::from(if_index)));
        }
        // ifHighSpeed is in Mbit/s; ifSpeed in bit/s, capped at 2^32-1.
        let speed = number(ifx_table, 15, index)
            .filter(|s| *s > 0.0)
            .map(|mbps| mbps * 1e6)
            .or_else(|| number(if_table, 5, index).filter(|s| *s > 0.0));
        if let Some(speed) = speed {
            samples.push(sample("speed_bits_per_second", speed));
        }
        let hc_in = number(ifx_table, 6, index);
        let hc_out = number(ifx_table, 10, index);
        let wide = hc_in.is_some() && hc_out.is_some();
        let (bytes_in, bytes_out) = if wide {
            (hc_in, hc_out)
        } else {
            (number(if_table, 10, index), number(if_table, 16, index))
        };
        let counters = Counters {
            at_ms: now_ms,
            values: [
                bytes_in,
                bytes_out,
                number(if_table, 14, index),
                number(if_table, 20, index),
                number(if_table, 13, index),
                number(if_table, 19, index),
            ],
            wide,
        };
        if let Some(old) = before.get(&name).filter(|old| old.at_ms < now_ms) {
            let seconds = (now_ms - old.at_ms) as f64 / 1000.0;
            for (i, metric) in COUNTER_METRICS.iter().enumerate() {
                let (Some(new), Some(was)) = (counters.values[i], old.values[i]) else {
                    continue;
                };
                // A counter that goes back was reset or wrapped: no rate.
                if new < was || old.wide != counters.wide {
                    continue;
                }
                let rate = (new - was) / seconds;
                samples.push(sample(metric, rate));
                if let (Some(speed), true) = (speed, i < 2) {
                    let ratio = (rate * 8.0 / speed).min(1.0);
                    samples.push(sample(
                        if i == 0 {
                            "in_utilization_ratio"
                        } else {
                            "out_utilization_ratio"
                        },
                        ratio,
                    ));
                }
            }
        }
        kept.insert(name, counters);
    }
    (samples, kept)
}

/// hrStorage types.
const HR_STORAGE_RAM: &[u32] = &[1, 3, 6, 1, 2, 1, 25, 2, 1, 2];
const HR_STORAGE_FIXED: &[u32] = &[1, 3, 6, 1, 2, 1, 25, 2, 1, 4];
const HR_STORAGE_NETWORK: &[u32] = &[1, 3, 6, 1, 2, 1, 25, 2, 1, 10];

/// Mount points of kernel file systems, not storage.
const PSEUDO: [&str; 4] = ["/proc", "/sys", "/dev", "/run"];

/// Processor load (one value per processor, in percent) and storage;
/// kernel file systems and repeats of one volume left out.
pub fn host(loads: &[f64], storage: &Table) -> Vec<Sample> {
    let mut samples = Vec::new();
    if !loads.is_empty() {
        let average = loads.iter().sum::<f64>() / loads.len() as f64;
        samples.push(Sample::metric(
            "cpu_usage_ratio",
            (average / 100.0).clamp(0.0, 1.0),
        ));
    }
    let Some(kinds) = storage.get(&2) else {
        return samples;
    };
    // One volume mounted at several places reports the same figures.
    let mut volumes = std::collections::BTreeSet::new();
    for (index, kind) in kinds {
        let Val::Oid(kind) = kind else { continue };
        let (Some(unit), Some(size), Some(used)) = (
            number(storage, 4, index),
            number(storage, 5, index),
            number(storage, 6, index),
        ) else {
            continue;
        };
        if size <= 0.0 || unit <= 0.0 {
            continue;
        }
        if kind.as_slice() == HR_STORAGE_RAM {
            samples.push(Sample::metric(
                "memory_used_ratio",
                (used / size).clamp(0.0, 1.0),
            ));
        } else if kind.as_slice() == HR_STORAGE_FIXED || kind.as_slice() == HR_STORAGE_NETWORK {
            let name = instance_name(cell(storage, 3, index).and_then(Val::text).unwrap_or(index));
            if PSEUDO
                .iter()
                .any(|p| name == *p || name.starts_with(&format!("{p}/")))
            {
                continue;
            }
            if !volumes.insert((size.to_bits(), used.to_bits(), unit.to_bits())) {
                continue;
            }
            let sample =
                |metric: &str, value: f64| Sample::metric(metric, value).with_instance(&name);
            samples.push(sample("storage_used_ratio", (used / size).clamp(0.0, 1.0)));
            samples.push(sample("storage_used_bytes", used * unit));
            samples.push(sample("storage_size_bytes", size * unit));
        }
    }
    samples
}

/// UPS-MIB, its scalars under `upsObjects` by the rest of their OID:
/// `2.1.0` battery status, `2.3.0` minutes left, `2.4.0` charge in
/// percent, `4.1.0` output source.
pub fn ups(values: &BTreeMap<String, Val>) -> Vec<Sample> {
    let get = |key: &str| values.get(key).and_then(Val::number);
    let mut samples = Vec::new();
    if let Some(charge) = get("2.4.0") {
        samples.push(Sample::metric(
            "battery_charge_ratio",
            (charge / 100.0).clamp(0.0, 1.0),
        ));
    }
    if let Some(minutes) = get("2.3.0") {
        samples.push(Sample::metric("runtime_seconds", minutes * 60.0));
    }
    // upsOutputSource 5: battery.
    if let Some(source) = get("4.1.0") {
        samples.push(Sample::metric(
            "on_battery",
            f64::from(u8::from(source == 5.0)),
        ));
    }
    // upsBatteryStatus 3: low, 4: depleted.
    if let Some(status) = get("2.1.0") {
        samples.push(Sample::metric(
            "battery_low",
            f64::from(u8::from(status == 3.0 || status == 4.0)),
        ));
    }
    samples
}

/// ENTITY-SENSOR-MIB sensors of type celsius (8), named by
/// entPhysicalName when there is one.
pub fn temperature(sensors: &Table, names: &BTreeMap<String, Val>) -> Vec<Sample> {
    let mut samples = Vec::new();
    let Some(kinds) = sensors.get(&1) else {
        return samples;
    };
    for (index, kind) in kinds {
        if kind.number() != Some(8.0) {
            continue;
        }
        // entPhySensorOperStatus 1: ok.
        if number(sensors, 5, index).is_some_and(|s| s != 1.0) {
            continue;
        }
        let Some(value) = number(sensors, 4, index) else {
            continue;
        };
        // Scale 9 is units, each step a factor of 1000; precision is
        // decimal places.
        let scale = number(sensors, 2, index).unwrap_or(9.0);
        let precision = number(sensors, 3, index).unwrap_or(0.0);
        let celsius = value * 10f64.powf((scale - 9.0) * 3.0) / 10f64.powf(precision);
        let name = names
            .get(index)
            .and_then(Val::text)
            .filter(|n| !n.trim().is_empty())
            .map(instance_name)
            .unwrap_or_else(|| format!("sensor {index}"));
        samples.push(Sample::metric("celsius", celsius).with_instance(name));
    }
    samples
}

/// Printer-MIB supplies: level over capacity, when both are known.
pub fn printer(supplies: &Table) -> Vec<Sample> {
    let mut samples = Vec::new();
    let Some(descrs) = supplies.get(&6) else {
        return samples;
    };
    for (index, descr) in descrs {
        let (Some(max), Some(level)) = (number(supplies, 8, index), number(supplies, 9, index))
        else {
            continue;
        };
        // -2 unknown, -3 "some left": no level to report.
        if max <= 0.0 || level < 0.0 {
            continue;
        }
        let name = instance_name(descr.text().unwrap_or(index));
        samples.push(
            Sample::metric("supply_level_ratio", (level / max).clamp(0.0, 1.0)).with_instance(name),
        );
    }
    samples
}

/// What "Add a device" learns: the system's description and name, and
/// which groups answer.
pub fn detect(descr: &str, name: &str, object_id: &str, has: &[(Group, bool)]) -> Vec<Sample> {
    let label = |text: &str| instance_name(text);
    let mut samples = vec![Sample::metric("answers", 1.0)
        .with_label("descr", label(descr))
        .with_label("name", label(name))
        .with_label("object_id", label(object_id))];
    for (group, present) in has {
        samples.push(
            Sample::metric("has", f64::from(u8::from(*present))).with_instance(group.as_str()),
        );
    }
    samples
}
