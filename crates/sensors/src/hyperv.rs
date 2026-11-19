//! Virtual machines of a Hyper-V host, read by its Windows agent
//! (ADR-0156).
//!
//! The sensor runs [`SCRIPT`], built into the agent and taking nothing
//! from the server, with Windows PowerShell 5.1 as AI commands run
//! (ADR-0084): `Get-VM`, `Get-VMSnapshot`, `Get-VMReplication`,
//! `Get-VMNetworkAdapter` and `Get-VMHardDiskDrive | Get-VHD` of the
//! Hyper-V module, and the SMBIOS UUID each guest reads from WMI
//! (`Msvm_VirtualSystemSettingData.BIOSGUID`). The script prints one JSON
//! document that [`parse`] reads on every platform, into the samples of
//! [`crate::virt`]: `vm_*` per virtual machine, checkpoints as snapshots,
//! `vm_replication_health`, and the source's `up`.
//!
//! Hyper-V answers only administrators and members of "Hyper-V
//! Administrators". The agent's account is neither until a person adds
//! it; until then `up` is 0 with reason `denied` and a message saying
//! how. The kind takes no parameter.

use serde::Deserialize;

use crate::virt::{self, Replication, Status, Vm};
use crate::Sample;

/// The script the agent runs. Errors of the module end in a JSON
/// `error`, never on stderr alone.
pub const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
function Out($value) { [Console]::Out.Write((ConvertTo-Json -InputObject $value -Depth 6 -Compress)) }
if (-not (Get-Module -ListAvailable -Name Hyper-V)) { Out @{ error = 'not_installed' }; exit 0 }
try { $vms = @(Get-VM) } catch { Out @{ error = 'denied'; message = "$($_.Exception.Message)" }; exit 0 }
$guids = @{}
try {
  Get-CimInstance -Namespace root\virtualization\v2 -ClassName Msvm_VirtualSystemSettingData -Filter "VirtualSystemType='Microsoft:Hyper-V:System:Realized'" |
    ForEach-Object { $guids["$($_.VirtualSystemIdentifier)".ToLower()] = "$($_.BIOSGUID)" }
} catch {}
$snapshots = @{}; $replicas = @{}; $nics = @{}; $disks = @{}
if ($vms.Count -gt 0) {
  foreach ($s in @(Get-VMSnapshot -VM $vms -ErrorAction SilentlyContinue)) {
    $id = "$($s.VMId)".ToLower()
    if (-not $snapshots[$id]) { $snapshots[$id] = New-Object System.Collections.ArrayList }
    [void]$snapshots[$id].Add(([DateTimeOffset]$s.CreationTime).ToUnixTimeMilliseconds())
  }
  foreach ($r in @(Get-VMReplication -ErrorAction SilentlyContinue)) {
    $replicas["$($r.VMId)".ToLower()] = @{ health = "$($r.Health)"; mode = "$($r.Mode)"; state = "$($r.State)" }
  }
  foreach ($n in @(Get-VMNetworkAdapter -VM $vms -ErrorAction SilentlyContinue)) {
    $id = "$($n.VMId)".ToLower()
    if (-not $nics[$id]) { $nics[$id] = @{ macs = New-Object System.Collections.ArrayList; ips = New-Object System.Collections.ArrayList } }
    if ($n.MacAddress) { [void]$nics[$id].macs.Add("$($n.MacAddress)") }
    foreach ($ip in @($n.IPAddresses)) { if ($ip) { [void]$nics[$id].ips.Add("$ip") } }
  }
  foreach ($d in @(Get-VMHardDiskDrive -VM $vms -ErrorAction SilentlyContinue)) {
    $id = "$($d.VMId)".ToLower()
    if (-not $d.Path) { continue }
    try { $v = Get-VHD -Path $d.Path } catch { continue }
    if (-not $disks[$id]) { $disks[$id] = @{ used = [int64]0; size = [int64]0 } }
    $disks[$id].used += [int64]$v.FileSize; $disks[$id].size += [int64]$v.Size
  }
}
$out = New-Object System.Collections.ArrayList
foreach ($vm in $vms) {
  $id = "$($vm.VMId)".ToLower()
  [void]$out.Add(@{
    id = $id; name = "$($vm.Name)"; state = "$($vm.State)"
    cpu_count = [int]$vm.ProcessorCount; cpu_usage = [int]$vm.CPUUsage
    memory_assigned = [int64]$vm.MemoryAssigned; memory_demand = [int64]$vm.MemoryDemand; memory_startup = [int64]$vm.MemoryStartup
    bios_guid = $guids[$id]; snapshots = @($snapshots[$id]); replication = $replicas[$id]
    macs = @($nics[$id].macs); addresses = @($nics[$id].ips); disk = $disks[$id]
  })
}
Out @{ host = "$env:COMPUTERNAME"; vms = @($out) }
"#;

#[derive(Debug, Deserialize)]
struct Report {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    vms: Vec<Machine>,
}

#[derive(Debug, Deserialize)]
struct Machine {
    id: String,
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    cpu_count: Option<f64>,
    #[serde(default)]
    cpu_usage: Option<f64>,
    #[serde(default)]
    memory_assigned: Option<f64>,
    #[serde(default)]
    memory_demand: Option<f64>,
    #[serde(default)]
    memory_startup: Option<f64>,
    #[serde(default)]
    bios_guid: Option<String>,
    #[serde(default, deserialize_with = "list")]
    snapshots: Vec<i64>,
    #[serde(default)]
    replication: Option<ReplicationReport>,
    #[serde(default, deserialize_with = "list")]
    macs: Vec<String>,
    #[serde(default, deserialize_with = "list")]
    addresses: Vec<String>,
    #[serde(default)]
    disk: Option<Disk>,
}

#[derive(Debug, Deserialize)]
struct ReplicationReport {
    #[serde(default)]
    health: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    state: String,
}

#[derive(Debug, Deserialize)]
struct Disk {
    used: f64,
    size: f64,
}

/// A list, which PowerShell may also give as `null` or as `[null]` when
/// a lookup found nothing.
fn list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let items: Option<Vec<Option<T>>> = Option::deserialize(deserializer)?;
    Ok(items.unwrap_or_default().into_iter().flatten().collect())
}

/// What a collection gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Read {
        host: String,
        vms: Vec<Vm>,
    },
    /// Why not, as `reason` and `error` of `up`.
    Failed {
        reason: &'static str,
        error: String,
    },
}

/// The message a person reads when the agent's account cannot read
/// Hyper-V.
pub const DENIED: &str = "the agent cannot read Hyper-V: run Add-LocalGroupMember -Group \"Hyper-V Administrators\" -Member \"NT SERVICE\\MonitorFlowAgent\" on this server, then restart the MonitorFlow agent";

/// `{XXXXXXXX-…}` as a lower-case UUID; `None` when it is not one.
fn uuid(guid: &str) -> Option<String> {
    let text = guid.trim().trim_start_matches('{').trim_end_matches('}');
    let ok = text.len() == 36
        && text.char_indices().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    ok.then(|| text.to_ascii_lowercase())
}

/// `00155D010203` as `00:15:5d:01:02:03`.
fn mac(text: &str) -> Option<String> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 || hex.chars().all(|c| c == '0') {
        return None;
    }
    let pairs: Vec<String> = (0..6)
        .map(|i| hex[2 * i..2 * i + 2].to_ascii_lowercase())
        .collect();
    Some(pairs.join(":"))
}

fn health(word: &str) -> Option<Status> {
    match word {
        "Normal" => Some(Status::Ok),
        "Warning" => Some(Status::Warning),
        "Critical" => Some(Status::Critical),
        _ => None,
    }
}

/// Reads the script's output.
pub fn parse(output: &str) -> Outcome {
    let output = output.trim_start_matches('\u{feff}').trim();
    let report: Report = match serde_json::from_str(output) {
        Ok(report) => report,
        Err(error) => {
            return Outcome::Failed {
                reason: "failed",
                error: format!("the Hyper-V script printed something unexpected: {error}"),
            }
        }
    };
    match report.error.as_deref() {
        None => {}
        Some("not_installed") => {
            return Outcome::Failed {
                reason: "not_installed",
                error: "Hyper-V is not installed on this server".into(),
            }
        }
        Some("denied") => {
            let detail = report.message.unwrap_or_default();
            let denied = detail.is_empty()
                || ["permission", "authoriz", "access", "privilege"]
                    .iter()
                    .any(|w| detail.to_ascii_lowercase().contains(w));
            return Outcome::Failed {
                reason: if denied { "denied" } else { "failed" },
                error: if denied {
                    DENIED.into()
                } else {
                    virt::cut(&format!("Get-VM failed: {detail}"))
                },
            };
        }
        Some(other) => {
            return Outcome::Failed {
                reason: "failed",
                error: virt::cut(&format!("the Hyper-V script failed: {other}")),
            }
        }
    }
    let host = report.host.unwrap_or_default();
    let vms = report
        .vms
        .into_iter()
        .map(|m| {
            let powered_on = matches!(m.state.as_str(), "Running" | "Paused" | "Saving");
            let memory = m.memory_assigned.filter(|b| *b > 0.0).or(m.memory_startup);
            let replication = m.replication.and_then(|r| {
                Some(Replication {
                    health: health(&r.health)?,
                    mode: r.mode,
                    state: r.state,
                })
            });
            Vm {
                uuid: m.bios_guid.as_deref().and_then(uuid),
                host: Some(host.clone()).filter(|h| !h.is_empty()),
                powered_on,
                cpu_count: m.cpu_count,
                cpu_usage_ratio: powered_on
                    .then(|| m.cpu_usage.map(|p| (p / 100.0).clamp(0.0, 1.0)))
                    .flatten(),
                memory_bytes: memory,
                memory_usage_ratio: match (m.memory_demand, m.memory_assigned) {
                    (Some(demand), Some(assigned)) if powered_on && assigned > 0.0 => {
                        Some((demand / assigned).clamp(0.0, 1.0))
                    }
                    _ => None,
                },
                storage_used_bytes: m.disk.as_ref().map(|d| d.used),
                storage_provisioned_bytes: m.disk.as_ref().map(|d| d.size),
                snapshots: m.snapshots,
                macs: m.macs.iter().filter_map(|t| mac(t)).collect(),
                addresses: m.addresses,
                replication,
                key: m.id,
                name: m.name,
                power_state: m.state,
                ..Vm::default()
            }
        })
        .collect();
    Outcome::Read { host, vms }
}

/// The samples of an outcome, at `now_ms`.
pub fn samples(outcome: &Outcome, now_ms: i64) -> Vec<Sample> {
    match outcome {
        Outcome::Read { host, vms } => {
            let mut out = virt::samples(vms, &[], &[], &[], now_ms);
            out.push(virt::up(
                None,
                &[("product", "Hyper-V"), ("host", host)],
                now_ms,
            ));
            out
        }
        Outcome::Failed { reason, error } => vec![virt::up(Some((reason, error)), &[], now_ms)],
    }
}

/// The `hyperv` sensor.
#[derive(Debug, Default)]
pub struct HyperVSensor;

#[cfg(windows)]
mod run {
    use std::process::Stdio;

    use tokio::io::AsyncReadExt;
    use tokio::time::Instant;

    use crate::powershell::{self, Job};

    /// Most output read; a host of thousands of machines stays below.
    const MAX_OUTPUT: u64 = 16 << 20;

    fn encoded(script: &str) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Runs the script and returns what it printed.
    pub async fn script(deadline: Instant) -> Result<String, String> {
        let system = powershell::system_dir().map_err(|e| e.to_string())?;
        let job = Job::new().map_err(|e| format!("job object: {e}"))?;
        let mut child = powershell::command(powershell::exe(&system))
            .and_then(|mut command| {
                command
                    .args(powershell::OPTIONS)
                    .arg("-EncodedCommand")
                    .arg(encoded(super::SCRIPT))
                    .stdout(Stdio::piped())
                    .spawn()
            })
            .map_err(|e| format!("starting powershell.exe: {e}"))?;
        job.assign(&mut child)
            .map_err(|e| format!("job object: {e}"))?;
        let mut stdout = child.stdout.take().expect("piped").take(MAX_OUTPUT);
        let mut text = String::new();
        let read = tokio::time::timeout_at(deadline, async {
            stdout.read_to_string(&mut text).await?;
            child.wait().await
        })
        .await;
        drop(job);
        match read {
            Ok(Ok(_)) => Ok(text),
            Ok(Err(error)) => Err(format!("reading the Hyper-V script's output: {error}")),
            Err(_) => Err("the Hyper-V script did not finish in time".into()),
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn base64_matches_known_values() {
            // "a" in UTF-16LE is 61 00.
            assert_eq!(super::encoded("a"), "YQA=");
            assert_eq!(super::encoded("ab"), "YQBiAA==");
        }
    }
}

#[async_trait::async_trait]
impl crate::Sensor for HyperVSensor {
    async fn collect(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<Sample>, crate::SensorError> {
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        };
        #[cfg(windows)]
        {
            let margin = deadline
                .checked_sub(std::time::Duration::from_millis(300))
                .unwrap_or(deadline);
            let outcome = match run::script(margin).await {
                Ok(output) => parse(&output),
                Err(error) => Outcome::Failed {
                    reason: "failed",
                    error: virt::cut(&error),
                },
            };
            Ok(samples(&outcome, now()))
        }
        #[cfg(not(windows))]
        {
            let _ = (deadline, now);
            Err(crate::SensorError::Permanent(
                "Hyper-V is read by the agent of a Windows server".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped as the script prints it on a Windows Server 2022 host: one
    /// running virtual machine replicated with a warning and two
    /// checkpoints, one stopped without integration services, no WMI
    /// answer for the second.
    const HOST: &str = r#"{"vms":[{"memory_startup":2147483648,"disk":{"used":14495514624,"size":136365211648},"snapshots":[1762000000000,1763400000000],"cpu_usage":12,"macs":["00155D01020A"],"state":"Running","memory_assigned":4294967296,"memory_demand":2576980377,"bios_guid":"{4C4C4544-0039-4810-8058-B4C04F395931}","cpu_count":4,"replication":{"state":"Replicating","mode":"Primary","health":"Warning"},"addresses":["10.1.2.3","fe80::215:5dff:fe01:20a"],"name":"web-01","id":"8e3b1a6c-5f0e-4f5b-9a55-1b2c3d4e5f60"},{"memory_startup":1073741824,"disk":null,"snapshots":[null],"cpu_usage":0,"macs":[null],"state":"Off","memory_assigned":0,"memory_demand":0,"bios_guid":null,"cpu_count":2,"replication":null,"addresses":[],"name":"build","id":"0f6f1c2e-0000-4000-8000-000000000002"}],"host":"HV-01"}"#;

    #[test]
    fn a_host_reads() {
        let Outcome::Read { host, vms } = parse(&format!("\u{feff}{HOST}\r\n")) else {
            panic!("not read");
        };
        assert_eq!(host, "HV-01");
        assert_eq!(vms.len(), 2);
        let web = &vms[0];
        assert_eq!(web.key, "8e3b1a6c-5f0e-4f5b-9a55-1b2c3d4e5f60");
        assert_eq!(
            web.uuid.as_deref(),
            Some("4c4c4544-0039-4810-8058-b4c04f395931")
        );
        assert!(web.powered_on);
        assert_eq!(web.cpu_usage_ratio, Some(0.12));
        assert_eq!(web.memory_bytes, Some(4294967296.0));
        assert!((web.memory_usage_ratio.unwrap() - 0.6).abs() < 0.001);
        assert_eq!(web.macs, ["00:15:5d:01:02:0a"]);
        assert_eq!(web.snapshots.len(), 2);
        assert_eq!(web.replication.as_ref().unwrap().health, Status::Warning);
        assert_eq!(web.storage_provisioned_bytes, Some(136365211648.0));
        let build = &vms[1];
        assert!(!build.powered_on && build.cpu_usage_ratio.is_none());
        assert_eq!(build.memory_bytes, Some(1073741824.0));
        assert!(build.snapshots.is_empty() && build.macs.is_empty());
        assert!(build.uuid.is_none() && build.replication.is_none());

        let samples = samples(&parse(HOST), 1_763_500_000_000);
        let value = |sensor: &str, instance: &str| {
            samples
                .iter()
                .find(|s| s.sensor == sensor && s.instance == instance)
                .map(|s| s.value)
        };
        assert_eq!(value("vm_replication_health", "web-01"), Some(1.0));
        assert_eq!(
            value("vm_snapshot_age_seconds", "web-01"),
            Some(1_500_000.0)
        );
        assert_eq!(value("vm_powered_on", "build"), Some(0.0));
        let up = samples.iter().find(|s| s.sensor == "up").unwrap();
        assert_eq!((up.value, up.labels["host"].as_str()), (1.0, "HV-01"));
    }

    #[test]
    fn failures_say_what_to_do() {
        let outcome = parse(r#"{"error":"not_installed"}"#);
        assert!(matches!(
            outcome,
            Outcome::Failed {
                reason: "not_installed",
                ..
            }
        ));
        let outcome = parse(
            r#"{"error":"denied","message":"You do not have the required permission to complete this task. Contact the administrator of the authorization policy for the computer 'HV-01'."}"#,
        );
        let Outcome::Failed { reason, error } = &outcome else {
            panic!()
        };
        assert_eq!(*reason, "denied");
        assert!(error.contains("Hyper-V Administrators"));
        let up = &samples(&outcome, 0)[0];
        assert_eq!(up.value, 0.0);
        assert_eq!(up.labels["reason"], "denied");
        assert!(matches!(
            parse(r#"{"error":"denied","message":"The operation failed."}"#),
            Outcome::Failed {
                reason: "failed",
                ..
            }
        ));
        assert!(matches!(
            parse("Get-VM : not json"),
            Outcome::Failed {
                reason: "failed",
                ..
            }
        ));
        assert!(matches!(
            parse(r#"{"host":"HV","vms":[]}"#),
            Outcome::Read { .. }
        ));
    }

    #[test]
    fn identities_are_normalised() {
        assert_eq!(
            uuid("{ABCDEF01-2345-6789-ABCD-EF0123456789}").as_deref(),
            Some("abcdef01-2345-6789-abcd-ef0123456789")
        );
        assert!(uuid("nope").is_none());
        assert_eq!(
            mac("00-15-5D-AA-BB-CC").as_deref(),
            Some("00:15:5d:aa:bb:cc")
        );
        assert!(mac("000000000000").is_none());
        assert!(mac("0015").is_none());
    }

    #[test]
    fn the_script_reads_only() {
        for verb in [
            "Set-",
            "New-VM",
            "Remove-",
            "Start-",
            "Stop-",
            "Invoke-",
            "Restart-",
            "Checkpoint-",
        ] {
            assert!(!SCRIPT.contains(verb), "{verb}");
        }
        assert!(!SCRIPT.contains('\u{2014}'));
    }
}
