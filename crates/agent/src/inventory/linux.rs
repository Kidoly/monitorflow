//! The inventory of a Linux host (ADR-0187), read as the agent's user:
//! `/sys/class/dmi/id`, `/proc`, `/sys/class/net` and `/etc/os-release`,
//! then `lsblk`, `ip` and `systemctl`. The DMI serial numbers are
//! readable by root only; the systemd unit copies them into the agent's
//! runtime directory before it starts, as it does the system UUID
//! (ADR-0093). Memory modules need root (`dmidecode`) and are left out:
//! the total is reported.
//!
//! Everything is parsed from text by plain functions, tested on fixtures.

use std::collections::BTreeMap;
use std::path::Path;

use monitorflow_common::inventory::Items;
use serde_json::Value;

use super::{clean_serial, command_output};

/// DMI fields read as they are, by file name and field name.
const DMI: [(&str, &str); 13] = [
    ("sys_vendor", "manufacturer"),
    ("product_name", "model"),
    ("product_family", "family"),
    ("product_sku", "sku"),
    ("product_version", "version"),
    ("board_vendor", "board_manufacturer"),
    ("board_name", "board_model"),
    ("chassis_vendor", "chassis_manufacturer"),
    ("chassis_type", "chassis"),
    ("bios_vendor", "bios_vendor"),
    ("bios_version", "bios_version"),
    ("bios_date", "bios_date"),
    ("bios_release", "bios_release"),
];

/// Root-only serial numbers, by file name and field name.
const DMI_SERIALS: [(&str, &str); 3] = [
    ("product_serial", "serial"),
    ("board_serial", "board_serial"),
    ("chassis_serial", "chassis_serial"),
];

/// SMBIOS chassis types (DSP0134, 7.4.1) people recognise.
fn chassis(code: &str) -> Option<&'static str> {
    Some(match code.trim().parse::<u8>().ok()? {
        3 => "desktop",
        4 => "low profile desktop",
        6 => "mini tower",
        7 => "tower",
        8 | 9 | 10 | 14 => "laptop",
        13 => "all in one",
        17 => "main server chassis",
        23 => "rack mount chassis",
        24 => "sealed-case PC",
        25 => "multi-system chassis",
        28 => "blade",
        29 => "blade enclosure",
        35 => "mini PC",
        36 => "stick PC",
        1 => "other",
        _ => return None,
    })
}

/// Collects the inventory of the host whose files are under `root`
/// (`/` in production), with the commands' output passed in.
pub fn collect_in(root: &Path, outputs: &Outputs) -> monitorflow_common::proto::v1::Inventory {
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let mut items = Items::new();

    // Inside a container the firmware is the host's.
    let in_container = ["run/systemd/container", "run/host/container-manager"]
        .iter()
        .any(|path| root.join(path).exists());
    let mut system: Vec<(&str, String)> = Vec::new();
    for (file, field) in DMI.iter().filter(|_| !in_container) {
        let (file, field) = (*file, *field);
        if let Some(value) = read(&format!("sys/class/dmi/id/{file}")) {
            let value = match file {
                "chassis_type" => chassis(&value).map(str::to_owned).unwrap_or_default(),
                "bios_date" => iso_date(&value),
                _ => value,
            };
            system.push((field, value));
        }
    }
    for (file, field) in DMI_SERIALS.iter().filter(|_| !in_container) {
        let (file, field) = (*file, *field);
        let value = read(&format!("run/monitorflow-agent/{file}"))
            .or_else(|| read(&format!("sys/class/dmi/id/{file}")));
        if let Some(serial) = value.as_deref().and_then(clean_serial) {
            system.push((field, serial));
        }
    }
    if let Some(release) = read("etc/os-release") {
        if let Some(name) = os_release(&release) {
            system.push(("os", name));
        }
    }
    if let Some(kernel) = read("proc/sys/kernel/osrelease") {
        system.push(("kernel", kernel));
    }
    system.push(("architecture", std::env::consts::ARCH.to_owned()));
    if let Some(bytes) = read("proc/meminfo").as_deref().and_then(mem_total) {
        system.push(("memory_bytes", bytes.to_string()));
    }
    items.push("system", "system", system);

    let max_mhz = read("sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .and_then(|khz| khz.parse::<u64>().ok())
        .map(|khz| (khz / 1000).to_string())
        .unwrap_or_default();
    for (socket, cpu) in processors(&read("proc/cpuinfo").unwrap_or_default()) {
        items.push(
            "processor",
            socket,
            [
                ("model", cpu.model),
                ("cores", cpu.cores.to_string()),
                ("threads", cpu.threads.to_string()),
                ("max_mhz", max_mhz.clone()),
            ],
        );
    }

    for disk in disks(&outputs.lsblk) {
        items.push("disk", disk.0, disk.1);
    }

    let addresses = addresses(&outputs.ip_addr);
    let net = root.join("sys/class/net");
    let mut names: Vec<String> = std::fs::read_dir(&net)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let dir = net.join(&name);
        // Physical cards, and virtual machines' emulated ones, have a
        // device; bridges, bonds, VLANs and veth pairs do not.
        if name == "lo" || !dir.join("device").exists() {
            continue;
        }
        let file = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .ok()
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        let driver = std::fs::read_link(dir.join("device/driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let speed = file("speed")
            .parse::<i64>()
            .ok()
            .filter(|s| *s > 0)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let (vendor, device) = (file("device/vendor"), file("device/device"));
        let pci = match (vendor.strip_prefix("0x"), device.strip_prefix("0x")) {
            (Some(v), Some(d)) => format!("{v}:{d}"),
            _ => String::new(),
        };
        items.push(
            "nic",
            name.clone(),
            [
                ("mac", file("address")),
                ("driver", driver),
                ("speed_mbps", speed),
                ("pci_id", pci),
                (
                    "addresses",
                    addresses.get(&name).cloned().unwrap_or_default().join(", "),
                ),
            ],
        );
    }

    for (unit, state) in services(&outputs.unit_files) {
        items.push("service", unit, [("start", state)]);
    }
    items.finish()
}

/// What the commands printed; empty when they could not run.
#[derive(Debug, Default)]
pub struct Outputs {
    /// `lsblk --json --bytes --nodeps --output NAME,TYPE,SIZE,MODEL,SERIAL,VENDOR,REV,ROTA,TRAN`.
    pub lsblk: String,
    /// `ip -json address show`.
    pub ip_addr: String,
    /// `systemctl list-unit-files --type=service --no-legend --plain`.
    pub unit_files: String,
}

impl Outputs {
    /// Runs the commands; `None` when one failed.
    pub async fn run() -> Option<Self> {
        let (lsblk, ip_addr, unit_files) = tokio::join!(
            command_output(
                "lsblk",
                &[
                    "--json",
                    "--bytes",
                    "--nodeps",
                    "--output",
                    "NAME,TYPE,SIZE,MODEL,SERIAL,VENDOR,REV,ROTA,TRAN",
                ],
            ),
            command_output("ip", &["-json", "address", "show"]),
            command_output(
                "systemctl",
                &[
                    "list-unit-files",
                    "--type=service",
                    "--no-legend",
                    "--plain",
                    "--no-pager",
                ],
            ),
        );
        Some(Self {
            lsblk: lsblk?,
            ip_addr: ip_addr?,
            unit_files: unit_files?,
        })
    }
}

/// DMI's `MM/DD/YYYY` as `YYYY-MM-DD`, as Windows gives it; anything
/// else as it is.
pub fn iso_date(text: &str) -> String {
    let parts: Vec<&str> = text.split('/').collect();
    match parts[..] {
        [m, d, y] if m.len() == 2 && d.len() == 2 && y.len() == 4 => format!("{y}-{m}-{d}"),
        _ => text.to_owned(),
    }
}

/// `PRETTY_NAME` of `/etc/os-release`, else `NAME VERSION_ID`.
pub fn os_release(text: &str) -> Option<String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim().trim_matches('"').trim_matches('\'');
            values.insert(key.trim(), value.to_owned());
        }
    }
    values.get("PRETTY_NAME").cloned().or_else(|| {
        let name = values.get("NAME")?;
        Some(match values.get("VERSION_ID") {
            Some(version) => format!("{name} {version}"),
            None => name.clone(),
        })
    })
}

/// `MemTotal` of `/proc/meminfo`, in bytes.
pub fn mem_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[derive(Debug, Default, PartialEq)]
pub struct Processor {
    pub model: String,
    pub cores: u32,
    pub threads: u32,
}

/// The processors of `/proc/cpuinfo`, by socket (`physical id`, `0` when
/// the file has none, as on most ARM machines).
pub fn processors(text: &str) -> BTreeMap<String, Processor> {
    let mut sockets: BTreeMap<String, Processor> = BTreeMap::new();
    for block in text.split("\n\n").filter(|b| b.contains("processor")) {
        let mut socket = "0".to_owned();
        let mut model = String::new();
        let mut cores = 0;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "physical id" => socket = value.to_owned(),
                "model name" | "Model" | "cpu model" if model.is_empty() => {
                    model = value.to_owned()
                }
                "cpu cores" => cores = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        let entry = sockets.entry(socket).or_default();
        entry.threads += 1;
        if entry.model.is_empty() {
            entry.model = model;
        }
        entry.cores = entry.cores.max(cores);
    }
    for processor in sockets.values_mut() {
        if processor.cores == 0 {
            processor.cores = processor.threads;
        }
    }
    sockets
}

/// The disks `lsblk` lists, with their fields. Loop, RAM and zram
/// devices and optical drives are left out.
pub fn disks(json: &str) -> Vec<(String, Vec<(&'static str, String)>)> {
    let Ok(doc) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let text = |device: &Value, name: &str| -> String {
        match device.get(name) {
            Some(Value::String(s)) => s.trim().to_owned(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => if *b { "1" } else { "0" }.to_owned(),
            _ => String::new(),
        }
    };
    doc.get("blockdevices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|d| text(d, "type") == "disk")
        .filter(|d| {
            let name = text(d, "name");
            !["loop", "ram", "zram", "sr"]
                .iter()
                .any(|p| name.starts_with(p))
        })
        .map(|d| {
            let media = match text(d, "rota").as_str() {
                "1" => "hdd",
                "0" => "ssd",
                _ => "",
            };
            (
                text(d, "name"),
                vec![
                    ("model", text(d, "model")),
                    ("vendor", text(d, "vendor")),
                    (
                        "serial",
                        clean_serial(&text(d, "serial")).unwrap_or_default(),
                    ),
                    ("size_bytes", text(d, "size")),
                    ("firmware", text(d, "rev")),
                    ("transport", text(d, "tran")),
                    ("media", media.to_owned()),
                ],
            )
        })
        .collect()
}

/// Global addresses by interface, from `ip -json address show`, like
/// `192.0.2.10/24`.
pub fn addresses(json: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Ok(Value::Array(links)) = serde_json::from_str::<Value>(json) else {
        return out;
    };
    for link in links {
        let Some(name) = link.get("ifname").and_then(Value::as_str) else {
            continue;
        };
        let list: Vec<String> = link
            .get("addr_info")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|a| a.get("scope").and_then(Value::as_str) == Some("global"))
            .filter_map(|a| {
                let local = a.get("local")?.as_str()?;
                let prefix = a.get("prefixlen")?.as_u64()?;
                Some(format!("{local}/{prefix}"))
            })
            .collect();
        out.insert(name.to_owned(), list);
    }
    out
}

/// Services from `systemctl list-unit-files`: enabled, disabled and
/// masked ones, by unit. Static, generated and template units start only
/// when something else asks, and are left out.
pub fn services(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let unit = words.next()?;
            let state = words.next()?;
            let kept = matches!(
                state,
                "enabled" | "enabled-runtime" | "disabled" | "masked" | "masked-runtime"
            );
            (kept && unit.ends_with(".service") && !unit.contains("@.")).then(|| {
                (
                    unit.to_owned(),
                    state.trim_end_matches("-runtime").to_owned(),
                )
            })
        })
        .collect()
}
