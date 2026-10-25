//! The error that stops the Windows service, written to the Application
//! event log under the source the MSI package registers (ADR-0050). The
//! service logs everything else to `agent.log`; this covers failures
//! before that file is open, such as an invalid configuration.

use std::ptr;

use windows_sys::Win32::System::EventLog::{
    DeregisterEventSource, RegisterEventSourceW, ReportEventW, EVENTLOG_ERROR_TYPE,
};

/// Event source, the service's name.
pub const SOURCE: &str = "MonitorFlowAgent";
/// Event ID of "the agent service stopped with an error". The source's
/// message file formats every ID as its one string.
pub const SERVICE_FAILED: u32 = 1;
/// UTF-16 units of one event string; Windows refuses longer ones.
const MAX_STRING: usize = 31_839;

/// Writes `message` as an error event. Best effort: the service is
/// stopping, and the control manager logs its exit code anyway.
pub fn report_error(message: &str) {
    let source = wide(SOURCE, MAX_STRING);
    let message = wide(message, MAX_STRING);
    let strings = [message.as_ptr()];
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the
    // calls, `strings` holds the one pointer `wnumstrings` announces, and
    // there is no SID or raw data. The handle is only used between a
    // successful RegisterEventSourceW and its DeregisterEventSource.
    unsafe {
        let log = RegisterEventSourceW(ptr::null(), source.as_ptr());
        if log.is_null() {
            return;
        }
        ReportEventW(
            log,
            EVENTLOG_ERROR_TYPE,
            0,
            SERVICE_FAILED,
            ptr::null_mut(),
            1,
            0,
            strings.as_ptr(),
            ptr::null(),
        );
        DeregisterEventSource(log);
    }
}

/// `text` as NUL-terminated UTF-16, cut to `max` units before the NUL.
fn wide(text: &str, max: usize) -> Vec<u16> {
    let mut units: Vec<u16> = text.encode_utf16().take(max).collect();
    units.push(0);
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_cut_and_terminated() {
        assert_eq!(wide("ab", 10), [97, 98, 0]);
        assert_eq!(wide("abc", 2), [97, 98, 0]);
    }

    #[test]
    fn reporting_never_fails() {
        report_error("monitorflow-agent unit test: report_error works");
    }
}
