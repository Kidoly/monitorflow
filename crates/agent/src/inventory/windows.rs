//! The inventory of a Windows host (ADR-0187): [`SCRIPT`], built into the
//! agent and taking nothing from the server, reads CIM (WMI) classes and
//! the Uninstall keys of the registry, read-only, and prints one JSON
//! document that [`parse`] reads on every platform. Installed programs
//! come from the registry, never from `Win32_Product`, which checks and
//! may repair every MSI package it lists.

use monitorflow_common::inventory::Items;
use monitorflow_common::proto::v1::Inventory;
use serde_json::{Map, Value};

use super::clean_serial;

/// The script the agent runs with Windows PowerShell 5.1. A class that
/// cannot be read leaves its part empty.
pub const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
function S($v) { if ($null -eq $v) { '' } else { "$v".Trim() } }
function D($v) { if ($v -is [datetime]) { $v.ToString('yyyy-MM-dd') } else { '' } }
$cs = Get-CimInstance Win32_ComputerSystem
$bios = Get-CimInstance Win32_BIOS
$board = Get-CimInstance Win32_BaseBoard | Select-Object -First 1
$enclosure = Get-CimInstance Win32_SystemEnclosure | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$system = @{
  manufacturer = S $cs.Manufacturer; model = S $cs.Model; family = S $cs.SystemFamily; sku = S $cs.SystemSKUNumber
  serial = S $bios.SerialNumber; board_manufacturer = S $board.Manufacturer; board_model = S $board.Product
  board_serial = S $board.SerialNumber; chassis_serial = S $enclosure.SerialNumber
  bios_vendor = S $bios.Manufacturer; bios_version = S $bios.SMBIOSBIOSVersion; bios_date = D $bios.ReleaseDate
  os = S $os.Caption; os_version = S $os.Version; architecture = S $os.OSArchitecture
  memory_bytes = S $cs.TotalPhysicalMemory
}
$processors = @(Get-CimInstance Win32_Processor | ForEach-Object {
  @{ key = S $_.DeviceID; model = S $_.Name; cores = S $_.NumberOfCores; threads = S $_.NumberOfLogicalProcessors
     max_mhz = S $_.MaxClockSpeed; socket = S $_.SocketDesignation } })
$memory = @(Get-CimInstance Win32_PhysicalMemory | ForEach-Object {
  @{ key = S $_.DeviceLocator; bank = S $_.BankLabel; size_bytes = S $_.Capacity; speed_mhz = S $_.Speed
     manufacturer = S $_.Manufacturer; part_number = S $_.PartNumber; serial = S $_.SerialNumber } })
$disks = @(Get-CimInstance Win32_DiskDrive | ForEach-Object {
  @{ key = "disk$($_.Index)"; model = S $_.Model; serial = S $_.SerialNumber; size_bytes = S $_.Size
     firmware = S $_.FirmwareRevision; transport = S $_.InterfaceType } })
$nics = @(Get-CimInstance Win32_NetworkAdapter -Filter 'PhysicalAdapter=TRUE' | ForEach-Object {
  $config = Get-CimInstance Win32_NetworkAdapterConfiguration -Filter "Index=$($_.Index)"
  $name = S $_.NetConnectionID; if (-not $name) { $name = S $_.Name }
  @{ key = $name; model = S $_.Name; manufacturer = S $_.Manufacturer; mac = S $_.MACAddress
     speed = S $_.Speed; addresses = (@($config.IPAddress) | Where-Object { $_ }) -join ', ' } })
$software = New-Object System.Collections.ArrayList
foreach ($path in 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
                  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*') {
  foreach ($p in @(Get-ItemProperty -Path $path)) {
    if (-not $p.DisplayName -or $p.SystemComponent -eq 1 -or $p.ParentKeyName) { continue }
    [void]$software.Add(@{ key = S $p.DisplayName; version = S $p.DisplayVersion; publisher = S $p.Publisher
                            installed_on = S $p.InstallDate })
  }
}
$services = @(Get-CimInstance Win32_Service | ForEach-Object {
  @{ key = S $_.Name; display_name = S $_.DisplayName; start = S $_.StartMode } })
$features = @(Get-CimInstance Win32_OptionalFeature -Filter 'InstallState=1' | ForEach-Object {
  @{ key = S $_.Name; display_name = S $_.Caption } })
$features += @(Get-CimInstance Win32_ServerFeature | ForEach-Object { @{ key = S $_.Name; display_name = S $_.Name } })
$licences = @(Get-CimInstance SoftwareLicensingProduct -Filter 'PartialProductKey IS NOT NULL' | ForEach-Object {
  @{ key = S $_.Name; description = S $_.Description; status = S $_.LicenseStatus; partial_key = S $_.PartialProductKey } })
$out = @{ system = $system; processors = $processors; memory = $memory; disks = $disks; nics = $nics
          software = @($software); services = $services; features = $features; licences = $licences }
[Console]::Out.Write((ConvertTo-Json -InputObject $out -Depth 4 -Compress))
"#;

/// The script's JSON as an inventory; an unreadable document gives an
/// empty one.
pub fn parse(json: &str) -> Inventory {
    let mut items = Items::new();
    let Ok(Value::Object(doc)) = serde_json::from_str::<Value>(json.trim_start_matches('\u{feff}'))
    else {
        return items.finish();
    };
    if let Some(Value::Object(system)) = doc.get("system") {
        let mut fields = Vec::new();
        for name in [
            "manufacturer",
            "model",
            "family",
            "sku",
            "board_manufacturer",
            "board_model",
            "bios_vendor",
            "bios_version",
            "bios_date",
            "os",
            "os_version",
            "architecture",
            "memory_bytes",
        ] {
            fields.push((name, text(system, name)));
        }
        for name in ["serial", "board_serial", "chassis_serial"] {
            fields.push((name, clean_serial(&text(system, name)).unwrap_or_default()));
        }
        items.push("system", "system", fields);
    }
    for row in list(&doc, "processors") {
        items.push(
            "processor",
            text(row, "key"),
            [
                ("model", text(row, "model")),
                ("cores", text(row, "cores")),
                ("threads", text(row, "threads")),
                ("max_mhz", text(row, "max_mhz")),
                ("socket", text(row, "socket")),
            ],
        );
    }
    for row in list(&doc, "memory") {
        items.push(
            "memory",
            text(row, "key"),
            [
                ("bank", text(row, "bank")),
                ("size_bytes", text(row, "size_bytes")),
                ("speed_mhz", text(row, "speed_mhz")),
                ("manufacturer", text(row, "manufacturer")),
                ("part_number", text(row, "part_number")),
                (
                    "serial",
                    clean_serial(&text(row, "serial")).unwrap_or_default(),
                ),
            ],
        );
    }
    for row in list(&doc, "disks") {
        items.push(
            "disk",
            text(row, "key"),
            [
                ("model", text(row, "model")),
                (
                    "serial",
                    clean_serial(&text(row, "serial")).unwrap_or_default(),
                ),
                ("size_bytes", text(row, "size_bytes")),
                ("firmware", text(row, "firmware")),
                ("transport", text(row, "transport")),
            ],
        );
    }
    for row in list(&doc, "nics") {
        // Bits per second; Windows says 2^63-1 when it does not know.
        let speed = text(row, "speed")
            .parse::<u64>()
            .ok()
            .filter(|bps| *bps > 0 && *bps < 10_000_000_000_000)
            .map(|bps| (bps / 1_000_000).to_string())
            .unwrap_or_default();
        items.push(
            "nic",
            text(row, "key"),
            [
                ("model", text(row, "model")),
                ("manufacturer", text(row, "manufacturer")),
                ("mac", text(row, "mac").to_ascii_lowercase()),
                ("speed_mbps", speed),
                ("addresses", text(row, "addresses")),
            ],
        );
    }
    for row in list(&doc, "software") {
        items.push(
            "software",
            text(row, "key"),
            [
                ("version", text(row, "version")),
                ("publisher", text(row, "publisher")),
                ("installed_on", install_date(&text(row, "installed_on"))),
            ],
        );
    }
    for row in list(&doc, "services") {
        items.push(
            "service",
            text(row, "key"),
            [
                ("display_name", text(row, "display_name")),
                ("start", text(row, "start").to_ascii_lowercase()),
            ],
        );
    }
    for row in list(&doc, "features") {
        items.push(
            "feature",
            text(row, "key"),
            [("display_name", text(row, "display_name"))],
        );
    }
    for row in list(&doc, "licences") {
        items.push(
            "licence",
            text(row, "key"),
            [
                ("description", text(row, "description")),
                ("status", licence_status(&text(row, "status")).to_owned()),
                ("partial_key", text(row, "partial_key")),
            ],
        );
    }
    items.finish()
}

/// The objects of the array `name`; PowerShell writes a lone object
/// without its array at times.
fn list<'a>(doc: &'a Map<String, Value>, name: &str) -> Vec<&'a Map<String, Value>> {
    match doc.get(name) {
        Some(Value::Array(rows)) => rows.iter().filter_map(Value::as_object).collect(),
        Some(Value::Object(row)) => vec![row],
        _ => Vec::new(),
    }
}

fn text(row: &Map<String, Value>, name: &str) -> String {
    match row.get(name) {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `20240115` as `2024-01-15`; anything else as it is.
fn install_date(text: &str) -> String {
    if text.len() == 8 && text.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &text[..4], &text[4..6], &text[6..])
    } else {
        text.to_owned()
    }
}

/// `SoftwareLicensingProduct.LicenseStatus` in words.
fn licence_status(code: &str) -> &'static str {
    match code {
        "0" => "unlicensed",
        "1" => "licensed",
        "2" | "3" | "6" => "grace",
        "4" => "non-genuine grace",
        "5" => "notification",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::inventory::fields;

    use super::*;

    const FIXTURE: &str = r#"{"system":{"manufacturer":"Dell Inc.","model":"PowerEdge R650","family":"PowerEdge","sku":"SKU=0A8C","serial":"7XK2LM3","board_manufacturer":"Dell Inc.","board_model":"0DY2X0","board_serial":"Default string","chassis_serial":"7XK2LM3","bios_vendor":"Dell Inc.","bios_version":"1.10.2","bios_date":"2023-05-04","os":"Microsoft Windows Server 2022 Standard","os_version":"10.0.20348","architecture":"64-bit","memory_bytes":"68719476736"},
"processors":{"key":"CPU0","model":"Intel(R) Xeon(R) Gold 6338 CPU @ 2.00GHz","cores":"32","threads":"64","max_mhz":"2000","socket":"CPU1"},
"memory":[{"key":"A1","bank":"","size_bytes":34359738368,"speed_mhz":"3200","manufacturer":"Hynix","part_number":"HMA84GR7CJR4N-XN","serial":"3A1B2C3D"}],
"disks":[{"key":"disk0","model":"DELL PERC H755 SCSI Disk Device","serial":"0012ab","size_bytes":"959656755200","firmware":"5.16","transport":"SCSI"}],
"nics":[{"key":"Ethernet","model":"Broadcom NetXtreme Gigabit Ethernet","manufacturer":"Broadcom","mac":"B0:7B:25:00:11:22","speed":"1000000000","addresses":"192.0.2.10, fe80::1"},{"key":"Ethernet 2","model":"Broadcom","manufacturer":"","mac":"","speed":"9223372036854775807","addresses":""}],
"software":[{"key":"7-Zip 24.08 (x64)","version":"24.08","publisher":"Igor Pavlov","installed_on":"20240911"},{"key":"","version":"1"}],
"services":[{"key":"wuauserv","display_name":"Windows Update","start":"Manual"}],
"features":[{"key":"NetFx4","display_name":".NET Framework 4.8"}],
"licences":[{"key":"Windows(R), ServerStandard edition","description":"Windows(R) Operating System, VOLUME_KMSCLIENT channel","status":"1","partial_key":"XXXXX"}]}"#;

    #[test]
    fn the_script_output_is_read() {
        let inventory = parse(FIXTURE);
        let by = |kind: &str, key: &str| {
            inventory
                .items
                .iter()
                .find(|i| i.kind == kind && i.key == key)
                .unwrap_or_else(|| panic!("{kind} {key}"))
        };
        let system = fields(by("system", "system"));
        assert_eq!(system["model"], "PowerEdge R650");
        assert_eq!(system["serial"], "7XK2LM3");
        assert!(!system.contains_key("board_serial"), "placeholder dropped");
        assert_eq!(fields(by("processor", "CPU0"))["cores"], "32");
        assert_eq!(fields(by("memory", "A1"))["size_bytes"], "34359738368");
        let nic = fields(by("nic", "Ethernet"));
        assert_eq!(nic["speed_mbps"], "1000");
        assert_eq!(nic["mac"], "b0:7b:25:00:11:22");
        assert!(!fields(by("nic", "Ethernet 2")).contains_key("speed_mbps"));
        assert_eq!(
            fields(by("software", "7-Zip 24.08 (x64)"))["installed_on"],
            "2024-09-11"
        );
        assert_eq!(fields(by("service", "wuauserv"))["start"], "manual");
        assert_eq!(
            fields(by("licence", "Windows(R), ServerStandard edition"))["status"],
            "licensed"
        );
        assert_eq!(by("feature", "NetFx4").kind, "feature");
        // The nameless program is dropped.
        assert_eq!(
            inventory
                .items
                .iter()
                .filter(|i| i.kind == "software")
                .count(),
            1
        );
    }

    #[test]
    fn garbage_gives_an_empty_inventory() {
        assert!(parse("").items.is_empty());
        assert!(parse("Get-CimInstance : Access denied").items.is_empty());
        assert!(parse("[]").items.is_empty());
    }
}
