//! The Service Control Manager, through `QueryServiceStatusEx`.

use std::io;

use tokio::time::{timeout_at, Instant};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_SERVICE_DOES_NOT_EXIST};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
    SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS,
};

use super::{Scm, ServiceStatus};
use crate::win::wide;
use crate::SensorError;

/// The local Service Control Manager.
#[derive(Debug, Default)]
pub struct Manager;

/// Closes a service or manager handle when dropped.
struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn transient(what: &str) -> SensorError {
    SensorError::Transient(format!("{what}: {}", io::Error::last_os_error()))
}

/// Queries `services` with a connection opened for this round.
pub fn query_all(services: &[String]) -> Result<Vec<Option<ServiceStatus>>, SensorError> {
    // SAFETY: the local manager's default database.
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(transient("OpenSCManagerW"));
    }
    let manager = Handle(manager);
    let mut states = Vec::with_capacity(services.len());
    for service in services {
        let name = wide(service);
        // SAFETY: an open manager and a NUL-terminated name.
        let handle = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_QUERY_STATUS) };
        if handle.is_null() {
            // SAFETY: no arguments; read right after the failed call.
            if unsafe { GetLastError() } == ERROR_SERVICE_DOES_NOT_EXIST {
                states.push(None);
                continue;
            }
            return Err(transient(&format!("OpenServiceW {service}")));
        }
        let handle = Handle(handle);
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0;
        // SAFETY: the buffer is a SERVICE_STATUS_PROCESS of the given size.
        let ok = unsafe {
            QueryServiceStatusEx(
                handle.0,
                SC_STATUS_PROCESS_INFO,
                (&raw mut status).cast(),
                std::mem::size_of_val(&status) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(transient(&format!("QueryServiceStatusEx {service}")));
        }
        states.push(Some(ServiceStatus {
            state: status.dwCurrentState,
            exit_code: status.dwWin32ExitCode,
            specific_exit_code: status.dwServiceSpecificExitCode,
        }));
    }
    Ok(states)
}

#[async_trait::async_trait]
impl Scm for Manager {
    async fn query(
        &self,
        services: &[String],
        deadline: Instant,
    ) -> Result<Vec<Option<ServiceStatus>>, SensorError> {
        let services = services.to_vec();
        // RPCs to services.exe: off the runtime, bounded by the deadline.
        let query = tokio::task::spawn_blocking(move || query_all(&services));
        match timeout_at(deadline, query).await {
            Ok(Ok(states)) => states,
            Ok(Err(e)) => Err(SensorError::Transient(format!("service query failed: {e}"))),
            Err(_) => Err(SensorError::Transient(
                "the Service Control Manager did not answer in time".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::Sensor;

    #[tokio::test]
    async fn queries_this_host() {
        let services = ["EventLog", "eventlog", "mf-not-installed"].map(String::from);
        let states = query_all(&services).unwrap();
        // The Event Log service always runs; names ignore case.
        assert_eq!(states[0].map(|s| s.state), Some(4));
        assert_eq!(states[1], states[0]);
        assert_eq!(states[2], None);
        let sensor = super::super::ServiceSensor::new(services[..1].to_vec(), Manager);
        let deadline = Instant::now() + Duration::from_secs(10);
        let samples = sensor.collect(deadline).await.unwrap();
        assert_eq!(samples[1].sensor, "active");
        assert_eq!(samples[1].value, 1.0);
    }
}
