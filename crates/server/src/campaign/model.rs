//! Campaign values without storage: states and actions, failure
//! thresholds, maintenance windows and the wave plan.

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use monitorflow_common::command::valid_package_name;
use monitorflow_common::proto::v1::ApplyUpdates;
use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Most servers in one campaign.
pub const MAX_TARGETS: usize = 10_000;
/// Most waves in one campaign.
pub const MAX_WAVES: usize = 100;
/// Most maintenance windows in one campaign.
pub const MAX_WINDOWS: usize = 32;
/// Longest package list, as for a single command.
pub const MAX_PACKAGES: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum FailureUnit {
    Count,
    Percent,
}

/// A wave fails when more of its servers failed than this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureThreshold {
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

/// When a campaign may queue commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Window {
    /// Every week on `weekdays` (ISO: 1 is Monday, 7 Sunday), from `start`
    /// to `end` local time in `timezone` (IANA name). An `end` before
    /// `start` closes the next day.
    Weekly {
        weekdays: Vec<u8>,
        start: LocalTime,
        end: LocalTime,
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
            Self::Once { starts_at, ends_at } => {
                if starts_at >= ends_at {
                    return Err("a window ends after it starts".into());
                }
                Ok(())
            }
        }
    }
}

/// The `ApplyUpdates` of every command of a campaign.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSpec {
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub security_only: bool,
    #[serde(default)]
    pub allow_reboot: bool,
}

impl UpdateSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.packages.len() > MAX_PACKAGES {
            return Err(format!("at most {MAX_PACKAGES} packages"));
        }
        if let Some(bad) = self.packages.iter().find(|p| !valid_package_name(p)) {
            return Err(format!("invalid package name `{}`", bad.escape_debug()));
        }
        Ok(())
    }

    pub fn payload(&self) -> Vec<u8> {
        ApplyUpdates {
            packages: self.packages.clone(),
            security_only: self.security_only,
            allow_reboot: self.allow_reboot,
        }
        .encode_to_vec()
    }
}

/// One wave as requested: explicit servers, a number of servers, or a
/// percentage of all targets (rounded up), taken in target order from
/// those no explicit wave names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum WaveSpec {
    Servers(Vec<Uuid>),
    Size(u32),
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
    fn specs_encode_to_apply_updates() {
        let spec = UpdateSpec {
            packages: vec!["libc6".into()],
            security_only: true,
            allow_reboot: false,
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
}
