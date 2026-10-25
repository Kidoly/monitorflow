//! The update service's named pipe (ADR-0049): the server, created with a
//! protected DACL for SYSTEM and the agent's service SID only, and the
//! agent's client.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use tokio::sync::Mutex;
use windows_sys::Win32::Foundation::{LocalFree, ERROR_PIPE_BUSY};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

use super::client::UpdateService;
use super::{read_frame, write_frame, Reply, Request, MAX_REPLY_BYTES, MAX_REQUEST_BYTES};
use crate::packages::PackageError;
use crate::shutdown::Shutdown;

/// Time a client has to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a client waits for a free pipe instance.
const BUSY_WAIT: Duration = Duration::from_secs(10);

/// Creates an instance of the pipe `name` with the DACL `sddl`, the first
/// one when `first`: it fails if another process holds the name.
pub fn create(name: &str, sddl: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl: Vec<u16> = std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `sddl` is NUL-terminated UTF-16 that outlives the call;
    // `descriptor` is allocated by it and freed with LocalFree below, as
    // its documentation requires, after the pipe was created.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor
    // lives until the LocalFree after this call, which copies it into the
    // pipe's security.
    let pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
    };
    // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
    // above and no longer used.
    unsafe { LocalFree(descriptor) };
    pipe
}

/// Serves `name` with the DACL `sddl` until `shutdown`: one request per
/// connection, answered by `handle` on the blocking pool, one at a time.
pub async fn serve<H>(name: &str, sddl: &str, handle: H, mut shutdown: Shutdown) -> io::Result<()>
where
    H: Fn(&Request) -> Reply + Send + Sync + 'static,
{
    let handle = Arc::new(handle);
    let one_at_a_time = Arc::new(Mutex::new(()));
    let mut server = create(name, sddl, true)?;
    tracing::info!(pipe = name, "update service listening");
    loop {
        tokio::select! {
            connected = server.connect() => connected?,
            () = shutdown.wait() => return Ok(()),
        }
        let client = std::mem::replace(&mut server, create(name, sddl, false)?);
        let (handle, lock) = (handle.clone(), one_at_a_time.clone());
        tokio::spawn(async move {
            if let Err(error) = answer(client, handle, lock).await {
                tracing::warn!(%error, "update service request failed");
            }
        });
    }
}

async fn answer<H>(
    mut client: NamedPipeServer,
    handle: Arc<H>,
    lock: Arc<Mutex<()>>,
) -> Result<(), super::FrameError>
where
    H: Fn(&Request) -> Reply + Send + Sync + 'static,
{
    let request: Result<Request, _> =
        tokio::time::timeout(REQUEST_TIMEOUT, read_frame(&mut client, MAX_REQUEST_BYTES))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no request in time"))?;
    let reply = match request {
        Err(error) => Reply::Error(format!("invalid request: {error}")),
        Ok(request) => match request.validate() {
            Err(error) => Reply::Error(error.to_string()),
            Ok(()) => {
                tracing::info!(?request, "update service request");
                let _turn = lock.lock().await;
                tokio::task::spawn_blocking(move || handle(&request))
                    .await
                    .unwrap_or_else(|e| Reply::Error(format!("the request panicked: {e}")))
            }
        },
    };
    if let Reply::Error(error) = &reply {
        tracing::warn!(%error, "update service request refused or failed");
    }
    write_frame(&mut client, &reply).await
}

/// The agent's side of the pipe.
pub struct PipeClient {
    name: String,
}

impl PipeClient {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    async fn call(&self, request: &Request) -> Result<Reply, String> {
        let deadline = tokio::time::Instant::now() + BUSY_WAIT;
        // The default security QoS is SECURITY_IDENTIFICATION: whoever
        // holds the name cannot impersonate the agent.
        let mut pipe = loop {
            match ClientOptions::new().open(&self.name) {
                Ok(pipe) => break pipe,
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(format!(
                        "the update service is not running (no pipe {}); install the {} service",
                        self.name,
                        super::SERVICE_NAME
                    ))
                }
                Err(e) => return Err(format!("cannot open {}: {e}", self.name)),
            }
        };
        write_frame(&mut pipe, request)
            .await
            .map_err(|e| format!("cannot send to the update service: {e}"))?;
        read_frame(&mut pipe, MAX_REPLY_BYTES)
            .await
            .map_err(|e| format!("no reply from the update service: {e}"))
    }
}

#[async_trait::async_trait]
impl UpdateService for PipeClient {
    async fn call(&self, request: &Request) -> Result<Reply, PackageError> {
        PipeClient::call(self, request)
            .await
            .map_err(PackageError::Helper)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{pipe_sddl, Status};
    use super::*;

    fn unique(name: &str) -> String {
        format!(r"\\.\pipe\monitorflow-test-{name}-{}", std::process::id())
    }

    #[tokio::test]
    async fn requests_are_answered_and_checked() {
        let name = unique("answer");
        let (trigger, stop) = crate::shutdown::channel();
        // Everyone may open this one, so the test process can.
        let server = tokio::spawn({
            let name = name.clone();
            async move {
                serve(
                    &name,
                    &pipe_sddl("WD"),
                    |r| match r {
                        Request::Status => Reply::Status(Status {
                            reboot_required: true,
                        }),
                        _ => Reply::Error("unexpected".into()),
                    },
                    stop,
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = PipeClient::new(name.clone());
        for _ in 0..3 {
            let reply = client.call(&Request::Status).await.unwrap();
            assert_eq!(
                reply,
                Reply::Status(Status {
                    reboot_required: true
                })
            );
        }
        // Checked by the service before the handler sees it.
        let bad = Request::Install {
            kind: super::super::Kind::Named,
            updates: vec!["openssl".into()],
        };
        assert_eq!(
            client.call(&bad).await.unwrap(),
            Reply::Error("`openssl` is not a KB number or update ID".into())
        );
        // A second server cannot take the name.
        assert!(create(&name, &pipe_sddl("WD"), true).is_err());
        trigger.fire();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn the_dacl_admits_only_the_agent_service() {
        let name = unique("dacl");
        // The pipe of the real service: SYSTEM and the agent's service SID.
        let sddl = pipe_sddl(&super::super::service_sid(super::super::AGENT_SERVICE));
        let _server = create(&name, &sddl, true).unwrap();
        // The test runs as neither, administrator or not.
        let error = ClientOptions::new().open(&name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }

    #[tokio::test]
    async fn a_missing_service_says_so() {
        let error = PipeClient::new(unique("missing"))
            .call(&Request::Scan)
            .await
            .unwrap_err();
        assert!(
            error.starts_with("the update service is not running"),
            "{error}"
        );
    }
}
