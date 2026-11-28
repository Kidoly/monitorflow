use std::ptr::{null, null_mut};

use tokio::io::AsyncReadExt;
use windows_sys::Win32::Foundation::{SEC_E_OK, SEC_I_CONTINUE_NEEDED};
use windows_sys::Win32::Security::Authentication::Identity::{
    AcceptSecurityContext, AcquireCredentialsHandleW, DeleteSecurityContext, FreeContextBuffer,
    FreeCredentialsHandle, SecBuffer, SecBufferDesc, ASC_REQ_ALLOCATE_MEMORY, ASC_REQ_CONNECTION,
    SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_CRED_INBOUND, SECURITY_NATIVE_DREP,
};
use windows_sys::Win32::Security::Credentials::SecHandle;

use super::*;
use crate::tds::{
    encrypt, packet, prelogin, read_message, send, Client, Encryption, Login, HEADER,
};

/// The server's side of Negotiate, as SQL Server runs it.
struct Server {
    credentials: SecHandle,
    context: Option<SecHandle>,
}

impl Server {
    fn new() -> Self {
        let package: Vec<u16> = "Negotiate\0".encode_utf16().collect();
        let mut credentials = SecHandle::default();
        let mut expiry = 0i64;
        // SAFETY: the package name is NUL-terminated and lives through the
        // call, which writes only `credentials` and `expiry`.
        let status = unsafe {
            AcquireCredentialsHandleW(
                null(),
                package.as_ptr(),
                SECPKG_CRED_INBOUND,
                null(),
                null(),
                None,
                null(),
                &mut credentials,
                &mut expiry,
            )
        };
        assert_eq!(status, SEC_E_OK, "{status:#x}");
        Self {
            credentials,
            context: None,
        }
    }

    /// The answer to the client's `token` and whether the sign-in is
    /// done, or the status Windows refused it with.
    fn accept(&mut self, token: &[u8]) -> Result<(Vec<u8>, bool), i32> {
        let mut input_buffer = SecBuffer {
            cbBuffer: u32::try_from(token.len()).unwrap(),
            BufferType: SECBUFFER_TOKEN,
            pvBuffer: token.as_ptr().cast_mut().cast(),
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
        // SAFETY: as in `Negotiate`'s step: the input borrows `token` for
        // the call, SSPI allocates the output, freed below.
        let status = unsafe {
            AcceptSecurityContext(
                &self.credentials,
                current,
                &input,
                ASC_REQ_ALLOCATE_MEMORY | ASC_REQ_CONNECTION,
                SECURITY_NATIVE_DREP,
                &mut context,
                &mut output,
                &mut attributes,
                &mut expiry,
            )
        };
        if status >= 0 {
            self.context.get_or_insert(context);
        }
        let answer = if output_buffer.pvBuffer.is_null() {
            Vec::new()
        } else {
            // SAFETY: SSPI wrote `cbBuffer` bytes there, freed once copied.
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
            SEC_E_OK => Ok((answer, true)),
            SEC_I_CONTINUE_NEEDED => Ok((answer, false)),
            other => Err(other),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: the handles are this value's own, freed once.
        unsafe {
            if let Some(context) = &self.context {
                DeleteSecurityContext(context);
            }
            FreeCredentialsHandle(&self.credentials);
        }
    }
}

/// The account running the test signs in to Windows's own server side:
/// NTLM on a runner outside a domain, as for an address in `host`.
#[tokio::test]
async fn the_agent_s_account_signs_in() {
    let mut client = Negotiate::new(&Account::Own, &spn("127.0.0.1", 1433)).unwrap();
    let mut server = Server::new();
    let mut token = client.step(None).await.unwrap();
    for _ in 0..4 {
        let (answer, done) = server.accept(&token).unwrap();
        if done {
            return;
        }
        token = client.step(Some(answer)).await.unwrap();
    }
    panic!("the sign-in did not end");
}

/// An account that does not exist is refused by Windows, as SQL Server
/// would see it.
#[tokio::test]
async fn an_unknown_account_is_refused() {
    let account = Account::named(".\\mf-nobody-here", "not-the-password");
    let mut client = Negotiate::new(&account, &spn("127.0.0.1", 1433)).unwrap();
    let mut server = Server::new();
    let mut token = client.step(None).await.unwrap();
    for _ in 0..4 {
        match server.accept(&token) {
            Err(_) => return,
            Ok((_, true)) => panic!("an unknown account was accepted"),
            Ok((answer, false)) => token = client.step(Some(answer)).await.unwrap(),
        }
    }
    panic!("the sign-in did not end");
}

/// The whole TDS login with a Windows account: LOGIN7 carries the first
/// token, SSPI packets the next ones, against Windows's server side.
#[tokio::test]
async fn a_tds_login_with_windows_s_sspi() {
    let (client, mut far) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let mut negotiate = Negotiate::new(&Account::Own, &spn("127.0.0.1", 1433))?;
        let login = Login {
            host: "127.0.0.1",
            instance: None,
            user: "",
            password: "",
            encryption: Encryption::Off,
            roots: None,
        };
        Client::over(client, &login, Some(&mut negotiate))
            .await
            .map(|_| ())
    });
    let _prelogin = read_message(&mut far).await.unwrap();
    let mut id = 1;
    let mut answer = prelogin(encrypt::NOT_SUP);
    answer[26..30].copy_from_slice(&[16, 0, 0x10, 0xE5]);
    send(&mut far, 0x04, &answer, &mut id).await.unwrap();
    let login = read_message(&mut far).await.unwrap();
    assert_eq!(login[25] & 0x80, 0x80);
    let word = |at: usize| usize::from(u16::from_le_bytes([login[at], login[at + 1]]));
    let mut token = login[word(78)..word(78) + word(80)].to_vec();
    let mut server = Server::new();
    for _ in 0..4 {
        let (challenge, done) = server.accept(&token).unwrap();
        let mut tokens = Vec::new();
        if !challenge.is_empty() {
            tokens.push(0xED);
            tokens.extend_from_slice(&u16::try_from(challenge.len()).unwrap().to_le_bytes());
            tokens.extend_from_slice(&challenge);
        }
        if done {
            // LOGINACK, then DONE.
            let mut ack = vec![1];
            ack.extend_from_slice(&0x7400_0004u32.to_be_bytes());
            ack.extend_from_slice(&[0, 16, 0, 0x10, 0xE5]);
            tokens.push(0xAD);
            tokens.extend_from_slice(&u16::try_from(ack.len()).unwrap().to_le_bytes());
            tokens.extend(ack);
            tokens.push(0xFD);
            tokens.extend_from_slice(&[0; 12]);
            send(&mut far, 0x04, &tokens, &mut id).await.unwrap();
            task.await.unwrap().unwrap();
            return;
        }
        send(&mut far, 0x04, &tokens, &mut id).await.unwrap();
        let mut head = [0u8; HEADER];
        far.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], packet::SSPI);
        token = vec![0u8; usize::from(u16::from_be_bytes([head[2], head[3]])) - HEADER];
        far.read_exact(&mut token).await.unwrap();
    }
    panic!("the sign-in did not end");
}
