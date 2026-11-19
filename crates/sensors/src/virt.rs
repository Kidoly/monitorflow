//! Virtual machines, hosts, clusters and datastores as samples (ADR-0156),
//! written alike by VMware checks of a probe and Hyper-V checks of a
//! Windows agent, and read back by the server into its inventory.
//!
//! Each object gives one `<kind>_info` sample of value 1 whose labels
//! describe it, and numeric samples; all of them carry the [`LABEL`]
//! label with the object's kind and are instances named after the object
//! ([`instances`]). A value that is not known is not written.

use std::collections::{BTreeMap, HashMap};

use crate::Sample;

/// The label naming the kind of object a sample is about.
pub const LABEL: &str = "virt";
/// The label holding an object's own ID at its source (managed object ID,
/// Hyper-V VM ID).
pub const KEY: &str = "key";
/// Longest label value the server accepts (ADR-0013).
pub const MAX_LABEL: usize = 256;

/// The kinds of objects.
pub const KINDS: [&str; 4] = ["vm", "host", "cluster", "datastore"];
/// The [`LABEL`] of the source's own `up` sample: the vCenter, ESXi or
/// Hyper-V host as a whole.
pub const SOURCE: &str = "source";

/// The source's `up`: 1 when it was read, 0 with `reason` and `error`.
/// `labels` describe what answered (product, version).
pub fn up(failure: Option<(&str, &str)>, labels: &[(&str, &str)], now_ms: i64) -> Sample {
    let mut sample =
        Sample::metric("up", if failure.is_some() { 0.0 } else { 1.0 }).with_label(LABEL, SOURCE);
    if let Some((reason, error)) = failure {
        sample.labels.insert("reason".into(), cut(reason));
        sample.labels.insert("error".into(), cut(error));
    }
    for (name, value) in labels {
        let value = cut(value);
        if !value.is_empty() {
            sample.labels.insert((*name).to_owned(), value);
        }
    }
    sample.timestamp_unix_ms = now_ms;
    sample
}

/// An overall status as hypervisors report it: green, yellow, red.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Ok = 0,
    Warning = 1,
    Critical = 2,
}

impl Status {
    /// vSphere's `ManagedEntityStatus`; `gray` and unknown words are
    /// `None`.
    pub fn from_color(color: &str) -> Option<Self> {
        match color {
            "green" => Some(Self::Ok),
            "yellow" => Some(Self::Warning),
            "red" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// Alarms triggered on one object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Alarms {
    pub critical: u32,
    pub warning: u32,
}

/// A virtual machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vm {
    pub key: String,
    pub name: String,
    /// The SMBIOS UUID the guest reads, lower case.
    pub uuid: Option<String>,
    /// The name of the host running it.
    pub host: Option<String>,
    /// As the hypervisor says it: `poweredOn`, `Running`, `Off`.
    pub power_state: String,
    pub powered_on: bool,
    pub cpu_count: Option<f64>,
    pub cpu_usage_ratio: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub memory_usage_ratio: Option<f64>,
    pub storage_used_bytes: Option<f64>,
    pub storage_provisioned_bytes: Option<f64>,
    /// When each snapshot was taken, in Unix milliseconds.
    pub snapshots: Vec<i64>,
    pub status: Option<Status>,
    pub alarms: Option<Alarms>,
    pub guest_hostname: Option<String>,
    pub guest_os: Option<String>,
    pub addresses: Vec<String>,
    pub macs: Vec<String>,
    /// Hyper-V replication: health, mode and state.
    pub replication: Option<Replication>,
}

/// Hyper-V replication of one virtual machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replication {
    pub health: Status,
    pub mode: String,
    pub state: String,
}

/// A hypervisor host of a vCenter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Host {
    pub key: String,
    pub name: String,
    pub cluster: Option<String>,
    /// `connected`, `disconnected` or `notResponding`.
    pub connection_state: String,
    pub powered_on: bool,
    pub maintenance: bool,
    pub cpu_usage_ratio: Option<f64>,
    pub memory_usage_ratio: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub cpu_cores: Option<f64>,
    pub status: Option<Status>,
    /// The worst of its hardware sensors.
    pub hardware_status: Option<Status>,
    pub alarms: Option<Alarms>,
    pub product: Option<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
}

/// A cluster of hosts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cluster {
    pub key: String,
    pub name: String,
    pub hosts: Option<f64>,
    pub hosts_effective: Option<f64>,
    pub status: Option<Status>,
    pub alarms: Option<Alarms>,
}

/// A datastore.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Datastore {
    pub key: String,
    pub name: String,
    pub kind: Option<String>,
    pub accessible: bool,
    pub capacity_bytes: Option<f64>,
    pub free_bytes: Option<f64>,
    pub status: Option<Status>,
    pub alarms: Option<Alarms>,
}

/// Instance names for `(key, name)` pairs of one kind: the name, or the
/// name and the key when two objects share it. Empty names take the key.
pub fn instances<'a>(
    objects: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> HashMap<String, String> {
    let objects: Vec<(&str, &str)> = objects.into_iter().collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for (_, name) in &objects {
        *count.entry(*name).or_default() += 1;
    }
    objects
        .into_iter()
        .map(|(key, name)| {
            let instance = if name.trim().is_empty() {
                key.to_owned()
            } else if count[name] > 1 {
                format!("{name} ({key})")
            } else {
                name.to_owned()
            };
            (key.to_owned(), cut(&instance))
        })
        .collect()
}

/// `value` cut to [`MAX_LABEL`] bytes on a character boundary, control
/// characters removed.
pub fn cut(value: &str) -> String {
    crate::validate::label_value(value)
}

/// As many of `values` as fit one label, comma separated.
pub fn list(values: &[String]) -> String {
    let mut out = String::new();
    for value in values {
        let extra = value.len() + usize::from(!out.is_empty());
        if value.is_empty() || value.contains(',') || out.len() + extra > MAX_LABEL {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(value);
    }
    out
}

/// Writes the samples of one object.
struct Writer<'a> {
    out: &'a mut Vec<Sample>,
    kind: &'static str,
    instance: String,
    now_ms: i64,
}

impl Writer<'_> {
    fn put(&mut self, metric: &str, value: Option<f64>) {
        if let Some(value) = value.filter(|v| v.is_finite()) {
            let mut sample = Sample::metric(format!("{}_{metric}", self.kind), value)
                .with_instance(self.instance.clone())
                .with_label(LABEL, self.kind);
            sample.timestamp_unix_ms = self.now_ms;
            self.out.push(sample);
        }
    }

    fn flag(&mut self, metric: &str, value: bool) {
        self.put(metric, Some(f64::from(u8::from(value))));
    }

    fn status(&mut self, metric: &str, status: Option<Status>) {
        self.put(metric, status.map(|s| s as u8 as f64));
    }

    fn alarms(&mut self, alarms: Option<Alarms>) {
        if let Some(alarms) = alarms {
            self.put("alarms_critical", Some(f64::from(alarms.critical)));
            self.put("alarms_warning", Some(f64::from(alarms.warning)));
        }
    }

    fn info(&mut self, key: &str, labels: &[(&str, Option<&str>)]) {
        let mut sample = Sample::metric(format!("{}_info", self.kind), 1.0)
            .with_instance(self.instance.clone())
            .with_label(LABEL, self.kind)
            .with_label(KEY, cut(key));
        for (name, value) in labels {
            if let Some(value) = value.map(cut).filter(|v| !v.is_empty()) {
                sample.labels.insert((*name).to_owned(), value);
            }
        }
        sample.timestamp_unix_ms = self.now_ms;
        self.out.push(sample);
    }
}

fn names<T>(items: &[T], f: impl Fn(&T) -> (&str, &str)) -> HashMap<String, String> {
    instances(items.iter().map(f))
}

/// The samples of a whole inventory, taken at `now_ms`.
pub fn samples(
    vms: &[Vm],
    hosts: &[Host],
    clusters: &[Cluster],
    datastores: &[Datastore],
    now_ms: i64,
) -> Vec<Sample> {
    let mut out = Vec::new();
    let named = names(vms, |v| (&v.key, &v.name));
    for vm in vms {
        let mut w = Writer {
            out: &mut out,
            kind: "vm",
            instance: named[&vm.key].clone(),
            now_ms,
        };
        let addresses = list(&vm.addresses);
        let macs = list(&vm.macs);
        let replication = vm.replication.as_ref();
        w.info(
            &vm.key,
            &[
                ("uuid", vm.uuid.as_deref()),
                ("host", vm.host.as_deref()),
                ("power_state", Some(&vm.power_state)),
                ("guest_hostname", vm.guest_hostname.as_deref()),
                ("guest_os", vm.guest_os.as_deref()),
                ("addresses", Some(&addresses)),
                ("macs", Some(&macs)),
                ("replication_mode", replication.map(|r| r.mode.as_str())),
                ("replication_state", replication.map(|r| r.state.as_str())),
            ],
        );
        w.flag("powered_on", vm.powered_on);
        w.put("cpu_count", vm.cpu_count);
        w.put("cpu_usage_ratio", vm.cpu_usage_ratio);
        w.put("memory_bytes", vm.memory_bytes);
        w.put("memory_usage_ratio", vm.memory_usage_ratio);
        w.put("storage_used_bytes", vm.storage_used_bytes);
        w.put("storage_provisioned_bytes", vm.storage_provisioned_bytes);
        w.put("snapshots", Some(vm.snapshots.len() as f64));
        let oldest = vm.snapshots.iter().min();
        w.put(
            "snapshot_age_seconds",
            oldest.map(|t| ((now_ms - t).max(0) / 1000) as f64),
        );
        w.status("status", vm.status);
        w.status("replication_health", replication.map(|r| r.health));
        w.alarms(vm.alarms);
    }
    let named = names(hosts, |h| (&h.key, &h.name));
    for host in hosts {
        let mut w = Writer {
            out: &mut out,
            kind: "host",
            instance: named[&host.key].clone(),
            now_ms,
        };
        w.info(
            &host.key,
            &[
                ("cluster", host.cluster.as_deref()),
                ("connection_state", Some(&host.connection_state)),
                (
                    "maintenance",
                    Some(if host.maintenance { "true" } else { "false" }),
                ),
                ("product", host.product.as_deref()),
                ("vendor", host.vendor.as_deref()),
                ("model", host.model.as_deref()),
            ],
        );
        w.flag("connected", host.connection_state == "connected");
        w.flag("maintenance", host.maintenance);
        w.flag("powered_on", host.powered_on);
        w.put("cpu_usage_ratio", host.cpu_usage_ratio);
        w.put("memory_usage_ratio", host.memory_usage_ratio);
        w.put("memory_bytes", host.memory_bytes);
        w.put("cpu_cores", host.cpu_cores);
        w.status("status", host.status);
        w.status("hardware_status", host.hardware_status);
        w.alarms(host.alarms);
    }
    let named = names(clusters, |c| (&c.key, &c.name));
    for cluster in clusters {
        let mut w = Writer {
            out: &mut out,
            kind: "cluster",
            instance: named[&cluster.key].clone(),
            now_ms,
        };
        w.info(&cluster.key, &[]);
        w.put("hosts", cluster.hosts);
        w.put("hosts_effective", cluster.hosts_effective);
        w.status("status", cluster.status);
        w.alarms(cluster.alarms);
    }
    let named = names(datastores, |d| (&d.key, &d.name));
    for ds in datastores {
        let mut w = Writer {
            out: &mut out,
            kind: "datastore",
            instance: named[&ds.key].clone(),
            now_ms,
        };
        w.info(&ds.key, &[("type", ds.kind.as_deref())]);
        w.flag("accessible", ds.accessible);
        w.put("capacity_bytes", ds.capacity_bytes);
        w.put("free_bytes", ds.free_bytes);
        let ratio = match (ds.free_bytes, ds.capacity_bytes) {
            (Some(free), Some(capacity)) if capacity > 0.0 => Some(free / capacity),
            _ => None,
        };
        w.put("free_ratio", ratio);
        w.status("status", ds.status);
        w.alarms(ds.alarms);
    }
    out
}

/// The labels of an `_info` sample other than [`LABEL`] and [`KEY`], for
/// readers of the inventory.
pub fn details(sample_labels: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    sample_labels
        .iter()
        .filter(|(k, _)| k.as_str() != LABEL && k.as_str() != KEY)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(samples: &'a [Sample], sensor: &str, instance: &str) -> Option<&'a Sample> {
        samples
            .iter()
            .find(|s| s.sensor == sensor && s.instance == instance)
    }

    #[test]
    fn up_says_why() {
        let ok = up(
            None,
            &[("product", "VMware vCenter Server 8"), ("empty", "")],
            5,
        );
        assert_eq!((ok.sensor.as_str(), ok.value), ("up", 1.0));
        assert_eq!(ok.labels[LABEL], SOURCE);
        assert!(!ok.labels.contains_key("empty") && !ok.labels.contains_key("reason"));
        let down = up(Some(("login", "Login failure")), &[], 5);
        assert_eq!(down.value, 0.0);
        assert_eq!(down.labels["error"], "Login failure");
    }

    #[test]
    fn instances_are_names_unless_shared() {
        let named = instances([
            ("vm-1", "web"),
            ("vm-2", "web"),
            ("vm-3", "db"),
            ("vm-4", " "),
        ]);
        assert_eq!(named["vm-1"], "web (vm-1)");
        assert_eq!(named["vm-2"], "web (vm-2)");
        assert_eq!(named["vm-3"], "db");
        assert_eq!(named["vm-4"], "vm-4");
        assert_eq!(cut(&"é".repeat(200)).len(), 256);
        assert_eq!(cut("a\nb"), "ab");
    }

    #[test]
    fn lists_keep_what_fits_one_label() {
        let many: Vec<String> = (0..40).map(|i| format!("10.0.0.{i}")).collect();
        let joined = list(&many);
        assert!(joined.len() <= MAX_LABEL);
        assert!(joined.starts_with("10.0.0.0,10.0.0.1,"));
        assert_eq!(list(&["a,b".into(), "c".into()]), "c");
    }

    #[test]
    fn an_inventory_gives_info_and_measures() {
        let now = 1_800_000_000_000;
        let vm = Vm {
            key: "vm-42".into(),
            name: "web-01".into(),
            uuid: Some("4237aaaa-0000-0000-0000-000000000001".into()),
            host: Some("esx-01".into()),
            power_state: "poweredOn".into(),
            powered_on: true,
            cpu_count: Some(2.0),
            snapshots: vec![now - 8 * 86_400_000, now - 1000],
            status: Some(Status::Warning),
            alarms: Some(Alarms {
                critical: 1,
                warning: 0,
            }),
            macs: vec!["00:50:56:aa:bb:cc".into()],
            ..Vm::default()
        };
        let host = Host {
            key: "host-1".into(),
            name: "esx-01".into(),
            connection_state: "disconnected".into(),
            ..Host::default()
        };
        let ds = Datastore {
            key: "datastore-1".into(),
            name: "ds1".into(),
            accessible: true,
            capacity_bytes: Some(100.0),
            free_bytes: Some(4.0),
            ..Datastore::default()
        };
        let samples = samples(&[vm], &[host], &[], &[ds], now);
        let info = find(&samples, "vm_info", "web-01").unwrap();
        assert_eq!(info.labels[LABEL], "vm");
        assert_eq!(info.labels[KEY], "vm-42");
        assert_eq!(info.labels["macs"], "00:50:56:aa:bb:cc");
        assert!(!info.labels.contains_key("guest_os"), "unknown left out");
        assert_eq!(details(&info.labels).len(), 4);
        assert_eq!(
            find(&samples, "vm_snapshot_age_seconds", "web-01")
                .unwrap()
                .value,
            (8 * 86_400) as f64
        );
        assert_eq!(find(&samples, "vm_snapshots", "web-01").unwrap().value, 2.0);
        assert_eq!(find(&samples, "vm_status", "web-01").unwrap().value, 1.0);
        assert_eq!(
            find(&samples, "vm_alarms_critical", "web-01")
                .unwrap()
                .value,
            1.0
        );
        assert!(find(&samples, "vm_cpu_usage_ratio", "web-01").is_none());
        assert_eq!(
            find(&samples, "host_connected", "esx-01").unwrap().value,
            0.0
        );
        assert_eq!(
            find(&samples, "datastore_free_ratio", "ds1").unwrap().value,
            0.04
        );
        assert!(samples.iter().all(|s| s.labels.contains_key(LABEL)));
        assert!(samples.iter().all(|s| s.timestamp_unix_ms == now));
    }
}
