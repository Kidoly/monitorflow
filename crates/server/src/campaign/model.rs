//! Campaign values without storage: states and actions, failure
//! thresholds, maintenance windows and the wave plan.

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use monitorflow_common::command::valid_package_name;
use monitorflow_common::major_upgrade;
use monitorflow_common::proto::v1::configure_sources::Mode;
use monitorflow_common::proto::v1::{
    ApplyUpdates, MajorUpgrade, MsiFile, MsiProperty, PackageManager, SoftwareChange,
    SoftwarePackage, ThirdPartySelection,
};
use monitorflow_common::{software, third_party};
use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::commands::sources::{CURRENT, MAX_MIRRORS};

/// Most servers in one campaign.
pub const MAX_TARGETS: usize = 10_000;
/// Most waves in one campaign.
pub const MAX_WAVES: usize = 100;
/// Most maintenance windows in one campaign.
pub const MAX_WINDOWS: usize = 32;
/// Longest package list, as for a single command.
pub const MAX_PACKAGES: usize = 1000;

/// Where a campaign is. `stopped`, `completed` and `failed` are final.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum CampaignState {
    /// Created, not started.
    Draft,
    /// Started; no wave queued yet, waiting for a window.
    Scheduled,
    /// A wave was queued.
    Running,
    /// Nothing more is queued until resumed.
    Paused,
    /// Stopped by a user.
    Stopped,
    /// Every wave ended within its failure threshold.
    Completed,
    /// A wave exceeded its failure threshold.
    Failed,
}

impl CampaignState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// Whether the runner advances it.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Scheduled | Self::Running)
    }

    pub fn is_final(self) -> bool {
        matches!(self, Self::Stopped | Self::Completed | Self::Failed)
    }
}

impl fmt::Display for CampaignState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a user can do to a campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Pause,
    Resume,
    Stop,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Stop => "stop",
        }
    }

    /// The state after this action from `from`, `None` when it is not
    /// allowed. `started` says whether a wave was queued already.
    pub fn next(self, from: CampaignState, started: bool) -> Option<CampaignState> {
        use CampaignState::*;
        match (self, from) {
            (Self::Start, Draft) => Some(Scheduled),
            (Self::Pause, Scheduled | Running) => Some(Paused),
            (Self::Resume, Paused) => Some(if started { Running } else { Scheduled }),
            (Self::Stop, Draft | Scheduled | Running | Paused) => Some(Stopped),
            _ => None,
        }
    }
}

/// How a failure threshold counts: servers, or percent of the batch.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
#[schema(as = CampaignFailureUnit)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum FailureUnit {
    Count,
    Percent,
}

/// A wave fails when more of its servers failed than this. A failed
/// server is a command `failed`, `expired` or `cancelled`, or a server
/// skipped with an error. The default, `{"value": 0, "unit": "count"}`,
/// stops at the first failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignFailureThreshold)]
#[serde(deny_unknown_fields)]
pub struct FailureThreshold {
    /// Servers (up to 10000), or percent (up to 100).
    pub value: u32,
    pub unit: FailureUnit,
}

impl Default for FailureThreshold {
    /// Any failure stops the campaign.
    fn default() -> Self {
        Self {
            value: 0,
            unit: FailureUnit::Count,
        }
    }
}

impl FailureThreshold {
    pub fn validate(&self) -> Result<(), String> {
        if self.unit == FailureUnit::Percent && self.value > 100 {
            return Err("a failure percentage is at most 100".into());
        }
        if self.value > MAX_TARGETS as u32 {
            return Err(format!("a failure count is at most {MAX_TARGETS}"));
        }
        Ok(())
    }

    /// Whether `failed` servers out of a wave of `size` exceed it.
    pub fn exceeded(&self, failed: usize, size: usize) -> bool {
        match self.unit {
            FailureUnit::Count => failed > self.value as usize,
            FailureUnit::Percent => failed * 100 > self.value as usize * size,
        }
    }
}

/// Minutes past local midnight, written `HH:MM`; `24:00` is the end of
/// the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime(pub u16);

impl LocalTime {
    pub const END_OF_DAY: Self = Self(24 * 60);
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

impl std::str::FromStr for LocalTime {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || format!("invalid time `{}`, expected HH:MM", s.escape_debug());
        let (h, m) = s.split_once(':').ok_or_else(bad)?;
        if h.len() != 2 || m.len() != 2 {
            return Err(bad());
        }
        let h: u16 = h.parse().map_err(|_| bad())?;
        let m: u16 = m.parse().map_err(|_| bad())?;
        if m >= 60 || h > 24 || (h == 24 && m != 0) {
            return Err(bad());
        }
        Ok(Self(h * 60 + m))
    }
}

impl Serialize for LocalTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LocalTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// When a campaign may queue commands. With no window, at any time; with
/// several, whenever one is open. At most 32.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignWindow)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Window {
    /// Every week on `weekdays` (ISO: 1 is Monday, 7 Sunday), from `start`
    /// to `end` local time in `timezone` (IANA name). An `end` before
    /// `start` closes the next day.
    Weekly {
        weekdays: Vec<u8>,
        /// `HH:MM`, local time.
        #[schema(value_type = String, example = "22:00")]
        start: LocalTime,
        /// `HH:MM`, local time; `00:00` and `24:00` are the end of the day.
        #[schema(value_type = String, example = "04:00")]
        end: LocalTime,
        /// An IANA time zone, like `Europe/Paris`.
        timezone: String,
    },
    /// Every month in `months` (1 is January; empty or absent is every
    /// month), on day `month_day` (1 to 31; a month without that day has
    /// no window), or on the `weekday` (ISO) of week `month_week` of the
    /// month (1 to 4, or 5 for the last), from `start` to `end` local time
    /// in `timezone`. One month in `months` makes a yearly window
    /// (ADR-0167).
    Monthly {
        /// Set alone, or `month_week` and `weekday` together.
        #[serde(default)]
        month_day: Option<u8>,
        #[serde(default)]
        month_week: Option<u8>,
        #[serde(default)]
        weekday: Option<u8>,
        #[serde(default)]
        months: Vec<u8>,
        /// `HH:MM`, local time.
        #[schema(value_type = String, example = "22:00")]
        start: LocalTime,
        /// `HH:MM`, local time; `00:00` and `24:00` are the end of the day.
        #[schema(value_type = String, example = "04:00")]
        end: LocalTime,
        /// An IANA time zone, like `Europe/Paris`.
        timezone: String,
    },
    /// Once, from `starts_at` to `ends_at`.
    Once {
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
    },
}

impl Window {
    /// Checks everything but the time zone name, which the store checks
    /// against PostgreSQL's database. Sorts and deduplicates `weekdays`.
    pub fn validate(&mut self) -> Result<(), String> {
        match self {
            Self::Weekly {
                weekdays,
                start,
                end,
                timezone,
            } => {
                weekdays.sort_unstable();
                weekdays.dedup();
                if weekdays.is_empty() || weekdays.iter().any(|d| !(1..=7).contains(d)) {
                    return Err("weekdays are 1 (Monday) to 7 (Sunday), at least one".into());
                }
                check_hours(start, end, timezone)
            }
            Self::Monthly {
                month_day,
                month_week,
                weekday,
                months,
                start,
                end,
                timezone,
            } => {
                match (*month_day, *month_week, *weekday) {
                    (Some(1..=31), None, None) => {}
                    (None, Some(1..=5), Some(1..=7)) => {}
                    _ => {
                        return Err(
                            "a monthly window has a month_day (1 to 31), or a month_week \
                                    (1 to 4, 5 for the last) and a weekday (1 to 7)"
                                .into(),
                        )
                    }
                }
                months.sort_unstable();
                months.dedup();
                if months.iter().any(|m| !(1..=12).contains(m)) {
                    return Err("months are 1 (January) to 12 (December)".into());
                }
                if months.is_empty() {
                    *months = (1..=12).collect();
                }
                check_hours(start, end, timezone)
            }
            Self::Once { starts_at, ends_at } => {
                if starts_at >= ends_at {
                    return Err("a window ends after it starts".into());
                }
                Ok(())
            }
        }
    }
}

/// Checks the local hours and time zone of a recurring window; midnight
/// as an end becomes `24:00`.
fn check_hours(start: &LocalTime, end: &mut LocalTime, timezone: &str) -> Result<(), String> {
    if *start >= LocalTime::END_OF_DAY {
        return Err("a window starts before 24:00".into());
    }
    // Midnight as an end is the end of the day.
    if end.0 == 0 {
        *end = LocalTime::END_OF_DAY;
    }
    if start == end {
        return Err("a window ends at another time than it starts".into());
    }
    if timezone.is_empty() || timezone.len() > 64 {
        return Err("a window needs a time zone such as `Europe/Paris`".into());
    }
    Ok(())
}

/// A major upgrade campaign (ADR-0182): every server moves from `from` to
/// `to`, the next release, one at a time unless `parallel` says more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignMajorUpgrade)]
#[serde(deny_unknown_fields)]
pub struct MajorUpgradeSpec {
    /// The release every server runs, like `debian 12`.
    pub from: String,
    /// The next release, like `debian 13`; never two releases at once.
    pub to: String,
    /// Upgrade servers that cannot get a snapshot first. Without it, a
    /// server whose rollback is limited fails instead of being upgraded.
    #[serde(default)]
    pub accept_limited_rollback: bool,
    /// How many servers of a batch may be upgrading at once, 1 to 50;
    /// 1 by default.
    #[serde(default = "one")]
    pub parallel: u32,
}

fn one() -> u32 {
    1
}

/// Most servers of a batch upgrading at once.
pub const MAX_PARALLEL: u32 = 50;

impl MajorUpgradeSpec {
    pub fn validate(&self) -> Result<&'static major_upgrade::Upgrade, String> {
        if !(1..=MAX_PARALLEL).contains(&self.parallel) {
            return Err(format!("parallel must be between 1 and {MAX_PARALLEL}"));
        }
        major_upgrade::step(&self.from, &self.to).ok_or_else(|| {
            format!(
                "MonitorFlow cannot upgrade from {} to {}: one release at a time, Debian 12 to 13 for now",
                self.from, self.to
            )
        })
    }

    /// The payload of each server's command.
    pub fn payload(&self) -> Vec<u8> {
        MajorUpgrade {
            from: self.from.clone(),
            to: self.to.clone(),
        }
        .encode_to_vec()
    }
}

/// The `ApplyUpdates` of every command of a campaign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignUpdateSpec)]
pub struct UpdateSpec {
    /// OS packages to upgrade; empty upgrades everything.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Only security updates of the OS. Third-party managers have no
    /// security classification: they are included whole or not at all.
    #[serde(default)]
    pub security_only: bool,
    /// Whether the agent may reboot the server when an update needs it.
    #[serde(default)]
    pub allow_reboot: bool,
    /// Whether the OS's own updates are included; true by default
    /// (ADR-0138).
    #[serde(default = "yes")]
    pub include_os: bool,
    /// Third-party managers to update, after the OS (ADR-0138).
    #[serde(default)]
    pub third_party: Vec<ThirdPartySpec>,
    /// Windows Update classifications approved, by English name (`Security
    /// Updates`, `Critical Updates`, `Definition Updates`, `Update
    /// Rollups`, `Updates`, `Feature Packs`, `Service Packs`, `Tools`,
    /// `Upgrades`); empty approves every one but `Upgrades` (ADR-0188).
    /// Windows servers only; Linux servers of the campaign ignore it.
    #[serde(default)]
    pub classifications: Vec<String>,
    /// Windows updates never installed, as KB numbers or update IDs
    /// (ADR-0188). Windows servers only, like `classifications`.
    #[serde(default)]
    pub hold: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for UpdateSpec {
    fn default() -> Self {
        Self {
            packages: Vec::new(),
            security_only: false,
            allow_reboot: false,
            include_os: true,
            third_party: Vec::new(),
            classifications: Vec::new(),
            hold: Vec::new(),
        }
    }
}

/// What an update asks of one third-party manager (ADR-0138).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ThirdPartySelection)]
#[serde(deny_unknown_fields)]
pub struct ThirdPartySpec {
    /// `winget`, `flatpak` or `snap`.
    pub manager: String,
    /// Package IDs to update; empty updates every pending one.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Package IDs never updated, whatever else selects them (ADR-0188).
    /// Windows (winget) only for now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hold: Vec<String>,
}

/// Why a server gets no third-party update: its agent, or a relay on the
/// way, predates them (ADR-0138).
pub const NO_THIRD_PARTY: &str = "agent does not report third-party apps";

/// Why a server gets no update that approves or holds: its agent predates
/// ADR-0188 and would install what is held.
pub const NO_APPROVALS: &str =
    "agent does not know approvals and held updates yet; it updates by itself";

impl UpdateSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.packages.len() > MAX_PACKAGES {
            return Err(format!("at most {MAX_PACKAGES} packages"));
        }
        if let Some(bad) = self.packages.iter().find(|p| !valid_package_name(p)) {
            return Err(format!("invalid package name `{}`", bad.escape_debug()));
        }
        let apply = self.apply();
        third_party::check(&apply, None).map_err(|e| e.to_string())?;
        software::check_approvals(&apply).map_err(|e| e.to_string())?;
        // Held apps are winget's only, for now (ADR-0188).
        if self
            .third_party
            .iter()
            .any(|s| !s.hold.is_empty() && s.manager != "winget")
        {
            return Err("held apps are for winget only for now".into());
        }
        Ok(())
    }

    /// Whether it approves classifications or holds anything.
    pub fn has_approvals(&self) -> bool {
        software::has_approvals(&self.apply())
    }

    /// Whether any third-party manager is selected.
    pub fn has_third_party(&self) -> bool {
        !self.third_party.is_empty()
    }

    /// The managers selected; unknown names left out.
    pub fn managers(&self) -> Vec<PackageManager> {
        self.third_party
            .iter()
            .filter_map(|s| third_party::parse(&s.manager))
            .collect()
    }

    pub fn apply(&self) -> ApplyUpdates {
        ApplyUpdates {
            packages: self.packages.clone(),
            security_only: self.security_only,
            allow_reboot: self.allow_reboot,
            skip_os: !self.include_os,
            third_party: self
                .third_party
                .iter()
                .map(|s| ThirdPartySelection {
                    manager: third_party::parse(&s.manager).map_or(0, Into::into),
                    packages: s.packages.clone(),
                    hold: s.hold.clone(),
                })
                .collect(),
            classifications: self.classifications.clone(),
            hold: self.hold.clone(),
        }
    }

    pub fn payload(&self) -> Vec<u8> {
        self.apply().encode_to_vec()
    }

    /// The `ApplyUpdates` for one server of `os_family`: managers of
    /// another platform are left out, and so is every manager when its
    /// agent does not `report` third-party apps, which an OS-less update
    /// then cannot do without (ADR-0138). Approvals and holds are Windows
    /// Update's and winget's: left out for Linux servers, and refused for
    /// a Windows agent that does not know them (`approvals`, ADR-0188).
    pub fn for_server(
        &self,
        os_family: &str,
        reports: bool,
        approvals: bool,
    ) -> Result<ApplyUpdates, String> {
        let mut apply = self.apply();
        if os_family != "windows" {
            apply.classifications.clear();
            apply.hold.clear();
            for selection in &mut apply.third_party {
                selection.hold.clear();
            }
        } else if software::has_approvals(&apply) && !approvals {
            return Err(NO_APPROVALS.into());
        }
        let platform = third_party::managers_for(os_family);
        apply
            .third_party
            .retain(|s| platform.contains(&s.manager()));
        if !reports && apply.skip_os {
            return Err(NO_THIRD_PARTY.into());
        }
        if !reports {
            apply.third_party.clear();
        }
        if apply.skip_os && apply.third_party.is_empty() {
            return Err("nothing to update on this server".into());
        }
        Ok(apply)
    }
}

/// What a software campaign or command installs or removes (ADR-0188),
/// resolved from the catalogue when it is made: later changes to the
/// catalogue do not change it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = SoftwareDeployment)]
#[serde(deny_unknown_fields)]
pub struct SoftwareSpec {
    /// `install` or `remove`.
    pub action: String,
    /// At least one, at most 100.
    pub packages: Vec<SoftwareItem>,
}

/// One package of a [`SoftwareSpec`], as the catalogue held it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = SoftwareDeploymentPackage)]
#[serde(deny_unknown_fields)]
pub struct SoftwareItem {
    /// The catalogue entry it came from; it may have been deleted since.
    pub catalog_id: Uuid,
    pub name: String,
    /// `winget` (Windows), `apt` (Debian family) or `dnf` (RHEL family).
    pub manager: String,
    /// The package ID, like `7zip.7zip`, or the distribution's package
    /// name, like `nginx`.
    pub id: String,
    /// The winget source it installs from, like `winget`; empty for apt
    /// and dnf.
    pub source: String,
    /// The mirror of the central server an apt or dnf package installs
    /// from, added to the server's sources for the campaign; absent when
    /// it comes from the server's own sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub mirror: Option<String>,
    /// `msi` only: the file and properties, as the catalogue held them;
    /// `id` is then the ProductCode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub msi: Option<MsiItem>,
}

/// The file of an MSI package of a [`SoftwareSpec`] (ADR-0188).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = SoftwareDeploymentMsi)]
#[serde(deny_unknown_fields)]
pub struct MsiItem {
    /// SHA-256 of the file, lower-case hex.
    pub sha256: String,
    pub size: u64,
    /// Public properties passed to `msiexec`.
    #[serde(default)]
    pub properties: Vec<MsiPropertyItem>,
}

/// One public property of an MSI install, `NAME=value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MsiProperty)]
#[serde(deny_unknown_fields)]
pub struct MsiPropertyItem {
    /// Upper-case letters, digits and `_`, starting with a letter.
    pub name: String,
    pub value: String,
}

/// Where a server downloads MSI files from, and whether its agent takes
/// them (ADR-0188).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsiSources {
    /// Mirror listeners, in the order the agent tries them: its relay's,
    /// then the central server's.
    pub bases: Vec<String>,
    /// The agent reports `software_msi`.
    pub known: bool,
}

impl MsiSources {
    /// `relay` (the server's relay's mirror URL) then `central` (the
    /// central server's), each when set.
    pub fn new(relay: Option<&str>, central: Option<&str>, known: bool) -> Self {
        Self {
            bases: relay
                .into_iter()
                .chain(central)
                .map(|url| url.trim_end_matches('/').to_owned())
                .collect(),
            known,
        }
    }
}

/// Why a server gets no software command: nothing of the campaign is
/// for its system.
pub const NO_SOFTWARE_FOR: &str = "none of this software is for this server's system";
/// Why a server whose agent predates MSI packages gets none.
pub const MSI_TOO_OLD: &str =
    "this server's agent is too old to install MSI packages; it updates by itself";
/// Why a server that cannot reach any mirror listener gets no MSI package.
pub const NO_MSI_URL: &str = "MSI packages download from a mirror listener; set the mirror URL \
     of the central server";

/// The catalogue manager of MSI packages, for Windows servers.
pub const MSI: &str = "msi";

impl SoftwareSpec {
    /// The payload of the `software` command of a server of `os_family`:
    /// the packages of its manager only (ADR-0188).
    /// MSI packages, Windows only, download from `msi`.
    pub fn change_for(
        &self,
        os_family: &str,
        allow_reboot: bool,
        msi: &MsiSources,
    ) -> Result<SoftwareChange, String> {
        let manager = software::manager_for(os_family).ok_or_else(|| {
            format!("software deployment is not available on {os_family} servers yet")
        })?;
        let key = third_party::key(manager).unwrap_or_default();
        let install = self.action == "install";
        let mut packages = Vec::new();
        for p in &self.packages {
            if p.manager == key {
                packages.push(SoftwarePackage {
                    id: p.id.clone(),
                    source: p.source.clone(),
                    msi: None,
                });
                continue;
            }
            if p.manager != MSI || manager != PackageManager::Winget {
                continue;
            }
            if !msi.known {
                return Err(MSI_TOO_OLD.into());
            }
            let file = p.msi.as_ref().ok_or("an MSI package without its file")?;
            let mut item = MsiFile {
                name: p.name.clone(),
                ..MsiFile::default()
            };
            if install {
                if msi.bases.is_empty() {
                    return Err(NO_MSI_URL.into());
                }
                item.sha256 = file.sha256.clone();
                item.size = file.size;
                item.urls = msi
                    .bases
                    .iter()
                    .map(|base| {
                        format!("{base}/{}/{}.msi", crate::software_files::DIR, file.sha256)
                    })
                    .collect();
                item.properties = file
                    .properties
                    .iter()
                    .map(|p| MsiProperty {
                        name: p.name.clone(),
                        value: p.value.clone(),
                    })
                    .collect();
            }
            packages.push(SoftwarePackage {
                id: p.id.clone(),
                source: String::new(),
                msi: Some(item),
            });
        }
        if packages.is_empty() {
            return Err(NO_SOFTWARE_FOR.into());
        }
        Ok(SoftwareChange {
            action: software::parse_action(&self.action).map_or(0, Into::into),
            manager: manager.into(),
            packages,
            allow_reboot,
        })
    }

    /// The managers it names, each once, in order.
    pub fn managers(&self) -> Vec<PackageManager> {
        let mut managers = Vec::new();
        for item in &self.packages {
            // MSI packages are for Windows, which winget serves too.
            let manager = if item.manager == MSI {
                Some(PackageManager::Winget)
            } else {
                third_party::parse(&item.manager)
            };
            if let Some(manager) = manager {
                if !managers.contains(&manager) {
                    managers.push(manager);
                }
            }
        }
        managers
    }

    /// The mirrors its packages install from, each once.
    pub fn mirrors(&self) -> Vec<String> {
        let mut mirrors: Vec<String> = Vec::new();
        for mirror in self.packages.iter().filter_map(|p| p.mirror.as_ref()) {
            if !mirrors.contains(mirror) {
                mirrors.push(mirror.clone());
            }
        }
        mirrors
    }

    /// Checks the action and, per manager, its packages as a server's
    /// command will carry them.
    pub fn validate(&self) -> Result<(), String> {
        if software::parse_action(&self.action).is_none() {
            return Err("software.action must be `install` or `remove`".into());
        }
        if self.packages.is_empty() {
            return Err(format!("no software to {}", self.action));
        }
        if self.packages.len() > software::MAX_PACKAGES {
            return Err(format!(
                "at most {} packages at once",
                software::MAX_PACKAGES
            ));
        }
        for item in &self.packages {
            let known =
                third_party::parse(&item.manager).filter(|m| software::is_software_manager(*m));
            if known.is_none() && item.manager != MSI {
                return Err(format!(
                    "unknown software manager `{}`: winget, msi, apt or dnf",
                    item.manager.escape_debug()
                ));
            }
            if item.mirror.is_some() && matches!(item.manager.as_str(), "winget" | MSI) {
                return Err("winget and MSI packages do not install from a mirror".into());
            }
            if item.msi.is_some() != (item.manager == MSI) {
                return Err("only MSI packages have a file".into());
            }
        }
        // Any URL does to check what the commands will carry.
        let sources = MsiSources::new(Some("http://mirror.invalid"), None, true);
        for family in ["windows", "debian", "rhel"] {
            match self.change_for(family, false, &sources) {
                Ok(change) => {
                    software::check_change(&change, Some(family)).map_err(|e| e.to_string())?
                }
                Err(error) if error == NO_SOFTWARE_FOR => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// Why a server gets no software command: its agent refuses them, or
/// predates them (ADR-0188).
pub fn software_refusal(state: Option<&str>, os_family: &str) -> Option<&'static str> {
    let windows = os_family == "windows";
    match state {
        Some("accepted") => None,
        Some("off") if windows => Some(
            "software installs are not allowed on this server, or were turned off from the web \
             interface; an administrator allows them there with `monitorflow-agent.exe software on`",
        ),
        Some("off") => Some(
            "software installs are not allowed on this server, or were turned off from the web \
             interface; an administrator allows them there with `sudo monitorflow-agent software \
             on`, or the installer with MF_SOFTWARE_FROM_WEB=1",
        ),
        Some(_) if !windows => Some(
            "this server cannot install software from the catalogue: its sudo rules predate \
             software installs, or it has no apt or dnf; run the install command again",
        ),
        Some(_) => Some("this server's system cannot install software from the catalogue yet"),
        None => Some("this server's agent is too old to install software; it updates by itself"),
    }
}

/// How pinned sources treat a host's other sources (ADR-0032).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
#[schema(as = CampaignSourcesMode)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum SourcesMode {
    /// The pinned mirrors are the server's only package sources during
    /// its update.
    Replace,
    /// They are added to the server's own sources.
    Add,
}

impl From<SourcesMode> for Mode {
    fn from(mode: SourcesMode) -> Self {
        match mode {
            SourcesMode::Replace => Mode::Replace,
            SourcesMode::Add => Mode::Add,
        }
    }
}

/// A mirror and the published snapshot a campaign pins it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignPinnedMirror)]
#[serde(deny_unknown_fields)]
pub struct PinnedMirror {
    /// The mirror's name.
    pub name: String,
    /// One of its published snapshots, not `current`.
    pub snapshot: String,
}

/// The snapshots every server of a campaign installs from (ADR-0038):
/// each server uses the mirrors of its OS family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignSources)]
#[serde(deny_unknown_fields)]
pub struct SourcesPin {
    pub mode: SourcesMode,
    /// One or more mirrors, each once. Windows servers take no sources.
    pub mirrors: Vec<PinnedMirror>,
}

impl SourcesPin {
    /// Checks the shape; the store checks the snapshots exist.
    pub fn validate(&self) -> Result<(), String> {
        if self.mirrors.is_empty() || self.mirrors.len() > MAX_MIRRORS {
            return Err(format!(
                "sources.mirrors must list 1 to {MAX_MIRRORS} mirrors"
            ));
        }
        for (i, m) in self.mirrors.iter().enumerate() {
            if m.snapshot == CURRENT {
                return Err("a pinned snapshot is a published snapshot, not `current`".into());
            }
            if self.mirrors[..i].iter().any(|o| o.name == m.name) {
                return Err(format!("mirror `{}` is repeated", m.name.escape_debug()));
            }
        }
        Ok(())
    }
}

/// One wave as requested: explicit servers, a number of servers, or a
/// percentage of all targets (rounded up), taken in target order from
/// those no explicit wave names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = CampaignWaveSpec)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WaveSpec {
    /// These servers of the campaign, `{"servers": [...]}`.
    Servers(Vec<Uuid>),
    /// The next `n` servers by host name, `{"size": n}`.
    Size(u32),
    /// The next servers, `p` percent of all of them rounded up (1 to 100),
    /// `{"percent": p}`.
    Percent(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("server {0} of wave {1} is not a target of the campaign")]
    NotATarget(Uuid, usize),
    #[error("server {0} is in more than one wave")]
    Duplicate(Uuid),
    #[error("wave {0} would be empty")]
    EmptyWave(usize),
    #[error("a wave percentage is 1 to 100")]
    Percent,
    #[error("at most {MAX_WAVES} waves")]
    TooManyWaves,
}

/// Splits `targets`, in their order, into waves following `specs`. Servers
/// left over form one last wave; no specs make one wave of all.
pub fn plan_waves(targets: &[Uuid], specs: &[WaveSpec]) -> Result<Vec<Vec<Uuid>>, PlanError> {
    let known: HashSet<Uuid> = targets.iter().copied().collect();
    let mut placed = HashSet::new();
    for (index, spec) in specs.iter().enumerate() {
        if let WaveSpec::Servers(servers) = spec {
            for server in servers {
                if !known.contains(server) {
                    return Err(PlanError::NotATarget(*server, index));
                }
                if !placed.insert(*server) {
                    return Err(PlanError::Duplicate(*server));
                }
            }
        }
    }
    let mut pool = targets.iter().filter(|t| !placed.contains(t)).copied();
    let mut waves = Vec::with_capacity(specs.len() + 1);
    for (index, spec) in specs.iter().enumerate() {
        let wave: Vec<Uuid> = match spec {
            WaveSpec::Servers(servers) => servers.clone(),
            WaveSpec::Size(size) => pool.by_ref().take(*size as usize).collect(),
            WaveSpec::Percent(p) => {
                if !(1..=100).contains(p) {
                    return Err(PlanError::Percent);
                }
                let size = (targets.len() * *p as usize).div_ceil(100);
                pool.by_ref().take(size).collect()
            }
        };
        if wave.is_empty() {
            return Err(PlanError::EmptyWave(index));
        }
        waves.push(wave);
    }
    let rest: Vec<Uuid> = pool.collect();
    if !rest.is_empty() {
        waves.push(rest);
    }
    if waves.len() > MAX_WAVES {
        return Err(PlanError::TooManyWaves);
    }
    Ok(waves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitorflow_common::proto::v1::software_change;
    use serde_json::json;

    fn ids(n: u128) -> Vec<Uuid> {
        (1..=n).map(Uuid::from_u128).collect()
    }

    #[test]
    fn actions_follow_the_state_machine() {
        use CampaignState::*;
        assert_eq!(Action::Start.next(Draft, false), Some(Scheduled));
        assert_eq!(Action::Start.next(Scheduled, false), None);
        assert_eq!(Action::Pause.next(Running, true), Some(Paused));
        assert_eq!(Action::Pause.next(Draft, false), None);
        assert_eq!(Action::Resume.next(Paused, true), Some(Running));
        assert_eq!(Action::Resume.next(Paused, false), Some(Scheduled));
        assert_eq!(Action::Resume.next(Running, true), None);
        for from in [Draft, Scheduled, Running, Paused] {
            assert_eq!(Action::Stop.next(from, false), Some(Stopped));
        }
        for from in [Stopped, Completed, Failed] {
            assert!(from.is_final() && !from.is_active());
            for action in [Action::Start, Action::Pause, Action::Resume, Action::Stop] {
                assert_eq!(action.next(from, true), None);
            }
        }
        assert!(Scheduled.is_active() && Running.is_active() && !Paused.is_active());
    }

    #[test]
    fn thresholds_count_or_percent() {
        let count = FailureThreshold::default();
        assert!(!count.exceeded(0, 10) && count.exceeded(1, 10));
        let two = FailureThreshold {
            value: 2,
            unit: FailureUnit::Count,
        };
        assert!(!two.exceeded(2, 3) && two.exceeded(3, 3));
        let percent = FailureThreshold {
            value: 25,
            unit: FailureUnit::Percent,
        };
        assert!(!percent.exceeded(1, 4) && percent.exceeded(2, 4));
        assert!(percent.exceeded(1, 3));
        assert!(FailureThreshold {
            value: 101,
            unit: FailureUnit::Percent
        }
        .validate()
        .is_err());
        assert!(percent.validate().is_ok());
    }

    #[test]
    fn local_times_parse_and_print() {
        assert_eq!("22:30".parse::<LocalTime>().unwrap(), LocalTime(1350));
        assert_eq!("24:00".parse::<LocalTime>(), Ok(LocalTime::END_OF_DAY));
        for bad in ["24:01", "7:00", "07:60", "0700", "aa:bb", ""] {
            assert!(bad.parse::<LocalTime>().is_err(), "{bad}");
        }
        assert_eq!(LocalTime(65).to_string(), "01:05");
    }

    #[test]
    fn windows_are_validated_and_round_trip() {
        let mut weekly: Window = serde_json::from_value(json!({
            "kind": "weekly", "weekdays": [7, 6, 6], "start": "22:00", "end": "00:00",
            "timezone": "Europe/Paris"
        }))
        .unwrap();
        weekly.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&weekly).unwrap(),
            json!({"kind": "weekly", "weekdays": [6, 7], "start": "22:00", "end": "24:00",
                   "timezone": "Europe/Paris"})
        );
        for bad in [
            json!({"kind": "weekly", "weekdays": [], "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "weekly", "weekdays": [8], "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "weekly", "weekdays": [1], "start": "01:00", "end": "01:00", "timezone": "UTC"}),
            json!({"kind": "weekly", "weekdays": [1], "start": "24:00", "end": "01:00", "timezone": "UTC"}),
            json!({"kind": "weekly", "weekdays": [1], "start": "01:00", "end": "02:00", "timezone": ""}),
            json!({"kind": "once", "starts_at": "2026-10-24T10:00:00Z", "ends_at": "2026-10-24T10:00:00Z"}),
        ] {
            let mut window: Window = serde_json::from_value(bad.clone()).unwrap();
            assert!(window.validate().is_err(), "{bad}");
        }
        assert!(serde_json::from_value::<Window>(json!({"kind": "daily"})).is_err());
    }

    #[test]
    fn monthly_windows_take_a_day_or_a_weekday_of_a_week() {
        let mut second_tuesday: Window = serde_json::from_value(json!({
            "kind": "monthly", "month_week": 2, "weekday": 2, "start": "22:00", "end": "00:00",
            "timezone": "Europe/Paris"
        }))
        .unwrap();
        second_tuesday.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&second_tuesday).unwrap(),
            json!({"kind": "monthly", "month_day": null, "month_week": 2, "weekday": 2,
                   "months": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
                   "start": "22:00", "end": "24:00", "timezone": "Europe/Paris"})
        );
        let mut yearly: Window = serde_json::from_value(json!({
            "kind": "monthly", "month_day": 31, "months": [12, 12], "start": "01:00",
            "end": "03:00", "timezone": "UTC"
        }))
        .unwrap();
        yearly.validate().unwrap();
        assert!(matches!(yearly, Window::Monthly { ref months, .. } if months == &[12]));
        for bad in [
            json!({"kind": "monthly", "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_day": 32, "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_day": 1, "weekday": 1, "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_week": 6, "weekday": 1, "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_week": 1, "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_day": 1, "months": [13], "start": "01:00", "end": "02:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_day": 1, "start": "01:00", "end": "01:00", "timezone": "UTC"}),
            json!({"kind": "monthly", "month_day": 1, "start": "01:00", "end": "02:00", "timezone": ""}),
        ] {
            let mut window: Window = serde_json::from_value(bad.clone()).unwrap();
            assert!(window.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn specs_encode_to_apply_updates() {
        let spec = UpdateSpec {
            packages: vec!["libc6".into()],
            security_only: true,
            ..UpdateSpec::default()
        };
        spec.validate().unwrap();
        let decoded = ApplyUpdates::decode(spec.payload().as_slice()).unwrap();
        assert_eq!(decoded.packages, ["libc6"]);
        assert!(decoded.security_only && !decoded.allow_reboot);
        let bad = UpdateSpec {
            packages: vec!["-o".into()],
            ..UpdateSpec::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn older_specs_are_os_only() {
        let spec: UpdateSpec = serde_json::from_value(
            json!({"packages": [], "security_only": true, "allow_reboot": false}),
        )
        .unwrap();
        assert!(spec.include_os && spec.third_party.is_empty());
        let decoded = ApplyUpdates::decode(spec.payload().as_slice()).unwrap();
        assert!(!decoded.skip_os && decoded.third_party.is_empty());
        assert_eq!(
            serde_json::to_value(UpdateSpec::default()).unwrap(),
            json!({"packages": [], "security_only": false, "allow_reboot": false,
                   "include_os": true, "third_party": [], "classifications": [], "hold": []})
        );
    }

    #[test]
    fn software_specs_are_checked_and_encoded() {
        let spec: SoftwareSpec = serde_json::from_value(json!({
            "action": "install",
            "packages": [{"catalog_id": Uuid::nil(), "name": "7-Zip", "manager": "winget",
                          "id": "7zip.7zip", "source": "winget"}]
        }))
        .unwrap();
        spec.validate().unwrap();
        let change = spec
            .change_for("windows", true, &MsiSources::default())
            .unwrap();
        assert_eq!(change.action, i32::from(software_change::Action::Install));
        assert_eq!(change.manager(), PackageManager::Winget);
        assert_eq!(change.packages[0].id, "7zip.7zip");
        assert!(change.allow_reboot);
        assert_eq!(
            spec.change_for("debian", false, &MsiSources::default()),
            Err(NO_SOFTWARE_FOR.into())
        );
        let remove = SoftwareSpec {
            action: "remove".into(),
            ..spec.clone()
        };
        assert_eq!(
            remove
                .change_for("windows", false, &MsiSources::default())
                .unwrap()
                .action,
            i32::from(software_change::Action::Remove)
        );
        for bad in [
            SoftwareSpec {
                action: "upgrade".into(),
                ..spec.clone()
            },
            SoftwareSpec {
                packages: vec![],
                ..spec.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert_eq!(software_refusal(Some("accepted"), "debian"), None);
        for state in [Some("off"), Some("unsupported"), None] {
            assert!(software_refusal(state, "windows").is_some());
            assert!(software_refusal(state, "rhel").is_some());
        }
        assert!(software_refusal(Some("off"), "debian")
            .unwrap()
            .contains("MF_SOFTWARE_FROM_WEB=1"));
    }

    #[test]
    fn msi_packages_download_from_the_relay_then_the_central_server() {
        let code = "{6F1C2E7A-3B9D-4C55-A0E2-8D4F7B1A9C30}";
        let spec: SoftwareSpec = serde_json::from_value(json!({
            "action": "install",
            "packages": [
                {"catalog_id": Uuid::nil(), "name": "7-Zip", "manager": "winget",
                 "id": "7zip.7zip", "source": "winget"},
                {"catalog_id": Uuid::nil(), "name": "Tiny", "manager": "msi", "id": code,
                 "source": "", "msi": {"sha256": "ab".repeat(32), "size": 4096,
                 "properties": [{"name": "MARKER", "value": "x"}]}},
            ]
        }))
        .unwrap();
        spec.validate().unwrap();
        assert_eq!(spec.managers(), [PackageManager::Winget]);
        let sources = MsiSources::new(
            Some("http://relay.example:7480/"),
            Some("http://mf.example:7480"),
            true,
        );
        let change = spec.change_for("windows", false, &sources).unwrap();
        assert_eq!(change.packages[0].msi, None);
        let file = change.packages[1].msi.as_ref().unwrap();
        assert_eq!(change.packages[1].id, code);
        assert_eq!(
            file.urls,
            [
                format!("http://relay.example:7480/software/{}.msi", "ab".repeat(32)),
                format!("http://mf.example:7480/software/{}.msi", "ab".repeat(32)),
            ]
        );
        assert_eq!((file.size, file.name.as_str()), (4096, "Tiny"));
        assert_eq!(file.properties[0].name, "MARKER");
        software::check_change(&change, Some("windows")).unwrap();
        // Linux servers take none of it.
        assert_eq!(
            spec.change_for("debian", false, &sources),
            Err(NO_SOFTWARE_FOR.into())
        );
        // An agent that predates MSI packages, or nowhere to download from.
        let old = MsiSources {
            known: false,
            ..sources.clone()
        };
        assert_eq!(
            spec.change_for("windows", false, &old),
            Err(MSI_TOO_OLD.into())
        );
        let nowhere = MsiSources::new(None, None, true);
        assert_eq!(
            spec.change_for("windows", false, &nowhere),
            Err(NO_MSI_URL.into())
        );
        // A removal needs no URL and carries the ProductCode only.
        let remove = SoftwareSpec {
            action: "remove".into(),
            ..spec.clone()
        };
        let change = remove.change_for("windows", false, &nowhere).unwrap();
        let file = change.packages[1].msi.as_ref().unwrap();
        assert!(file.sha256.is_empty() && file.urls.is_empty() && file.properties.is_empty());
        software::check_change(&change, Some("windows")).unwrap();
        // Only MSI packages have a file, and they have one.
        let mut bad = spec.clone();
        bad.packages[1].msi = None;
        assert!(bad.validate().is_err());
        let mut bad = spec.clone();
        bad.packages[0].msi = spec.packages[1].msi.clone();
        assert!(bad.validate().is_err());
        let mut bad = spec;
        bad.packages[1].msi.as_mut().unwrap().properties[0].name = "REBOOT".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn each_server_gets_the_packages_of_its_manager() {
        let item = |manager: &str, id: &str, source: &str, mirror: Option<&str>| SoftwareItem {
            catalog_id: Uuid::new_v4(),
            name: id.into(),
            manager: manager.into(),
            id: id.into(),
            source: source.into(),
            mirror: mirror.map(Into::into),
            msi: None,
        };
        let spec = SoftwareSpec {
            action: "install".into(),
            packages: vec![
                item("winget", "Git.Git", "winget", None),
                item("apt", "nginx", "", Some("nginx-debian")),
                item("dnf", "nginx", "", None),
                item("apt", "git", "", Some("nginx-debian")),
            ],
        };
        spec.validate().unwrap();
        assert_eq!(
            spec.managers(),
            [
                PackageManager::Winget,
                PackageManager::Apt,
                PackageManager::Dnf
            ]
        );
        assert_eq!(spec.mirrors(), ["nginx-debian"]);
        let debian = spec
            .change_for("debian", false, &MsiSources::default())
            .unwrap();
        assert_eq!(debian.manager(), PackageManager::Apt);
        let names: Vec<&str> = debian.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(names, ["nginx", "git"]);
        assert_eq!(
            spec.change_for("rhel", false, &MsiSources::default())
                .unwrap()
                .packages
                .len(),
            1
        );
        assert!(spec
            .change_for("linux", false, &MsiSources::default())
            .is_err());
        // Never a source for apt, a mirror for winget or an unknown manager.
        for bad in [
            item("apt", "nginx", "winget", None),
            item("winget", "Git.Git", "winget", Some("m")),
            item("snap", "lxd", "", None),
            item("dnf", "-y", "", None),
        ] {
            let spec = SoftwareSpec {
                action: "install".into(),
                packages: vec![bad.clone()],
            };
            assert!(spec.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn approvals_are_checked_and_fitted_to_each_server() {
        let spec: UpdateSpec = serde_json::from_value(json!({
            "classifications": ["Security Updates", "Critical Updates"],
            "hold": ["KB5034441"],
            "third_party": [{"manager": "winget", "hold": ["Mozilla.Firefox"]},
                            {"manager": "snap"}]
        }))
        .unwrap();
        spec.validate().unwrap();
        assert!(spec.has_approvals());
        let windows = spec.for_server("windows", true, true).unwrap();
        assert_eq!(windows.classifications.len(), 2);
        assert_eq!(windows.hold, ["KB5034441"]);
        assert_eq!(windows.third_party[0].hold, ["Mozilla.Firefox"]);
        // A Windows agent that predates approvals would install what is held.
        assert_eq!(
            spec.for_server("windows", true, false),
            Err(NO_APPROVALS.into())
        );
        // Linux has no classifications or KB numbers: left out.
        let debian = spec.for_server("debian", true, false).unwrap();
        assert!(debian.classifications.is_empty() && debian.hold.is_empty());
        assert!(!UpdateSpec::default().has_approvals());
        for bad in [
            json!({"classifications": ["Drivers"]}),
            json!({"hold": ["libc6"]}),
            json!({"packages": ["KB1"], "hold": ["kb1"]}),
            json!({"third_party": [{"manager": "snap", "hold": ["firefox"]}]}),
        ] {
            let spec: UpdateSpec = serde_json::from_value(bad.clone()).unwrap();
            assert!(spec.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn third_party_specs_are_checked_and_fitted_to_each_server() {
        let spec: UpdateSpec = serde_json::from_value(json!({
            "include_os": false,
            "third_party": [{"manager": "winget", "packages": ["7zip.7zip"]}, {"manager": "snap"}]
        }))
        .unwrap();
        spec.validate().unwrap();
        let windows = spec.for_server("windows", true, true).unwrap();
        assert!(windows.skip_os);
        assert_eq!(windows.third_party.len(), 1);
        assert_eq!(windows.third_party[0].manager(), PackageManager::Winget);
        assert_eq!(windows.third_party[0].packages, ["7zip.7zip"]);
        let debian = spec.for_server("debian", true, true).unwrap();
        assert_eq!(debian.third_party[0].manager(), PackageManager::Snap);
        assert_eq!(
            spec.for_server("windows", false, true),
            Err(NO_THIRD_PARTY.into())
        );

        let flatpak_only = UpdateSpec {
            include_os: false,
            third_party: vec![ThirdPartySpec {
                manager: "flatpak".into(),
                packages: vec![],
                hold: vec![],
            }],
            ..UpdateSpec::default()
        };
        assert!(flatpak_only.for_server("windows", true, true).is_err());

        // With the OS kept, an older agent gets the OS part alone.
        let both = UpdateSpec {
            third_party: vec![ThirdPartySpec {
                manager: "flatpak".into(),
                packages: vec![],
                hold: vec![],
            }],
            ..UpdateSpec::default()
        };
        let older = both.for_server("debian", false, true).unwrap();
        assert!(!older.skip_os && older.third_party.is_empty());
        assert_eq!(
            both.for_server("debian", true, true)
                .unwrap()
                .third_party
                .len(),
            1
        );

        for bad in [
            json!({"include_os": false}),
            json!({"include_os": false, "packages": ["libc6"], "third_party": [{"manager": "snap"}]}),
            json!({"third_party": [{"manager": "chocolatey"}]}),
            json!({"third_party": [{"manager": "snap"}, {"manager": "snap"}]}),
            json!({"third_party": [{"manager": "snap", "packages": ["--classic"]}]}),
        ] {
            let spec: UpdateSpec = serde_json::from_value(bad.clone()).unwrap();
            assert!(spec.validate().is_err(), "{bad}");
        }
        assert!(serde_json::from_value::<UpdateSpec>(
            json!({"third_party": [{"manager": "snap", "scope": "user"}]})
        )
        .is_err());
    }

    #[test]
    fn waves_are_planned_in_target_order() {
        let t = ids(10);
        assert_eq!(plan_waves(&t, &[]).unwrap(), vec![t.clone()]);
        let specs: Vec<WaveSpec> = serde_json::from_value(json!([
            {"servers": [t[9]]}, {"size": 2}, {"percent": 30}
        ]))
        .unwrap();
        let waves = plan_waves(&t, &specs).unwrap();
        assert_eq!(
            waves,
            vec![
                vec![t[9]],
                t[0..2].to_vec(),
                t[2..5].to_vec(),
                t[5..9].to_vec()
            ]
        );
        // Rounded up, and nothing left over.
        let waves = plan_waves(&t[..3], &[WaveSpec::Percent(50), WaveSpec::Size(5)]).unwrap();
        assert_eq!(waves, vec![t[0..2].to_vec(), vec![t[2]]]);
    }

    #[test]
    fn bad_wave_plans_are_refused() {
        let t = ids(3);
        let other = Uuid::from_u128(99);
        let cases = [
            (
                vec![WaveSpec::Servers(vec![other])],
                PlanError::NotATarget(other, 0),
            ),
            (
                vec![WaveSpec::Servers(vec![t[0]]), WaveSpec::Servers(vec![t[0]])],
                PlanError::Duplicate(t[0]),
            ),
            (
                vec![WaveSpec::Size(3), WaveSpec::Size(1)],
                PlanError::EmptyWave(1),
            ),
            (vec![WaveSpec::Size(0)], PlanError::EmptyWave(0)),
            (vec![WaveSpec::Percent(0)], PlanError::Percent),
            (vec![WaveSpec::Servers(vec![])], PlanError::EmptyWave(0)),
        ];
        for (specs, error) in cases {
            assert_eq!(plan_waves(&t, &specs), Err(error));
        }
        let many: Vec<Uuid> = ids(MAX_WAVES as u128 + 1);
        let specs = vec![WaveSpec::Size(1); MAX_WAVES + 1];
        assert_eq!(plan_waves(&many, &specs), Err(PlanError::TooManyWaves));
    }

    #[test]
    fn pins_need_published_snapshots_once_per_mirror() {
        let pin = |mirrors: &[(&str, &str)]| SourcesPin {
            mode: SourcesMode::Replace,
            mirrors: mirrors
                .iter()
                .map(|(name, snapshot)| PinnedMirror {
                    name: (*name).into(),
                    snapshot: (*snapshot).into(),
                })
                .collect(),
        };
        assert!(pin(&[("debian-12", "s1"), ("rocky-9", "s2")])
            .validate()
            .is_ok());
        assert!(pin(&[]).validate().is_err());
        assert!(pin(&[("debian-12", "current")])
            .validate()
            .unwrap_err()
            .contains("not `current`"));
        assert!(pin(&[("debian-12", "s1"), ("debian-12", "s2")])
            .validate()
            .unwrap_err()
            .contains("repeated"));
        let parsed: SourcesPin = serde_json::from_value(serde_json::json!({
            "mode": "add", "mirrors": [{"name": "debian-12", "snapshot": "s1"}]
        }))
        .unwrap();
        assert_eq!(Mode::from(parsed.mode), Mode::Add);
        assert!(serde_json::from_value::<SourcesPin>(serde_json::json!({
            "mirrors": [{"name": "debian-12", "snapshot": "s1"}]
        }))
        .is_err());
    }
}

#[cfg(test)]
mod major_upgrade_tests {
    use super::*;

    #[test]
    fn a_major_upgrade_is_one_known_step() {
        let spec = MajorUpgradeSpec {
            from: "debian 12".into(),
            to: "debian 13".into(),
            accept_limited_rollback: false,
            parallel: 1,
        };
        assert_eq!(spec.validate().unwrap().to_codename, "trixie");
        let two = MajorUpgradeSpec {
            from: "debian 11".into(),
            ..spec.clone()
        };
        assert!(two
            .validate()
            .unwrap_err()
            .contains("one release at a time"));
        let many = MajorUpgradeSpec {
            parallel: 51,
            ..spec.clone()
        };
        assert!(many.validate().is_err());
        let decoded: MajorUpgradeSpec =
            serde_json::from_value(serde_json::json!({"from": "debian 12", "to": "debian 13"}))
                .unwrap();
        assert_eq!(decoded, spec);
        assert_eq!(
            MajorUpgrade::decode(spec.payload().as_slice()).unwrap().to,
            "debian 13"
        );
    }
}
