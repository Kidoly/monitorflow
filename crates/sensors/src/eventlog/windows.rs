//! Reading an Event Log channel with the `Evt*` API of `wevtapi.dll`.
//!
//! Each collection finds the channel's newest record, then queries the
//! records after the last one examined, up to that newest one, with the
//! provider, level and ID filters as XPath. A pattern is matched against
//! the event's message, formatted with its provider's metadata, or
//! against the event's XML when the provider has no message for it.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use tokio::time::{timeout_at, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_EVT_CHANNEL_NOT_FOUND, ERROR_EVT_INVALID_QUERY,
    ERROR_EVT_UNRESOLVED_VALUE_INSERT, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS,
};
use windows_sys::Win32::System::EventLog::{
    EvtClose, EvtCreateRenderContext, EvtFormatMessage, EvtFormatMessageEvent, EvtNext,
    EvtOpenPublisherMetadata, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection, EvtRender,
    EvtRenderContextSystem, EvtRenderEventValues, EvtRenderEventXml, EvtSystemEventRecordId,
    EvtSystemProviderName, EvtVarTypeString, EvtVarTypeUInt64, EVT_HANDLE, EVT_VARIANT,
};

use super::{EventLogSensor, Filter, MAX_EVENTS};
use crate::win::{from_wide, from_wide_ptr, wide};
use crate::{Sample, Sensor, SensorError};

/// Time kept before the deadline, so a slow read is reported as such.
const MARGIN: Duration = Duration::from_millis(200);
/// How long one `EvtNext` may wait, in milliseconds.
const NEXT_TIMEOUT_MS: u32 = 5_000;
/// Bytes of a message that are matched.
const MAX_TEXT: usize = 16 * 1024;
/// Providers whose metadata is kept open.
const MAX_PUBLISHERS: usize = 256;

/// An `Evt*` handle, closed when dropped.
struct Evt(EVT_HANDLE);

impl Drop for Evt {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { EvtClose(self.0) };
    }
}

/// What a sensor keeps between collections.
#[derive(Default)]
pub struct State {
    /// Last record examined; `None` before the first collection.
    last: Option<u64>,
    /// Renders the system properties of an event.
    context: Option<Evt>,
    /// Metadata of the providers seen, `None` when it cannot be opened.
    publishers: HashMap<String, Option<Evt>>,
}

/// What one collection found.
struct Round {
    matches: u64,
    lag: u64,
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn failed(what: &str, channel: &str) -> SensorError {
    let error = last_error();
    let code = error.raw_os_error().unwrap_or(0) as u32;
    let message = format!("{what} on channel `{channel}`: {error}");
    match code {
        ERROR_EVT_CHANNEL_NOT_FOUND | ERROR_ACCESS_DENIED | ERROR_EVT_INVALID_QUERY => {
            SensorError::Permanent(message)
        }
        _ => SensorError::Transient(message),
    }
}

fn query(channel: &str, xpath: &str, flags: u32) -> Result<Evt, SensorError> {
    let (path, xpath) = (wide(channel), wide(xpath));
    // SAFETY: the local session and NUL-terminated strings.
    let handle = unsafe {
        EvtQuery(
            0,
            path.as_ptr(),
            xpath.as_ptr(),
            EvtQueryChannelPath | flags,
        )
    };
    if handle == 0 {
        return Err(failed("EvtQuery", channel));
    }
    Ok(Evt(handle))
}

/// The next event of `results`, or `None` at the end.
fn next(results: &Evt, channel: &str) -> Result<Option<Evt>, SensorError> {
    let (mut event, mut returned) = (0, 0);
    // SAFETY: room for one handle.
    if unsafe { EvtNext(results.0, 1, &mut event, NEXT_TIMEOUT_MS, 0, &mut returned) } == 0 {
        if last_error().raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
            return Ok(None);
        }
        return Err(failed("EvtNext", channel));
    }
    Ok((returned == 1).then_some(Evt(event)))
}

/// Calls `render` with a buffer of `T`, growing it once when too small.
/// Returns the buffer and the bytes used.
fn grow<T: Default + Clone>(
    mut render: impl FnMut(&mut [T], &mut u32) -> bool,
) -> Option<(Vec<T>, u32)> {
    let mut buf = vec![T::default(); 256];
    let mut used = 0;
    if render(&mut buf, &mut used) {
        return Some((buf, used));
    }
    if last_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return None;
    }
    buf = vec![T::default(); (used as usize).div_ceil(std::mem::size_of::<T>())];
    render(&mut buf, &mut used).then_some((buf, used))
}

/// The record ID and provider name of `event`.
fn system_values(context: &Evt, event: &Evt) -> Option<(u64, String)> {
    let (values, used) = grow::<EVT_VARIANT>(|buf, used| {
        let mut count = 0;
        let size = std::mem::size_of_val(buf) as u32;
        // SAFETY: the buffer and its size in bytes match.
        unsafe {
            EvtRender(
                context.0,
                event.0,
                EvtRenderEventValues,
                size,
                buf.as_mut_ptr().cast(),
                used,
                &mut count,
            ) != 0
        }
    })?;
    let count = (used as usize / std::mem::size_of::<EVT_VARIANT>()).min(values.len());
    let value = |index: i32| values[..count].get(index as usize);
    let id = value(EvtSystemEventRecordId).filter(|v| v.Type == EvtVarTypeUInt64 as u32)?;
    // SAFETY: the type says which field of the union is set.
    let id = unsafe { id.Anonymous.UInt64Val };
    let provider = match value(EvtSystemProviderName) {
        // SAFETY: a string value points into the rendered buffer.
        Some(v) if v.Type == EvtVarTypeString as u32 => unsafe {
            from_wide_ptr(v.Anonymous.StringVal)
        },
        _ => String::new(),
    };
    Some((id, provider))
}

/// The newest record ID of the channel, `None` when it is empty.
fn newest(channel: &str, context: &Evt) -> Result<Option<u64>, SensorError> {
    let results = query(channel, "*", EvtQueryReverseDirection)?;
    let Some(event) = next(&results, channel)? else {
        return Ok(None);
    };
    let (id, _) = system_values(context, &event).ok_or_else(|| failed("EvtRender", channel))?;
    Ok(Some(id))
}

/// The event's message, or its XML when there is no message.
fn text(publishers: &mut HashMap<String, Option<Evt>>, provider: &str, event: &Evt) -> Vec<u8> {
    if publishers.len() >= MAX_PUBLISHERS && !publishers.contains_key(provider) {
        publishers.clear();
    }
    let publisher = publishers.entry(provider.to_owned()).or_insert_with(|| {
        let name = wide(provider);
        // SAFETY: the local session and a NUL-terminated name.
        let handle = unsafe { EvtOpenPublisherMetadata(0, name.as_ptr(), std::ptr::null(), 0, 0) };
        (handle != 0).then_some(Evt(handle))
    });
    let message = publisher.as_ref().and_then(|publisher| {
        grow::<u16>(|buf, used| {
            // SAFETY: the buffer and its size in characters match.
            let ok = unsafe {
                EvtFormatMessage(
                    publisher.0,
                    event.0,
                    0,
                    0,
                    std::ptr::null(),
                    EvtFormatMessageEvent,
                    buf.len() as u32,
                    buf.as_mut_ptr(),
                    used,
                )
            } != 0;
            // Missing inserts still give a message.
            ok || last_error().raw_os_error() == Some(ERROR_EVT_UNRESOLVED_VALUE_INSERT as i32)
        })
    });
    let text = message.or_else(|| {
        grow::<u16>(|buf, used| {
            let mut count = 0;
            let size = std::mem::size_of_val(buf) as u32;
            // SAFETY: the buffer and its size in bytes match.
            unsafe {
                EvtRender(
                    0,
                    event.0,
                    EvtRenderEventXml,
                    size,
                    buf.as_mut_ptr().cast(),
                    used,
                    &mut count,
                ) != 0
            }
        })
    });
    let mut text = text
        .map(|(buf, _)| from_wide(&buf))
        .unwrap_or_default()
        .into_bytes();
    text.truncate(MAX_TEXT);
    text
}

/// One collection, on the blocking pool, examining events until `cutoff`.
fn read(
    filter: &Filter,
    state: &mut State,
    cutoff: std::time::Instant,
) -> Result<Round, SensorError> {
    let channel = filter.channel.as_str();
    if state.context.is_none() {
        // SAFETY: no value paths, for the system properties.
        let context =
            unsafe { EvtCreateRenderContext(0, std::ptr::null(), EvtRenderContextSystem) };
        if context == 0 {
            return Err(failed("EvtCreateRenderContext", channel));
        }
        state.context = Some(Evt(context));
    }
    let context = state.context.as_ref().expect("created above");
    let idle = Round { matches: 0, lag: 0 };
    let Some(newest) = newest(channel, context)? else {
        // Empty: events written from now on are new.
        state.last = Some(0);
        return Ok(idle);
    };
    let last = match state.last {
        None => {
            state.last = Some(newest);
            return Ok(idle);
        }
        // Cleared since: read again from the start.
        Some(last) if last > newest => 0,
        Some(last) => last,
    };
    if last == newest {
        return Ok(idle);
    }
    let results = query(channel, &filter.xpath(last, newest), 0)?;
    let (mut seen, mut matches, mut examined) = (last, 0, 0);
    loop {
        if examined == MAX_EVENTS || std::time::Instant::now() >= cutoff {
            break;
        }
        let Some(event) = next(&results, channel)? else {
            seen = newest;
            break;
        };
        let (id, provider) =
            system_values(context, &event).ok_or_else(|| failed("EvtRender", channel))?;
        examined += 1;
        seen = id;
        if !filter.needs_text() || filter.matches(&text(&mut state.publishers, &provider, &event)) {
            matches += 1;
        }
    }
    state.last = Some(seen);
    Ok(Round {
        matches,
        lag: newest - seen,
    })
}

#[async_trait::async_trait]
impl Sensor for EventLogSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        // The read stops on its own well before the cutoff, so what it
        // examined is reported rather than lost to the timeout.
        let budget = cutoff.saturating_duration_since(now).mul_f64(0.75);
        let (filter, state) = (self.filter.clone(), self.state.clone());
        let read = tokio::task::spawn_blocking(move || {
            let Ok(mut state) = state.try_lock() else {
                return Err(SensorError::Transient(
                    "the previous read is still running".into(),
                ));
            };
            read(&filter, &mut state, std::time::Instant::now() + budget)
        });
        let round = match timeout_at(cutoff, read).await {
            Ok(Ok(round)) => round?,
            Ok(Err(e)) => return Err(SensorError::Transient(format!("event read failed: {e}"))),
            Err(_) => {
                return Err(SensorError::Transient(
                    "event read did not finish in time".into(),
                ))
            }
        };
        let sample = |metric: &str, value: f64| {
            Sample::metric(metric, value).with_label("channel", &self.filter.channel)
        };
        Ok(vec![
            sample("matches", round.matches as f64),
            sample("lag_events", round.lag as f64),
        ])
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;

    use super::*;
    use crate::registry::{build_local, Local};

    fn in_secs(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    fn values(samples: &[Sample]) -> (f64, f64) {
        assert_eq!(samples[0].sensor, "matches");
        assert_eq!(samples[0].labels["channel"], "Application");
        (samples[0].value, samples[1].value)
    }

    /// Writes an Application event with `eventcreate`, provider
    /// `MonitorFlowTest`, as an administrator (the CI runner is one).
    fn write_event(id: u16, message: &str) {
        let status = Command::new("eventcreate")
            .args([
                "/L",
                "APPLICATION",
                "/T",
                "ERROR",
                "/SO",
                "MonitorFlowTest",
                "/ID",
            ])
            .arg(id.to_string())
            .args(["/D", message])
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    }

    #[tokio::test]
    async fn counts_new_matching_events() {
        let params = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let local = Local::default();
        let by_id = build_local(
            "eventlog",
            &params(&[
                ("channel", "Application"),
                ("providers", "MonitorFlowTest"),
                ("levels", "error"),
                ("event_ids", "901"),
            ]),
            &local,
        )
        .unwrap();
        let by_text = build_local(
            "eventlog",
            &params(&[
                ("channel", "Application"),
                ("providers", "MonitorFlowTest"),
                ("pattern", "backup (failed|stalled)"),
                ("exclude", "drill"),
            ]),
            &local,
        )
        .unwrap();
        write_event(901, "before the first collection");
        // The first collection only finds where the channel ends.
        assert_eq!(
            values(&by_id.collect(in_secs(30)).await.unwrap()),
            (0.0, 0.0)
        );
        assert_eq!(
            values(&by_text.collect(in_secs(30)).await.unwrap()),
            (0.0, 0.0)
        );
        write_event(901, "backup failed on disk 2");
        write_event(902, "backup stalled");
        write_event(902, "backup failed during the drill");
        // Writes reach readers through the Event Log service.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            values(&by_id.collect(in_secs(30)).await.unwrap()),
            (1.0, 0.0)
        );
        assert_eq!(
            values(&by_text.collect(in_secs(30)).await.unwrap()),
            (2.0, 0.0)
        );
        assert_eq!(
            values(&by_id.collect(in_secs(30)).await.unwrap()),
            (0.0, 0.0)
        );
    }

    #[tokio::test]
    async fn missing_channels_are_permanent() {
        let local = Local {
            event_channels: vec!["MonitorFlow-Missing/Operational".into()],
            ..Local::default()
        };
        let params = BTreeMap::from([(
            "channel".to_string(),
            "MonitorFlow-Missing/Operational".to_string(),
        )]);
        let sensor = build_local("eventlog", &params, &local).unwrap();
        let error = sensor.collect(in_secs(30)).await.unwrap_err();
        assert!(error.is_permanent(), "{error}");
    }
}
