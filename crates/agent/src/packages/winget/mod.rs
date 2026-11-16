//! winget, the third-party manager of Windows (ADR-0139). The update
//! service runs the `Microsoft.WinGet.Client` module in PowerShell 7 as
//! LocalSystem, the one way winget supports the system context, with the
//! fixed scripts below; the agent never runs winget itself.
//!
//! This module is what builds everywhere, so Linux tests cover it: the
//! scripts and the paths they run from, the App Installer policy, the
//! parsing of the scripts' JSON, which apps an update selects and how a
//! scan becomes the report's `ThirdPartyPackage`s and status.

use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::proto::v1::package_manager_status::State;
use monitorflow_common::proto::v1::{
    PackageManager, PackageManagerStatus, PackageScope, ThirdPartyPackage,
};
use monitorflow_common::third_party::valid_id;
use serde::{Deserialize, Serialize};

/// The module that runs winget in the system context.
pub const MODULE: &str = "Microsoft.WinGet.Client";
/// winget's store source, never used (ADR-0138).
pub const STORE_SOURCE: &str = "msstore";
/// winget's community source, which `EnableDefaultSource` turns off.
pub const DEFAULT_SOURCE: &str = "winget";
/// Comma-separated sources [`SCAN_SCRIPT`] leaves out.
pub const ENV_EXCLUDE: &str = "MONITORFLOW_WINGET_EXCLUDE";
/// A scan: the first one downloads the community source's index.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Most apps in a report (ADR-0138).
pub const MAX_APPS: usize = 5000;
const MAX_NAME: usize = 256;
const MAX_VERSION: usize = 128;
const MAX_ERROR: usize = 300;

/// What every script starts with: stop on errors, no progress records,
/// UTF-8 output without a byte order mark, the newest module present.
const PRELUDE: &str = "$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$module = Get-Module -ListAvailable -Name Microsoft.WinGet.Client |
  Sort-Object Version -Descending | Select-Object -First 1
";

/// Lists the apps of every configured source but the excluded ones, as
/// one JSON object: `{"module", "sources", "apps"}`, `{"missing":
/// "module"}` or `{"error"}`.
pub const SCAN_SCRIPT: &str = "try {
  if (-not $module) { @{ missing = 'module' } | ConvertTo-Json -Compress; exit 0 }
  Import-Module $module
  $exclude = @(\"$env:MONITORFLOW_WINGET_EXCLUDE\" -split ',' | Where-Object { $_ })
  $sources = @(Get-WinGetSource | ForEach-Object { [string]$_.Name } |
    Where-Object { $exclude -notcontains $_ })
  $apps = @(foreach ($source in $sources) {
    Get-WinGetPackage -Source $source | Where-Object { $_.Source -eq $source } |
      ForEach-Object {
        $available = ''
        if ($_.IsUpdateAvailable -and $_.AvailableVersions) {
          $available = [string]$_.AvailableVersions[0]
        }
        [ordered]@{ id = [string]$_.Id; name = [string]$_.Name; source = $source;
          version = [string]$_.InstalledVersion; available = $available }
      }
  })
  [ordered]@{ module = $module.Version.ToString(); sources = $sources; apps = $apps } |
    ConvertTo-Json -Depth 4 -Compress
} catch {
  @{ error = $_.Exception.Message } | ConvertTo-Json -Compress
}
";

/// The script `body` with the prelude.
pub fn script(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

/// `-EncodedCommand`'s argument: UTF-16LE in base64.
pub fn encoded(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Where PowerShell 7 and its machine-wide modules are, under
/// `%ProgramFiles%`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub pwsh: PathBuf,
    pub modules: PathBuf,
}

impl Paths {
    pub fn under(program_files: &Path) -> Self {
        let root = program_files.join("PowerShell");
        Self {
            pwsh: root.join("7").join("pwsh.exe"),
            modules: root.join("Modules"),
        }
    }

    /// The module's folder, which holds one folder per version.
    pub fn module(&self) -> PathBuf {
        self.modules.join(MODULE)
    }
}

/// The App Installer policy (`HKLM\SOFTWARE\Policies\Microsoft\Windows\
/// AppInstaller`): each value as read, `None` when missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    pub enable_app_installer: Option<u32>,
    pub enable_command_line: Option<u32>,
    pub enable_default_source: Option<u32>,
}

impl Policy {
    /// The registry values, in the order of the fields.
    pub const VALUES: [&'static str; 3] = [
        "EnableAppInstaller",
        "EnableWindowsPackageManagerCommandLineInterfaces",
        "EnableDefaultSource",
    ];

    /// Whether policy turns winget off: 0 does, a missing value does not.
    pub fn disabled(&self) -> bool {
        self.enable_app_installer == Some(0) || self.enable_command_line == Some(0)
    }

    /// The sources a scan leaves out: always the store's, and the
    /// community source when policy turns it off.
    pub fn excluded(&self) -> Vec<&'static str> {
        let mut excluded = vec![STORE_SOURCE];
        if self.enable_default_source == Some(0) {
            excluded.push(DEFAULT_SOURCE);
        }
        excluded
    }

    fn allows(&self, source: &str) -> bool {
        !self
            .excluded()
            .iter()
            .any(|s| s.eq_ignore_ascii_case(source))
    }
}

/// An app as a scan reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub id: String,
    pub name: String,
    pub source: String,
    pub version: String,
    /// The version an update would install; empty when none.
    pub available: String,
}

impl App {
    pub fn pending(&self) -> bool {
        !self.available.is_empty()
    }
}

/// [`PackageManagerStatus`]'s states, on the pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Absent,
    Unusable,
}

/// The reply to a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apps {
    pub state: Availability,
    /// The module's version, when it ran.
    pub version: String,
    /// Why winget is absent or unusable; empty when available.
    pub error: String,
    pub apps: Vec<App>,
}

impl Apps {
    pub fn absent(error: &str) -> Self {
        Self::not_available(Availability::Absent, error)
    }

    pub fn unusable(error: &str) -> Self {
        Self::not_available(Availability::Unusable, error)
    }

    fn not_available(state: Availability, error: &str) -> Self {
        Self {
            state,
            version: String::new(),
            error: one_line(error, MAX_ERROR),
            apps: Vec::new(),
        }
    }
}

pub const TURNED_OFF: &str = "winget is turned off by Group Policy";
pub const NO_POWERSHELL: &str = "PowerShell 7 is not installed";
pub const NO_MODULE: &str = "the Microsoft.WinGet.Client module is not installed for all users";

/// What the scan script wrote.
#[derive(Debug, Deserialize)]
struct ScanOutput {
    #[serde(default)]
    module: String,
    #[serde(default)]
    apps: Vec<App>,
    missing: Option<String>,
    error: Option<String>,
}

/// The scan's reply from the script's stdout. Apps of a source policy
/// leaves out, and apps whose ID falls outside ADR-0138's grammar, are
/// dropped; the second count is returned for the log. Pending apps come
/// first.
pub fn parse_scan(stdout: &[u8], policy: &Policy) -> (Apps, usize) {
    let output: ScanOutput = match serde_json::from_slice(trim_bom(stdout)) {
        Ok(output) => output,
        Err(error) => {
            return (
                Apps::unusable(&format!("the winget scan wrote no valid JSON: {error}")),
                0,
            )
        }
    };
    if let Some(error) = output.error {
        return (Apps::unusable(&error), 0);
    }
    if output.missing.is_some() {
        return (Apps::absent(NO_MODULE), 0);
    }
    let mut invalid = 0;
    let mut apps: Vec<App> = output
        .apps
        .into_iter()
        .filter(|app| policy.allows(&app.source))
        .filter(|app| {
            let ok = valid_id(PackageManager::Winget, &app.id);
            invalid += usize::from(!ok);
            ok
        })
        .map(|app| App {
            name: one_line(&app.name, MAX_NAME),
            version: one_line(&app.version, MAX_VERSION),
            available: one_line(&app.available, MAX_VERSION),
            ..app
        })
        .collect();
    apps.sort_by_key(|app| !app.pending());
    apps.truncate(MAX_APPS);
    let apps = Apps {
        state: Availability::Available,
        version: one_line(&output.module, MAX_VERSION),
        error: String::new(),
        apps,
    };
    (apps, invalid)
}

/// The report's status and apps for a scan (ADR-0138).
pub fn to_report(apps: &Apps) -> (PackageManagerStatus, Vec<ThirdPartyPackage>) {
    let state = match apps.state {
        Availability::Available => State::Available,
        Availability::Absent => State::Absent,
        Availability::Unusable => State::Unusable,
    };
    let status = PackageManagerStatus {
        manager: PackageManager::Winget.into(),
        state: state.into(),
        version: apps.version.clone(),
        error: apps.error.clone(),
    };
    let packages = apps
        .apps
        .iter()
        .map(|app| ThirdPartyPackage {
            manager: PackageManager::Winget.into(),
            id: app.id.clone(),
            name: app.name.clone(),
            version: app.version.clone(),
            available_version: app.available.clone(),
            scope: PackageScope::Machine.into(),
            origin: app.source.clone(),
        })
        .collect();
    (status, packages)
}

/// PowerShell may start its output with a byte order mark.
fn trim_bom(bytes: &[u8]) -> &[u8] {
    bytes
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(bytes)
        .trim_ascii()
}

/// `text` on one line, without control characters, at most `max`
/// characters.
fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    line.unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests;
