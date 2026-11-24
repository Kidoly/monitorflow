//! winget, the third-party manager of Windows (ADR-0139). The update
//! service runs the `Microsoft.WinGet.Client` module in PowerShell 7 as
//! LocalSystem, the one way winget supports the system context, with the
//! fixed scripts below; the agent never runs winget itself.
//!
//! This module is what builds everywhere, so Linux tests cover it: the
//! scripts and the paths they run from, the App Installer policy, the
//! parsing of the scripts' JSON, which apps an update selects and how a
//! scan becomes the report's `ThirdPartyPackage`s and status. Installs
//! and removals from the catalogue, and held apps, follow ADR-0188.

use std::path::{Path, PathBuf};
use std::time::Duration;

use monitorflow_common::proto::v1::package_manager_status::State;
use monitorflow_common::proto::v1::{
    PackageManager, PackageManagerStatus, PackageScope, ThirdPartyPackage,
};
use monitorflow_common::third_party::valid_id;
use serde::{Deserialize, Serialize};

use super::windows_update::{InstallResult, Outcome};

#[cfg(windows)]
pub mod run;

/// The module that runs winget in the system context.
pub const MODULE: &str = "Microsoft.WinGet.Client";
/// winget's store source, never used (ADR-0138).
pub const STORE_SOURCE: &str = "msstore";
/// winget's community source, which `EnableDefaultSource` turns off.
pub const DEFAULT_SOURCE: &str = "winget";
/// The package ID of an update, read by [`UPGRADE_SCRIPT`].
pub const ENV_ID: &str = "MONITORFLOW_WINGET_ID";
/// The source of an update, read by [`UPGRADE_SCRIPT`].
pub const ENV_SOURCE: &str = "MONITORFLOW_WINGET_SOURCE";
/// Comma-separated sources [`SCAN_SCRIPT`] leaves out.
pub const ENV_EXCLUDE: &str = "MONITORFLOW_WINGET_EXCLUDE";
/// A scan: the first one downloads the community source's index.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// One app's update, install or removal.
pub const UPGRADE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Most apps one command updates.
pub const MAX_UPGRADES: usize = 100;
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

/// Updates the one app named by the environment, silently, from its
/// source, as one JSON object: the module's result or `{"error"}`.
pub const UPGRADE_SCRIPT: &str = "try {
  if (-not $module) { throw 'the Microsoft.WinGet.Client module is not installed' }
  Import-Module $module
  $r = Update-WinGetPackage -Id $env:MONITORFLOW_WINGET_ID -Source $env:MONITORFLOW_WINGET_SOURCE `
    -MatchOption Equals -Mode Silent
  $extended = 0
  if ($r.ExtendedErrorCode) { $extended = $r.ExtendedErrorCode.HResult }
  $message = ''
  try { $message = [string]$r.ErrorMessage() } catch { }
  [ordered]@{ status = [string]$r.Status; reboot_required = [bool]$r.RebootRequired;
    installer_error = [int64]$r.InstallerErrorCode; extended_error = [int64]$extended;
    message = $message } | ConvertTo-Json -Compress
} catch {
  @{ error = $_.Exception.Message } | ConvertTo-Json -Compress
}
";

/// Installs the one package named by the environment from its source,
/// silently and machine-wide when the package says, as one JSON object:
/// the module's result or `{"error"}` (ADR-0188).
pub const INSTALL_SCRIPT: &str = "try {
  if (-not $module) { throw 'the Microsoft.WinGet.Client module is not installed' }
  Import-Module $module
  $r = Install-WinGetPackage -Id $env:MONITORFLOW_WINGET_ID -Source $env:MONITORFLOW_WINGET_SOURCE `
    -MatchOption Equals -Mode Silent -Scope SystemOrUnknown
  $extended = 0
  if ($r.ExtendedErrorCode) { $extended = $r.ExtendedErrorCode.HResult }
  $message = ''
  try { $message = [string]$r.ErrorMessage() } catch { }
  [ordered]@{ status = [string]$r.Status; reboot_required = [bool]$r.RebootRequired;
    installer_error = [int64]$r.InstallerErrorCode; extended_error = [int64]$extended;
    message = $message } | ConvertTo-Json -Compress
} catch {
  @{ error = $_.Exception.Message } | ConvertTo-Json -Compress
}
";

/// Removes the one app named by the environment, as installed from its
/// source, silently, as one JSON object: the module's result or
/// `{"error"}` (ADR-0188).
pub const REMOVE_SCRIPT: &str = "try {
  if (-not $module) { throw 'the Microsoft.WinGet.Client module is not installed' }
  Import-Module $module
  $r = Uninstall-WinGetPackage -Id $env:MONITORFLOW_WINGET_ID -Source $env:MONITORFLOW_WINGET_SOURCE `
    -MatchOption Equals -Mode Silent
  $extended = 0
  if ($r.ExtendedErrorCode) { $extended = $r.ExtendedErrorCode.HResult }
  $message = ''
  try { $message = [string]$r.ErrorMessage() } catch { }
  [ordered]@{ status = [string]$r.Status; reboot_required = [bool]$r.RebootRequired;
    installer_error = [int64]$r.UninstallerErrorCode; extended_error = [int64]$extended;
    message = $message } | ConvertTo-Json -Compress
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

    pub fn allows(&self, source: &str) -> bool {
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

/// What the upgrade script wrote.
#[derive(Debug, Deserialize)]
struct UpgradeOutput {
    #[serde(default)]
    status: String,
    #[serde(default)]
    reboot_required: bool,
    #[serde(default)]
    installer_error: i64,
    #[serde(default)]
    extended_error: i64,
    #[serde(default)]
    message: String,
    error: Option<String>,
}

/// What became of one app's update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upgraded {
    /// Its result, and whether it needs a restart.
    Done(InstallResult, bool),
    /// winget had no update for it after all.
    NotPending,
}

/// One app's result from the upgrade script's stdout.
pub fn parse_upgrade(app: &App, stdout: &[u8]) -> Upgraded {
    let failed = |hresult: i64, message: &str| {
        let title = match one_line(message, MAX_ERROR) {
            m if m.is_empty() => app.name.clone(),
            m => format!("{}: {m}", app.name),
        };
        Upgraded::Done(
            InstallResult {
                name: app.id.clone(),
                title,
                result: Outcome::Failed,
                // HRESULTs are 32 bits; the module widens them.
                hresult: hresult as i32,
            },
            false,
        )
    };
    let output: UpgradeOutput = match serde_json::from_slice(trim_bom(stdout)) {
        Ok(output) => output,
        Err(_) => return failed(0, "the winget update wrote no valid JSON"),
    };
    if let Some(error) = output.error {
        return failed(0, &error);
    }
    match output.status.as_str() {
        "Ok" => Upgraded::Done(
            InstallResult {
                name: app.id.clone(),
                title: app.name.clone(),
                result: Outcome::Succeeded,
                hresult: 0,
            },
            output.reboot_required,
        ),
        "NoApplicableUpgrade" => Upgraded::NotPending,
        status => {
            let code = if output.extended_error != 0 {
                output.extended_error
            } else {
                output.installer_error
            };
            let message = if output.message.trim().is_empty() {
                status.to_owned()
            } else {
                output.message
            };
            failed(code, &message)
        }
    }
}

/// The app an update of `id` takes, the IDs compared without case as
/// winget does.
fn find<'a>(apps: &'a [App], id: &str) -> Option<&'a App> {
    apps.iter().find(|app| app.id.eq_ignore_ascii_case(id))
}

/// What an update of winget apps leaves out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LeftOut<'a> {
    /// IDs that are not installed or have no update.
    pub not_pending: Vec<String>,
    /// Pending apps held (ADR-0188).
    pub held: Vec<String>,
    /// Apps over [`MAX_UPGRADES`].
    pub over: Vec<&'a App>,
}

/// Which `apps` a selection of `ids` updates (every pending one when
/// empty) but those `hold` names, at most [`MAX_UPGRADES`], and what it
/// leaves out.
pub fn select<'a>(ids: &[String], hold: &[String], apps: &'a [App]) -> (Vec<&'a App>, LeftOut<'a>) {
    let mut not_pending = Vec::new();
    let picked: Vec<&App> = if ids.is_empty() {
        apps.iter().filter(|app| app.pending()).collect()
    } else {
        let mut picked: Vec<&App> = Vec::new();
        for id in ids {
            match find(apps, id).filter(|app| app.pending()) {
                Some(app) if !picked.contains(&app) => picked.push(app),
                Some(_) => {}
                None => not_pending.push(id.clone()),
            }
        }
        picked
    };
    let (mut picked, held): (Vec<&App>, Vec<&App>) = picked
        .into_iter()
        .partition(|app| !hold.iter().any(|h| h.eq_ignore_ascii_case(&app.id)));
    let over = picked.split_off(picked.len().min(MAX_UPGRADES));
    let held = held.into_iter().map(|app| app.id.clone()).collect();
    (
        picked,
        LeftOut {
            not_pending,
            held,
            over,
        },
    )
}

/// What an install or a removal does with one package of the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    /// Run the script for this app, from this source.
    Run(App),
    /// Already as asked: installed for an install, absent for a removal.
    Unchanged(String),
}

/// What installing `packages` (`(id, source)`) does, given the installed
/// `apps`: an app winget lists as installed, from any source, is left as
/// it is. At most [`MAX_UPGRADES`] packages are planned; a signed
/// command never holds more (ADR-0188).
pub fn plan_install(packages: &[(String, String)], apps: &[App]) -> Vec<Planned> {
    packages
        .iter()
        .take(MAX_UPGRADES)
        .map(|(id, source)| match find(apps, id) {
            Some(app) => Planned::Unchanged(app.id.clone()),
            None => Planned::Run(App {
                id: id.clone(),
                name: id.clone(),
                source: source.clone(),
                ..App::default()
            }),
        })
        .collect()
}

/// What removing the apps `ids` does: an app that is not installed is
/// left as it is; one that is is removed as installed, from the source
/// winget matched it to.
pub fn plan_remove(ids: &[String], apps: &[App]) -> Vec<Planned> {
    ids.iter()
        .take(MAX_UPGRADES)
        .map(|id| match find(apps, id) {
            Some(app) => Planned::Run(app.clone()),
            None => Planned::Unchanged(id.clone()),
        })
        .collect()
}

/// The result of an app [`select`] left out over the limit: not started,
/// which fails the command.
pub fn not_attempted(app: &App) -> InstallResult {
    InstallResult {
        name: app.id.clone(),
        title: format!("{}: not attempted, over {MAX_UPGRADES} apps", app.name),
        result: Outcome::NotStarted,
        hresult: 0,
    }
}

/// The result of an install from a source Group Policy turns off, or
/// the store's, which MonitorFlow never uses (ADR-0138, ADR-0188).
pub fn source_turned_off(app: &App) -> InstallResult {
    InstallResult {
        name: app.id.clone(),
        title: format!("{}: the source {} is not used", app.name, app.source),
        result: Outcome::NotStarted,
        hresult: 0,
    }
}

/// The result of an app whose update did not finish in `timeout`.
pub fn timed_out(app: &App, timeout: Duration) -> InstallResult {
    InstallResult {
        name: app.id.clone(),
        title: format!(
            "{}: timed out after {} minutes",
            app.name,
            timeout.as_secs() / 60
        ),
        result: Outcome::Aborted,
        hresult: 0,
    }
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
