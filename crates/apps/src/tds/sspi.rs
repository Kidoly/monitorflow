//! Signing in to SQL Server with a Windows account: SSPI's Negotiate
//! package, Kerberos when the server's name has a service principal
//! name in the domain, NTLM otherwise. Windows only; elsewhere
//! [`Negotiate::new`] says so, as signing in with NTLM from Linux would
//! need MD4 and MD5, which the workspace's ring does not have.
//!
//! The TDS client ([`super::Client`]) only sees [`Sspi`]: tokens in,
//! tokens out, so the exchange is tested on every system.

use super::TdsError;

/// One side of an SSPI exchange.
#[async_trait::async_trait]
pub trait Sspi: Send {
    /// The next token for the server, from its last one (`None` at the
    /// start).
    async fn step(&mut self, challenge: Option<Vec<u8>>) -> Result<Vec<u8>, TdsError>;
}

/// Whose account signs in.
#[derive(Clone, PartialEq, Eq)]
pub enum Account {
    /// The account the agent runs as: the machine's account in the
    /// domain for the Local System account.
    Own,
    /// `DOMAIN\name` or `name@domain`, with its password.
    Named {
        user: String,
        domain: String,
        password: String,
    },
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Own => f.write_str("Own"),
            Self::Named { user, domain, .. } => f
                .debug_struct("Named")
                .field("user", user)
                .field("domain", domain)
                .finish_non_exhaustive(),
        }
    }
}

impl Account {
    /// `DOMAIN\name` or `name@domain` (the domain then stays in the name,
    /// as Windows reads a user principal name).
    pub fn named(account: &str, password: &str) -> Self {
        let (domain, user) = account.split_once('\\').unwrap_or(("", account));
        Self::Named {
            user: user.to_owned(),
            domain: domain.to_owned(),
            password: password.to_owned(),
        }
    }
}

/// The service principal name SQL Server registers for TCP:
/// `MSSQLSvc/host:port`.
pub fn spn(host: &str, port: u16) -> String {
    format!("MSSQLSvc/{host}:{port}")
}

#[cfg(windows)]
pub use windows::Negotiate;

/// Where SSPI is not, a Windows sign-in fails with this.
#[cfg(not(windows))]
#[derive(Debug)]
pub struct Negotiate(std::convert::Infallible);

#[cfg(not(windows))]
impl Negotiate {
    pub fn new(_account: &Account, _spn: &str) -> Result<Self, TdsError> {
        Err(TdsError::Sspi(
            "signing in with a Windows account needs the agent on Windows".into(),
        ))
    }
}

#[cfg(not(windows))]
#[async_trait::async_trait]
impl Sspi for Negotiate {
    async fn step(&mut self, _challenge: Option<Vec<u8>>) -> Result<Vec<u8>, TdsError> {
        match self.0 {}
    }
}

#[cfg(windows)]
mod windows {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        SEC_E_OK, SEC_I_COMPLETE_AND_CONTINUE, SEC_I_COMPLETE_NEEDED, SEC_I_CONTINUE_NEEDED,
    };
    use windows_sys::Win32::Security::Authentication::Identity::{
        AcquireCredentialsHandleW, CompleteAuthToken, DeleteSecurityContext, FreeContextBuffer,
        FreeCredentialsHandle, InitializeSecurityContextW, SecBuffer, SecBufferDesc,
        ISC_REQ_ALLOCATE_MEMORY, ISC_REQ_CONNECTION, ISC_REQ_MUTUAL_AUTH, SECBUFFER_TOKEN,
        SECBUFFER_VERSION, SECPKG_CRED_OUTBOUND, SECURITY_NATIVE_DREP,
    };
    use windows_sys::Win32::Security::Credentials::SecHandle;
    use windows_sys::Win32::System::Rpc::{
        SEC_WINNT_AUTH_IDENTITY_UNICODE, SEC_WINNT_AUTH_IDENTITY_W,
    };

    use std::sync::{Arc, Mutex};

    use super::{Account, Sspi, TdsError};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn failed(what: &str, status: i32) -> TdsError {
        TdsError::Sspi(format!(
            "Windows sign-in: {what} failed with {:#010x}",
            status as u32
        ))
    }

    /// A client context of the Negotiate package, for one connection.
    /// Each step runs off the runtime: Kerberos may ask a domain
    /// controller. The handles are freed when the last of the context
    /// and a step still running ends.
    pub struct Negotiate(Arc<Mutex<Handles>>);

    struct Handles {
        credentials: SecHandle,
        context: Option<SecHandle>,
        spn: Vec<u16>,
    }

    impl std::fmt::Debug for Negotiate {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Negotiate").finish_non_exhaustive()
        }
    }

    impl Negotiate {
        /// Credentials of `account` for the server named `spn`.
        pub fn new(account: &Account, spn: &str) -> Result<Self, TdsError> {
            let package = wide("Negotiate");
            let (mut user, mut domain, mut password) = match account {
                Account::Own => (Vec::new(), Vec::new(), Vec::new()),
                Account::Named {
                    user,
                    domain,
                    password,
                } => (wide(user), wide(domain), wide(password)),
            };
            let length = |w: &Vec<u16>| u32::try_from(w.len().saturating_sub(1)).unwrap_or(0);
            let identity = SEC_WINNT_AUTH_IDENTITY_W {
                User: user.as_mut_ptr(),
                UserLength: length(&user),
                Domain: domain.as_mut_ptr(),
                DomainLength: length(&domain),
                Password: password.as_mut_ptr(),
                PasswordLength: length(&password),
                Flags: SEC_WINNT_AUTH_IDENTITY_UNICODE,
            };
            let auth_data = match account {
                Account::Own => null(),
                Account::Named { .. } => (&raw const identity).cast(),
            };
            let mut credentials = SecHandle::default();
            let mut expiry = 0i64;
            // SAFETY: every pointer is to a live, NUL-terminated UTF-16
            // buffer or to `identity`, which outlives the call; SSPI copies
            // the credentials it keeps, and writes only `credentials` and
            // `expiry`.
            let status = unsafe {
                AcquireCredentialsHandleW(
                    null(),
                    package.as_ptr(),
                    SECPKG_CRED_OUTBOUND,
                    null(),
                    auth_data,
                    None,
                    null(),
                    &mut credentials,
                    &mut expiry,
                )
            };
            password.fill(0);
            if status != SEC_E_OK {
                return Err(failed("AcquireCredentialsHandle", status));
            }
            Ok(Self(Arc::new(Mutex::new(Handles {
                credentials,
                context: None,
                spn: wide(spn),
            }))))
        }
    }

    #[async_trait::async_trait]
    impl Sspi for Negotiate {
        async fn step(&mut self, challenge: Option<Vec<u8>>) -> Result<Vec<u8>, TdsError> {
            let handles = self.0.clone();
            tokio::task::spawn_blocking(move || {
                let mut handles = handles.lock().unwrap_or_else(|e| e.into_inner());
                handles.step(challenge.as_deref())
            })
            .await
            .map_err(|e| TdsError::Sspi(format!("Windows sign-in: {e}")))?
        }
    }

    impl Handles {
        fn step(&mut self, challenge: Option<&[u8]>) -> Result<Vec<u8>, TdsError> {
            let mut input_buffer = SecBuffer {
                cbBuffer: challenge.map_or(0, |c| u32::try_from(c.len()).unwrap_or(u32::MAX)),
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: challenge.map_or(null_mut(), |c| c.as_ptr().cast_mut().cast()),
            };
            let input = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 1,
                pBuffers: &mut input_buffer,
            };
            let mut output_buffer = SecBuffer {
                cbBuffer: 0,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: null_mut(),
            };
            let mut output = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 1,
                pBuffers: &mut output_buffer,
            };
            let mut context = self.context.unwrap_or_default();
            let mut attributes = 0u32;
            let mut expiry = 0i64;
            let current = self
                .context
                .as_ref()
                .map_or(null(), |c| c as *const SecHandle);
            // SAFETY: the handles are this context's own, the input buffer
            // borrows `challenge` for the call only (SSPI does not write
            // to it), and SSPI allocates the output token
            // (ISC_REQ_ALLOCATE_MEMORY), freed below.
            let status = unsafe {
                InitializeSecurityContextW(
                    &self.credentials,
                    current,
                    self.spn.as_ptr(),
                    ISC_REQ_ALLOCATE_MEMORY | ISC_REQ_CONNECTION | ISC_REQ_MUTUAL_AUTH,
                    0,
                    SECURITY_NATIVE_DREP,
                    if challenge.is_some() { &input } else { null() },
                    0,
                    &mut context,
                    &mut output,
                    &mut attributes,
                    &mut expiry,
                )
            };
            if self.context.is_none() && status >= 0 {
                self.context = Some(context);
            }
            let completed = if matches!(status, SEC_I_COMPLETE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE)
            {
                // SAFETY: the context exists (set above) and `output` is
                // the token SSPI just wrote, not yet freed.
                unsafe { CompleteAuthToken(&context, &output) }
            } else {
                SEC_E_OK
            };
            let token = if output_buffer.pvBuffer.is_null() {
                Vec::new()
            } else {
                // SAFETY: SSPI wrote `cbBuffer` bytes at `pvBuffer`, which
                // it allocated and which is freed once copied.
                unsafe {
                    let bytes = std::slice::from_raw_parts(
                        output_buffer.pvBuffer.cast::<u8>(),
                        output_buffer.cbBuffer as usize,
                    )
                    .to_vec();
                    FreeContextBuffer(output_buffer.pvBuffer);
                    bytes
                }
            };
            match status {
                SEC_E_OK | SEC_I_CONTINUE_NEEDED => Ok(token),
                SEC_I_COMPLETE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE if completed == SEC_E_OK => {
                    Ok(token)
                }
                SEC_I_COMPLETE_NEEDED | SEC_I_COMPLETE_AND_CONTINUE => {
                    Err(failed("CompleteAuthToken", completed))
                }
                other => Err(failed("InitializeSecurityContext", other)),
            }
        }
    }

    impl Drop for Handles {
        fn drop(&mut self) {
            // SAFETY: both handles are this value's own and freed once.
            unsafe {
                if let Some(context) = &self.context {
                    DeleteSecurityContext(context);
                }
                FreeCredentialsHandle(&self.credentials);
            }
        }
    }
}

/// Windows's own SSPI on both ends: the server side is
/// `AcceptSecurityContext`, as SQL Server's, in the test's process.
#[cfg(all(test, windows))]
mod tests;
