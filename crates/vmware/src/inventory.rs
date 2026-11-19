//! The objects of `RetrievePropertiesEx` as the shared model of
//! `monitorflow_sensors::virt`.

use std::collections::HashMap;

use monitorflow_sensors::virt::{Alarms, Cluster, Datastore, Host, Status, Vm};

use crate::xml::Element;

/// A whole inventory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inventory {
    pub vms: Vec<Vm>,
    pub hosts: Vec<Host>,
    pub clusters: Vec<Cluster>,
    pub datastores: Vec<Datastore>,
}

/// One object: its type, managed object ID and properties by path.
struct Object<'a> {
    kind: &'a str,
    key: String,
    properties: HashMap<&'a str, &'a Element>,
}

impl<'a> Object<'a> {
    fn read(element: &'a Element) -> Option<Self> {
        let obj = element.child("obj")?;
        let mut properties = HashMap::new();
        for set in element.all("propSet") {
            if let (Some(name), Some(value)) = (set.child("name"), set.child("val")) {
                properties.insert(name.text.trim(), value);
            }
        }
        Some(Self {
            kind: obj.attribute("type")?,
            key: obj.text.trim().to_owned(),
            properties,
        })
    }

    fn get(&self, path: &str) -> Option<&'a Element> {
        self.properties.get(path).copied()
    }

    fn text(&self, path: &str) -> Option<String> {
        self.get(path)
            .map(|v| v.text.trim().to_owned())
            .filter(|t| !t.is_empty())
    }

    fn number(&self, path: &str) -> Option<f64> {
        self.text(path)?.parse().ok()
    }

    fn flag(&self, path: &str) -> bool {
        self.text(path).as_deref() == Some("true")
    }

    fn status(&self, path: &str) -> Option<Status> {
        Status::from_color(&self.text(path)?)
    }

    /// Triggered alarms, red as critical and yellow as warning.
    fn alarms(&self) -> Option<Alarms> {
        let value = self.get("triggeredAlarmState")?;
        let mut alarms = Alarms::default();
        for state in value.all("AlarmState") {
            match state.text_of("overallStatus") {
                Some("red") => alarms.critical += 1,
                Some("yellow") => alarms.warning += 1,
                _ => {}
            }
        }
        Some(alarms)
    }
}

fn number(element: &Element, name: &str) -> Option<f64> {
    element.text_of(name)?.parse().ok()
}

fn ratio(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0.0 => Some((used / total).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Snapshot times of a snapshot tree, in Unix milliseconds.
fn snapshots(trees: &Element, out: &mut Vec<i64>) {
    for tree in trees
        .children
        .iter()
        .filter(|c| c.name == "VirtualMachineSnapshotTree" || c.name == "childSnapshotList")
    {
        if let Some(time) = tree
            .text_of("createTime")
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        {
            out.push(time.timestamp_millis());
        }
        snapshots(tree, out);
    }
}

/// Reads the objects of a collection. Templates are left out.
pub fn read(objects: &[Element]) -> Inventory {
    let objects: Vec<Object<'_>> = objects.iter().filter_map(Object::read).collect();
    // The first object of each key, as a search in order would find.
    let mut by_key: HashMap<&str, &Object<'_>> = HashMap::new();
    for object in &objects {
        by_key.entry(object.key.as_str()).or_insert(object);
    }
    let name_of = |key: &str| by_key.get(key).and_then(|o| o.text("name"));
    let mut inventory = Inventory::default();
    for object in &objects {
        let key = object.key.clone();
        let name = object.text("name").unwrap_or_else(|| key.clone());
        match object.kind {
            "VirtualMachine" => {
                if object.flag("config.template") {
                    continue;
                }
                let power_state = object.text("runtime.powerState").unwrap_or_default();
                let memory_mb = object.number("summary.config.memorySizeMB");
                let committed = object.number("summary.storage.committed");
                let uncommitted = object.number("summary.storage.uncommitted");
                let mut addresses = Vec::new();
                let mut macs = Vec::new();
                if let Some(net) = object.get("guest.net") {
                    for nic in net.all("GuestNicInfo") {
                        if let Some(mac) = nic.text_of("macAddress").filter(|m| !m.is_empty()) {
                            macs.push(mac.to_ascii_lowercase());
                        }
                        for ip in nic.all("ipAddress") {
                            let ip = ip.text.trim();
                            if !ip.is_empty() && !addresses.iter().any(|a| a == ip) {
                                addresses.push(ip.to_owned());
                            }
                        }
                    }
                }
                if let Some(ip) = object.text("guest.ipAddress") {
                    if !addresses.contains(&ip) {
                        addresses.insert(0, ip);
                    }
                }
                let mut times = Vec::new();
                if let Some(tree) = object.get("snapshot.rootSnapshotList") {
                    snapshots(tree, &mut times);
                }
                let powered_on = power_state == "poweredOn";
                inventory.vms.push(Vm {
                    uuid: object.text("config.uuid").map(|u| u.to_ascii_lowercase()),
                    host: object.text("runtime.host").and_then(|h| name_of(&h)),
                    powered_on,
                    power_state,
                    cpu_count: object.number("summary.config.numCpu"),
                    cpu_usage_ratio: powered_on
                        .then(|| {
                            ratio(
                                object.number("summary.quickStats.overallCpuUsage"),
                                object.number("summary.runtime.maxCpuUsage"),
                            )
                        })
                        .flatten(),
                    memory_bytes: memory_mb.map(|m| m * 1024.0 * 1024.0),
                    memory_usage_ratio: powered_on
                        .then(|| {
                            ratio(
                                object.number("summary.quickStats.guestMemoryUsage"),
                                memory_mb,
                            )
                        })
                        .flatten(),
                    storage_used_bytes: committed,
                    storage_provisioned_bytes: committed.map(|c| c + uncommitted.unwrap_or(0.0)),
                    snapshots: times,
                    status: object.status("summary.overallStatus"),
                    alarms: object.alarms(),
                    guest_hostname: object.text("guest.hostName"),
                    guest_os: object.text("summary.config.guestFullName"),
                    addresses,
                    macs,
                    replication: None,
                    key,
                    name,
                });
            }
            "HostSystem" => {
                let hardware = object.get("summary.hardware");
                let stats = object.get("summary.quickStats");
                let cores = hardware.and_then(|h| number(h, "numCpuCores"));
                let mhz = hardware.and_then(|h| number(h, "cpuMhz"));
                let memory = hardware.and_then(|h| number(h, "memorySize"));
                let parent = object.get("parent");
                let cluster = parent
                    .filter(|p| p.attribute("type") == Some("ClusterComputeResource"))
                    .and_then(|p| name_of(p.text.trim()));
                let hardware_status = object
                    .get("runtime.healthSystemRuntime.systemHealthInfo.numericSensorInfo")
                    .and_then(|sensors| {
                        sensors
                            .all("HostNumericSensorInfo")
                            .filter_map(|s| {
                                s.path(&["healthState", "key"])
                                    .and_then(|k| Status::from_color(k.text.trim()))
                            })
                            .max()
                    });
                inventory.hosts.push(Host {
                    cluster,
                    connection_state: object.text("runtime.connectionState").unwrap_or_default(),
                    powered_on: object.text("runtime.powerState").as_deref() == Some("poweredOn"),
                    maintenance: object.flag("runtime.inMaintenanceMode"),
                    cpu_usage_ratio: ratio(
                        stats.and_then(|s| number(s, "overallCpuUsage")),
                        mhz.zip(cores).map(|(m, c)| m * c),
                    ),
                    memory_usage_ratio: ratio(
                        stats
                            .and_then(|s| number(s, "overallMemoryUsage"))
                            .map(|m| m * 1024.0 * 1024.0),
                        memory,
                    ),
                    memory_bytes: memory,
                    cpu_cores: cores,
                    status: object.status("summary.overallStatus"),
                    hardware_status,
                    alarms: object.alarms(),
                    product: object.text("summary.config.product.fullName"),
                    vendor: hardware
                        .and_then(|h| h.text_of("vendor"))
                        .map(str::to_owned),
                    model: hardware.and_then(|h| h.text_of("model")).map(str::to_owned),
                    key,
                    name,
                });
            }
            "ClusterComputeResource" => inventory.clusters.push(Cluster {
                hosts: object.number("summary.numHosts"),
                hosts_effective: object.number("summary.numEffectiveHosts"),
                status: object.status("overallStatus"),
                alarms: object.alarms(),
                key,
                name,
            }),
            "Datastore" => inventory.datastores.push(Datastore {
                kind: object.text("summary.type"),
                accessible: object.flag("summary.accessible"),
                capacity_bytes: object.number("summary.capacity"),
                free_bytes: object.number("summary.freeSpace"),
                status: object.status("overallStatus"),
                alarms: object.alarms(),
                key,
                name,
            }),
            _ => {}
        }
    }
    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml;

    /// An answer of vcsim (govmomi's simulator) after `govc
    /// snapshot.create` twice on DC0_H0_VM0, `govc host.disconnect` of
    /// DC0_C0_H1 and a 5100 GB disk on LocalDS_1; host sensors cut to
    /// three, one turned yellow, and a red alarm added to vm-77.
    fn recorded() -> Inventory {
        let root = xml::parse(include_str!("../tests/fixtures/inventory.xml")).unwrap();
        let value = root.find("returnval").unwrap();
        let objects: Vec<Element> = value.all("objects").cloned().collect();
        read(&objects)
    }

    #[test]
    fn a_recorded_inventory_reads() {
        let inventory = recorded();
        assert_eq!(inventory.hosts.len(), 5);
        assert_eq!(inventory.clusters.len(), 2);
        assert_eq!(inventory.datastores.len(), 2);
        assert_eq!(inventory.vms.len(), 7);

        let vm = inventory
            .vms
            .iter()
            .find(|v| v.name == "DC0_H0_VM0")
            .unwrap();
        assert_eq!(vm.key, "vm-77");
        assert_eq!(
            vm.uuid.as_deref(),
            Some("63d40cc5-9cba-5cc1-884a-f7f19070ecea")
        );
        assert_eq!(vm.host.as_deref(), Some("DC0_H0"));
        assert!(vm.powered_on);
        assert_eq!(vm.snapshots.len(), 2);
        assert_eq!(vm.macs, ["00:0c:29:63:65:61"]);
        assert_eq!(vm.memory_bytes, Some(32.0 * 1024.0 * 1024.0));
        assert_eq!(
            vm.alarms,
            Some(Alarms {
                critical: 1,
                warning: 0
            })
        );
        assert_eq!(vm.status, Some(Status::Ok));

        let big = inventory.vms.iter().find(|v| v.name == "bigvm").unwrap();
        assert!(!big.powered_on && big.cpu_usage_ratio.is_none());
        assert!(big.snapshots.is_empty());

        let down = inventory
            .hosts
            .iter()
            .find(|h| h.name == "DC0_C0_H1")
            .unwrap();
        assert_eq!(down.connection_state, "disconnected");
        assert_eq!(down.cluster.as_deref(), Some("DC0_C0"));
        let standalone = inventory.hosts.iter().find(|h| h.name == "DC0_H0").unwrap();
        assert_eq!(standalone.cluster, None);
        assert!(standalone
            .cpu_usage_ratio
            .is_some_and(|r| r > 0.0 && r < 1.0));
        assert!(standalone
            .product
            .as_deref()
            .unwrap()
            .starts_with("VMware ESXi"));
        assert_eq!(
            inventory
                .hosts
                .iter()
                .filter_map(|h| h.hardware_status)
                .max(),
            Some(Status::Warning)
        );

        let full = inventory
            .datastores
            .iter()
            .find(|d| d.name == "LocalDS_1")
            .unwrap();
        let free = full.free_bytes.unwrap() / full.capacity_bytes.unwrap();
        assert!(free < 0.01, "{free}");
        assert!(full.accessible);
        let cluster = &inventory.clusters[0];
        assert_eq!(cluster.hosts, Some(2.0));
    }

    #[test]
    fn templates_are_left_out() {
        let objects = vec![xml::parse(
            r#"<objects><obj type="VirtualMachine">vm-9</obj><propSet><name>name</name><val>tpl</val></propSet><propSet><name>config.template</name><val>true</val></propSet></objects>"#,
        )
        .unwrap()];
        assert!(read(&objects).vms.is_empty());
    }
}
