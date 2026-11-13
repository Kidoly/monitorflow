//! The update service's named pipe (ADR-0049): the server, created with a
//! protected DACL for SYSTEM and the agent's service SID only, and the
//! agent's client.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use tokio::sync::Mutex;
use windows_sys::Win32::Foundation::{LocalFree, ERROR_PIPE_BUSY, ERROR_SUCCESS};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

use super::client::UpdateService;
use super::{
    check_pipe_owner, pipe_taken, read_frame, write_frame, Reply, Request, MAX_REPLY_BYTES,
    MAX_REQUEST_BYTES, TRUSTED_PIPE_OWNERS,
};
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

/// The owner SID of `pipe`, as a string such as `S-1-5-18`. Needs `READ_CONTROL`, which a read handle has.
pub fn owner_sid(pipe: &impl AsRawHandle) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle of `pipe` stays open while it is borrowed.
    // `owner` points into `descriptor`, which GetSecurityInfo allocates
    // and which is freed with LocalFree below, after the last use of
    // `owner`; the other out pointers are null, as allowed.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `owner` is the valid SID inside `descriptor`, still
    // allocated; `text` is allocated by the call and freed with LocalFree.
    let converted = unsafe { ConvertSidToStringSidW(owner, &mut text) };
    let result = if converted == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: on success `text` is a NUL-terminated UTF-16 string.
        let units = unsafe {
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            std::slice::from_raw_parts(text, len)
        };
        let sid = String::from_utf16_lossy(units);
        // SAFETY: allocated by ConvertSidToStringSidW and no longer used.
        unsafe { LocalFree(text.cast()) };
        Ok(sid)
    };
    // SAFETY: allocated by GetSecurityInfo; `owner` is not used after this.
    unsafe { LocalFree(descriptor) };
    result
}

/// The process holding the server end of `pipe`.
fn server_pid(pipe: &impl AsRawHandle) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: the handle of `pipe` stays open while it is borrowed, and
    // `pid` is a valid u32 the call writes.
    (unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } != 0).then_some(pid)
}

/// Serves `name` with the DACL `sddl` until `shutdown`: one request per
/// connection, answered by `handle` on the blocking pool, one at a time.
pub async fn serve<H>(name: &str, sddl: &str, handle: H, mut shutdown: Shutdown) -> io::Result<()>
where
    H: Fn(&Request) -> Reply + Send + Sync + 'static,
{
    let handle = Arc::new(handle);
    let one_at_a_time = Arc::new(Mutex::new(()));
    // Access denied, or busy if the holder allowed one instance: the
    // name is taken.
    let mut server = create(name, sddl, true).map_err(|error| {
        let taken = error.kind() == io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32);
        if !taken {
            return error;
        }
        let message = pipe_taken(name, &error);
        tracing::error!(pipe = name, %error, "{message}");
        io::Error::new(error.kind(), message)
    })?;
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
    /// Owner SIDs the pipe may have; see [`TRUSTED_PIPE_OWNERS`].
    owners: Vec<String>,
}

impl PipeClient {
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_owners(name, TRUSTED_PIPE_OWNERS.map(String::from).to_vec())
    }

    /// A client that accepts a pipe owned by one of `owners`.
    pub fn with_owners(name: impl Into<String>, owners: Vec<String>) -> Self {
        Self {
            name: name.into(),
            owners,
        }
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
        // While the service is stopped, any user may create the pipe; only
        // SYSTEM or an administrator can create one these owners have.
        let owner =
            owner_sid(&pipe).map_err(|e| format!("cannot read the owner of {}: {e}", self.name))?;
        let owners: Vec<&str> = self.owners.iter().map(String::as_str).collect();
        if let Err(error) = check_pipe_owner(&self.name, &owner, server_pid(&pipe), &owners) {
            tracing::error!(%error, "the update service's pipe is held by another process");
            return Err(error);
        }
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

    /// The owner of the pipes this process creates.
    fn own_owner(test: &str) -> String {
        let probe = create(&unique(&format!("{test}-probe")), &pipe_sddl("WD"), true).unwrap();
        owner_sid(&probe).unwrap()
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
        let client = PipeClient::with_owners(name.clone(), vec![own_owner("answer")]);
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
    async fn a_pipe_another_account_owns_gets_nothing() {
        let name = unique("squatted");
        let (trigger, stop) = crate::shutdown::channel();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server = tokio::spawn({
            let (name, calls) = (name.clone(), calls.clone());
            async move {
                serve(
                    &name,
                    &pipe_sddl("WD"),
                    move |_| {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Reply::Status(Status::default())
                    },
                    stop,
                )
                .await
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let owner = own_owner("squatted");
        // The test runs as neither SYSTEM nor, for this client, a trusted
        // owner: it plays the user who took the name.
        let client = PipeClient::with_owners(name.clone(), vec!["S-1-5-18".into()]);
        let error = client.call(&Request::Status).await.unwrap_err();
        assert!(error.starts_with(&format!("refusing {name}: ")), "{error}");
        assert!(
            error.contains(&format!("process {} owned by {owner}", std::process::id())),
            "{error}"
        );
        trigger.fire();
        server.await.unwrap().unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_taken_name_stops_the_service_with_a_reason() {
        let name = unique("taken");
        let _squatter = create(&name, &pipe_sddl("WD"), true).unwrap();
        let (_trigger, stop) = crate::shutdown::channel();
        let error = serve(
            &name,
            &pipe_sddl("WD"),
            |_| Reply::Status(Status::default()),
            stop,
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("another process holds the name"),
            "{error}"
        );
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
