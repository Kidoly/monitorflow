//! Events of a Windows Event Log channel matching a filter, the analogue
//! of the `log` sensor for Windows, as its own kind. See ADR-0048.
//!
//! The server's configuration names a channel; the agent reads it only if
//! it is one of its own event channels ([`Local::event_channels`],
//! `Application` and `System` by default).
//!
//! Metrics (prefixed with the configured name), with label `channel`:
//! - `matches`: events that matched since the previous collection;
//! - `lag_events`: records of the channel not examined yet, above zero
//!   when more than [`MAX_EVENTS`] arrive per collection.
//!
//! Parameters: `channel` (required, like `System` or
//! `Microsoft-Windows-TaskScheduler/Operational`), and filters that must
//! all hold: `providers` (provider names), `levels` (`critical`, `error`,
//! `warning`, `information`, `verbose`), `event_ids` (numbers), `pattern`
//! (a regular expression of the `regex` crate on the event's message) and
//! `exclude` (events whose message matches it are not counted). Lists are
//! comma-separated; an event matches one item of each list given.
//!
//! The first collection starts after the newest record: history is not
//! counted. A cleared channel is read again from its start. Parameters
//! are checked on every platform so the server can validate them; the
//! sensor only runs on Windows.

use std::collections::BTreeSet;
use std::sync::Arc;

use regex::bytes::Regex;

use crate::pattern::pattern;
use crate::registry::{BuildError, Local, Params};

/// Default [`Local::event_channels`]. `Security` holds logons and other
/// audit events; an administrator adds it explicitly.
pub const DEFAULT_CHANNELS: &[&str] = &["Application", "System"];
/// Most events examined per collection; the rest waits for the next one.
pub const MAX_EVENTS: u64 = 10_000;
/// Most providers, and most event IDs, in a filter. The Event Log refuses
/// queries with too many conditions.
pub const MAX_ITEMS: usize = 16;
/// Longest channel or provider name.
const NAME_MAX: usize = 255;
/// Level names and the `Level` values they stand for; providers log
/// information as 4 or as 0 (`LogAlways`).
const LEVELS: [(&str, &[u8]); 5] = [
    ("critical", &[1]),
    ("error", &[2]),
    ("warning", &[3]),
    ("information", &[0, 4]),
    ("verbose", &[5]),
];

/// A name of `[A-Za-z0-9 ._-]`, and `/` in channels, at most 255 bytes,
/// without leading or trailing space nor leading `/`. Names go into an
/// XPath query, so quotes and brackets must never get through.
fn name(name: &str, slash: bool) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= NAME_MAX
        && name.trim() == name
        && !name.starts_with('/')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c) || (slash && c == '/'));
    ok.then_some(()).ok_or_else(|| {
        format!(
            "`{}` must be at most {NAME_MAX} bytes of [A-Za-z0-9 ._-{}]",
            name.chars().take(64).collect::<String>().escape_debug(),
            if slash { "/" } else { "" }
        )
    })
}

/// What a sensor counts.
#[derive(Debug)]
pub struct Filter {
    channel: String,
    providers: Vec<String>,
    levels: Vec<u8>,
    event_ids: Vec<u16>,
    pattern: Option<Regex>,
    exclude: Option<Regex>,
}

impl Filter {
    /// The Event Log XPath query of the records after `after` up to `upto`
    /// that pass the provider, level and ID filters.
    pub fn xpath(&self, after: u64, upto: u64) -> String {
        let any = |items: Vec<String>| {
            if items.is_empty() {
                String::new()
            } else {
                format!(" and ({})", items.join(" or "))
            }
        };
        format!(
            "*[System[EventRecordID>{after} and EventRecordID<={upto}{}{}{}]]",
            any(self
                .providers
                .iter()
                .map(|p| format!("Provider[@Name='{p}']"))
                .collect()),
            any(self.levels.iter().map(|l| format!("Level={l}")).collect()),
            any(self
                .event_ids
                .iter()
                .map(|i| format!("EventID={i}"))
                .collect()),
        )
    }

    /// Whether matching needs the event's message.
    pub fn needs_text(&self) -> bool {
        self.pattern.is_some() || self.exclude.is_some()
    }

    /// Whether a message passes `pattern` and `exclude`.
    pub fn matches(&self, text: &[u8]) -> bool {
        self.pattern.as_ref().is_none_or(|p| p.is_match(text))
            && !self.exclude.as_ref().is_some_and(|e| e.is_match(text))
    }
}

pub struct EventLogSensor {
    filter: Arc<Filter>,
    #[cfg(windows)]
    state: Arc<std::sync::Mutex<windows::State>>,
}

impl std::fmt::Debug for EventLogSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLogSensor")
            .field("channel", &self.filter.channel)
            .finish_non_exhaustive()
    }
}

impl EventLogSensor {
    pub fn new(filter: Filter) -> Self {
        Self {
            filter: Arc::new(filter),
            #[cfg(windows)]
            state: Arc::default(),
        }
    }

    pub(crate) fn from_params(
        params: &mut Params<'_>,
        local: Option<&Local>,
    ) -> Result<Self, BuildError> {
        let Some(channel) = params.get("channel") else {
            return Err(params.bad("channel", "is required"));
        };
        name(channel, true).map_err(|reason| params.bad("channel", reason))?;
        let providers = params.list("providers")?.unwrap_or_default();
        for provider in &providers {
            name(provider, false).map_err(|reason| params.bad("providers", reason))?;
        }
        let mut levels = BTreeSet::new();
        for level in params.list("levels")?.unwrap_or_default() {
            let values = LEVELS.iter().find(|(n, _)| *n == level).ok_or_else(|| {
                params.bad(
                    "levels",
                    "must list critical, error, warning, information or verbose",
                )
            })?;
            levels.extend(values.1);
        }
        let event_ids = params
            .list("event_ids")?
            .unwrap_or_default()
            .iter()
            .map(|id| id.parse::<u16>())
            .collect::<Result<BTreeSet<u16>, _>>()
            .map_err(|_| params.bad("event_ids", "must list numbers from 0 to 65535"))?;
        if providers.len() > MAX_ITEMS || event_ids.len() > MAX_ITEMS {
            let field = if event_ids.len() > MAX_ITEMS {
                "event_ids"
            } else {
                "providers"
            };
            return Err(params.bad(field, format!("may list at most {MAX_ITEMS} items")));
        }
        let mut regex = |field| match params.get(field) {
            None => Ok(None),
            Some(text) => pattern(text).map(Some).map_err(|r| params.bad(field, r)),
        };
        let pattern = regex("pattern")?;
        let exclude = regex("exclude")?;
        if let Some(local) = local {
            if !local
                .event_channels
                .iter()
                .any(|c| c.eq_ignore_ascii_case(channel))
            {
                return Err(params.refused(format!(
                    "channel `{channel}` is not one of the agent's event channels (sensors.event_channels)"
                )));
            }
        }
        Ok(Self::new(Filter {
            channel: channel.to_owned(),
            providers,
            levels: levels.into_iter().collect(),
            event_ids: event_ids.into_iter().collect(),
            pattern,
            exclude,
        }))
    }
}

#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
#[async_trait::async_trait]
impl crate::Sensor for EventLogSensor {
    async fn collect(
        &self,
        _deadline: tokio::time::Instant,
    ) -> Result<Vec<crate::Sample>, crate::SensorError> {
        Err(crate::SensorError::Permanent(format!(
            "the Event Log channel `{}` can only be read on Windows",
            self.filter.channel
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn build(pairs: &[(&str, &str)], local: Option<&Local>) -> Result<EventLogSensor, BuildError> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut params = Params::new("eventlog", &map)?;
        let sensor = EventLogSensor::from_params(&mut params, local)?;
        params.finish()?;
        Ok(sensor)
    }

    fn bad(pairs: &[(&str, &str)]) -> &'static str {
        match build(pairs, None) {
            Err(BuildError::BadParam { name, .. }) => name,
            other => panic!("{pairs:?}: {other:?}"),
        }
    }

    #[test]
    fn params_are_checked() {
        assert!(build(
            &[("channel", "Microsoft-Windows-TaskScheduler/Operational")],
            None
        )
        .is_ok());
        assert_eq!(bad(&[]), "channel");
        for channel in ["", "Sys'tem", "a]b", "/System", "Sys\ntem", "é"] {
            assert_eq!(bad(&[("channel", channel)]), "channel", "{channel:?}");
        }
        let ch = ("channel", "System");
        assert_eq!(bad(&[ch, ("providers", "a/b")]), "providers");
        assert_eq!(bad(&[ch, ("providers", "x'] or 1=1")]), "providers");
        assert_eq!(bad(&[ch, ("levels", "fatal")]), "levels");
        assert_eq!(bad(&[ch, ("event_ids", "70000")]), "event_ids");
        assert_eq!(bad(&[ch, ("event_ids", "-1")]), "event_ids");
        let many = (0..=MAX_ITEMS)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(bad(&[ch, ("event_ids", &many)]), "event_ids");
        assert_eq!(bad(&[ch, ("pattern", "(")]), "pattern");
        assert_eq!(bad(&[ch, ("exclude", "")]), "exclude");
        assert!(matches!(
            build(&[ch, ("level", "error")], None),
            Err(BuildError::UnknownParam { .. })
        ));
    }

    #[test]
    fn channels_are_limited_by_the_agent() {
        let local = Local::default();
        assert!(build(&[("channel", "system")], Some(&local)).is_ok());
        let error = build(&[("channel", "Security")], Some(&local)).unwrap_err();
        assert!(matches!(error, BuildError::Refused { .. }), "{error}");
        assert!(
            error.to_string().contains("sensors.event_channels"),
            "{error}"
        );
        let wider = Local {
            event_channels: vec!["Security".into()],
            ..Local::default()
        };
        assert!(build(&[("channel", "Security")], Some(&wider)).is_ok());
    }

    #[test]
    fn xpath_queries() {
        let plain = build(&[("channel", "System")], None).unwrap();
        assert_eq!(
            plain.filter.xpath(10, 20),
            "*[System[EventRecordID>10 and EventRecordID<=20]]"
        );
        let full = build(
            &[
                ("channel", "System"),
                ("providers", "Service Control Manager,Disk"),
                ("levels", "information,error,error"),
                ("event_ids", "7034, 7031"),
            ],
            None,
        )
        .unwrap();
        assert_eq!(
            full.filter.xpath(0, 5),
            "*[System[EventRecordID>0 and EventRecordID<=5 \
             and (Provider[@Name='Service Control Manager'] or Provider[@Name='Disk']) \
             and (Level=0 or Level=2 or Level=4) and (EventID=7031 or EventID=7034)]]"
        );
        assert!(!full.filter.needs_text() && full.filter.matches(b"anything"));
    }

    #[test]
    fn messages_are_matched() {
        let sensor = build(
            &[
                ("channel", "System"),
                ("pattern", "(?i)terminated"),
                ("exclude", "as expected"),
            ],
            None,
        )
        .unwrap();
        let filter = &sensor.filter;
        assert!(filter.needs_text());
        assert!(filter.matches(b"The service terminated unexpectedly."));
        assert!(!filter.matches(b"The service terminated as expected."));
        assert!(!filter.matches(b"The service entered the running state."));
    }
}
