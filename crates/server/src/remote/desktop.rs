//! Remote desktops (ADR-0190): the central server as the RDCleanPath
//! proxy of IronRDP's web client, inside a remote session.
//!
//! The browser's WebSocket starts with an RDCleanPath request: the
//! session's one-time token and the client's X.224 connection request.
//! The session then opens as a terminal's would, except that the agent
//! connects to the host's own RDP service on `127.0.0.1:3389` instead of
//! starting a shell. Through the sealed frames, this module sends the
//! X.224 request, reads the service's answer, and does the TLS handshake
//! with it; it answers the browser with that answer and the service's
//! certificates, which the client binds its network level authentication
//! to. From then on the browser speaks RDP in the clear on its WebSocket
//! (itself HTTPS), and this module carries it in and out of that TLS.
//!
//! The service's certificate is not checked against any authority:
//! Windows makes its own, and the tunnel already proves the host, since
//! only the agent's certificate key can open the session's keys and the
//! agent reaches nothing but its own loopback.

use std::sync::Arc;
use std::time::Duration;

use ironrdp_rdcleanpath::{RDCleanPath, RDCleanPathPdu};
use monitorflow_common::remote::MAX_CHUNK;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::rdp::clipboard::{self, Allow, Clipboard, Event, Side};
use super::rdp::recorder::{self, Feed};
use super::rdp::{self, Connection, Framer, FromClient, FromService};
use super::{FromBrowser, RemoteHub, ToBrowser, COMMAND_TTL};

pub use ironrdp_rdcleanpath;

/// What the browser is told the service's address is.
pub const SERVICE_ADDRESS: &str = "127.0.0.1:3389";
/// The X.224 exchange and the TLS handshake, each.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest X.224 PDU either side may send: they are a few dozen bytes.
const MAX_X224: usize = 1024;
/// Bytes between the TLS stream and the session's channels.
const PIPE: usize = 64 * 1024;
/// PDUs waiting for the recorder.
const FEED: usize = 64;
/// Clipboard events waiting for the audit log.
const EVENTS: usize = 256;
/// Why a recorded desktop refuses a client asking for the graphics
/// pipeline: its screen would reach the recording as nothing it decodes.
const GRAPHICS_PIPELINE: &str =
    "this desktop is recorded, and the client asked for a graphics mode the recording cannot read";

/// Why a desktop's WebSocket was refused before its session ran.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DesktopError {
    #[error("the first message is not an RDCleanPath request")]
    BadRequest,
    #[error("{0}")]
    Refused(String),
}

/// The parts of the browser's RDCleanPath request this proxy uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanPathRequest {
    /// The session's one-time token, in `proxy_auth`.
    pub token: String,
    /// The client's X.224 connection request, a whole TPKT.
    pub x224: Vec<u8>,
}

/// Parses the first WebSocket message. The destination is ignored: the
/// agent only ever reaches its own RDP service.
pub fn parse_request(bytes: &[u8]) -> Result<CleanPathRequest, DesktopError> {
    let pdu = RDCleanPathPdu::from_der(bytes).map_err(|_| DesktopError::BadRequest)?;
    match pdu.into_enum().map_err(|_| DesktopError::BadRequest)? {
        RDCleanPath::Request {
            proxy_auth,
            preconnection_blob: None,
            x224_connection_request,
            ..
        } => {
            let x224 = x224_connection_request.as_bytes().to_vec();
            if tpkt_length(&x224) != Some(x224.len()) {
                return Err(DesktopError::BadRequest);
            }
            Ok(CleanPathRequest {
                token: proxy_auth,
                x224,
            })
        }
        _ => Err(DesktopError::BadRequest),
    }
}

/// The length a TPKT header announces, when it is one of at most
/// [`MAX_X224`] bytes.
fn tpkt_length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 || bytes[0] != 3 {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    (7..=MAX_X224).contains(&length).then_some(length)
}

/// An X.224 connection confirm carrying RDP_NEG_FAILURE: the service
/// refused the security protocols the client asked for.
fn negotiation_failed(confirm: &[u8]) -> bool {
    // TPKT (4), X.224 header (7), then the negotiation's type.
    confirm.len() >= 19 && confirm[11] == 0x03
}

/// An RDCleanPath error for the browser, as DER.
fn error_pdu(status: u16) -> Vec<u8> {
    RDCleanPathPdu::new_http_error(status)
        .to_der()
        .unwrap_or_default()
}

/// Runs the desktop session of the WebSocket whose first message was
/// `first`, then carries the browser's messages `from_browser` and
/// answers on `to_browser` until it ends. Returns why it ended, or why it
/// never ran.
pub async fn run(
    hub: RemoteHub,
    session: Uuid,
    first: &[u8],
    mut from_browser: mpsc::Receiver<Vec<u8>>,
    to_browser: mpsc::Sender<Vec<u8>>,
) -> Result<String, DesktopError> {
    let request = match parse_request(first) {
        Ok(request) => request,
        Err(error) => {
            let _ = to_browser.send(error_pdu(400)).await;
            return Err(error);
        }
    };
    let attached = match hub.attach_desktop(session, &request.token).await {
        Ok(attached) => attached,
        Err(error) => {
            let _ = to_browser.send(error_pdu(401)).await;
            return Err(DesktopError::Refused(error.to_string()));
        }
    };
    let (session_tx, session_rx) = mpsc::channel(64);
    let (output_tx, output_rx) = mpsc::channel(64);
    let runner = hub.clone();
    let session_of = attached.clone();
    let task = tokio::spawn(async move { runner.run(session_of, session_rx, output_tx).await });
    let (near, far) = tokio::io::duplex(PIPE);
    let (opened_tx, opened_rx) = oneshot::channel();
    tokio::spawn(pump(far, session_tx.clone(), output_rx, opened_tx));

    let opened = tokio::time::timeout(COMMAND_TTL + Duration::from_secs(15), opened_rx).await;
    if !matches!(opened, Ok(Ok(()))) {
        let _ = to_browser.send(error_pdu(502)).await;
        drop(near);
        return Ok(task.await.unwrap_or_else(|_| "the session stopped".into()));
    }
    let opened_at = tokio::time::Instant::now();
    let abort = |reason: String| {
        let session_tx = session_tx.clone();
        async move {
            let _ = session_tx.send(FromBrowser::Abort(reason)).await;
        }
    };
    let tls = match handshake(near, &request.x224).await {
        Ok((tls, confirm)) => {
            let protocol = rdp::selected_protocol(&confirm);
            let chain: Vec<Vec<u8>> = tls
                .get_ref()
                .1
                .peer_certificates()
                .unwrap_or_default()
                .iter()
                .map(|c| c.to_vec())
                .collect();
            match RDCleanPathPdu::new_response(SERVICE_ADDRESS.into(), confirm, chain)
                .and_then(|pdu| pdu.to_der())
            {
                Ok(response) => {
                    let _ = to_browser.send(response).await;
                    Some((tls, protocol))
                }
                Err(_) => {
                    let _ = to_browser.send(error_pdu(502)).await;
                    abort("cannot answer the browser".into()).await;
                    None
                }
            }
        }
        Err(Handshake::Refused(confirm)) => {
            let pdu = RDCleanPathPdu::new_negotiation_error(confirm)
                .and_then(|pdu| pdu.to_der())
                .unwrap_or_else(|_| error_pdu(502));
            let _ = to_browser.send(pdu).await;
            abort("the remote desktop service refused the security the client asked for".into())
                .await;
            None
        }
        Err(Handshake::Failed(reason)) => {
            let _ = to_browser.send(error_pdu(502)).await;
            abort(reason).await;
            None
        }
    };
    if let Some((tls, protocol)) = tls {
        let (feed, recording) = if attached.recorded {
            let (feed, rx) = mpsc::channel(FEED);
            let store = recorder::Store {
                pool: hub.pool().clone(),
                session,
            };
            (
                Some(feed),
                Some(tokio::spawn(recorder::record(store, rx, opened_at))),
            )
        } else {
            (None, None)
        };
        let mut recording = recording;
        // What passes on the clipboard, audited as it passes.
        let (events, mut audited) = mpsc::channel::<Event>(EVENTS);
        let pool = hub.pool().clone();
        let audited_session = attached.clone();
        let auditor = tokio::spawn(async move {
            while let Some(event) = audited.recv().await {
                audit(&pool, &audited_session, event).await;
            }
        });
        let allow = Allow {
            clipboard: attached.clipboard,
            files: attached.file_transfer,
        };
        let proxying = proxy(
            tls,
            protocol,
            attached.recorded,
            allow,
            &mut from_browser,
            &to_browser,
            feed,
            events,
        );
        // The recorder failing ends the session: a recorded desktop must be
        // recorded.
        let failed = async {
            match recording.as_mut() {
                Some(task) => match task.await {
                    Ok(Err(error)) => Some(error.to_string()),
                    Ok(Ok(())) => None,
                    Err(_) => Some("the recording stopped".to_owned()),
                },
                None => std::future::pending().await,
            }
        };
        let stopped = tokio::select! {
            reason = proxying => reason,
            Some(reason) = failed => {
                recording = None;
                Some(reason)
            }
        };
        if let Some(reason) = stopped {
            abort(reason).await;
        }
        // The last picture, once the proxy no longer feeds it.
        if let Some(task) = recording {
            if let Ok(Err(error)) = task.await {
                tracing::warn!(%session, %error, "a desktop recording ended early");
            }
        }
        let _ = auditor.await;
    }
    drop(session_tx);
    Ok(task.await.unwrap_or_else(|_| "the session stopped".into()))
}

/// Writes what passed on a desktop's clipboard to the audit log.
async fn audit(pool: &sqlx::PgPool, attached: &super::Attached, event: Event) {
    let (action, details) = match event {
        Event::Copy { from, kind, bytes } => (
            "remote_session.clipboard",
            serde_json::json!({"direction": from.direction(), "format": kind, "bytes": bytes}),
        ),
        Event::File { from, name, size } => (
            "remote_session.file",
            serde_json::json!({"direction": from.direction(), "name": name, "size": size}),
        ),
    };
    let result = async {
        let mut connection = pool.acquire().await?;
        super::session::audit(&mut connection, attached, action, details).await
    }
    .await;
    if let Err(error) = result {
        tracing::error!(session = %attached.id, %error, "cannot audit a desktop's clipboard");
    }
}

/// What a static virtual channel PDU of `name` from `side` becomes: the
/// frames to pass on (none, itself, or a whole clipboard message held
/// until its last chunk).
fn channel(
    clipboard: &std::sync::Mutex<Clipboard>,
    allow: Allow,
    side: Side,
    name: &str,
    pdu: Vec<u8>,
    events: &mut Vec<Event>,
) -> Vec<Vec<u8>> {
    match name {
        clipboard::CHANNEL => {
            let mut clipboard = clipboard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (frames, said) = clipboard.pdu(side, pdu);
            events.extend(said);
            frames
        }
        // Device redirection, printers included, carries files: never
        // without file transfers.
        "rdpdr" if !allow.files => Vec::new(),
        _ => vec![pdu],
    }
}

/// Carries RDP between the browser and the service's TLS, whole PDUs
/// only, reading them as they pass, letting through the clipboard and
/// files as `allow` says, sending what passed to `audited`, and feeding
/// the recorder `feed` with the service's screen. Returns why it had to
/// stop, if it did; when either side closes, `None`.
#[allow(clippy::too_many_arguments)]
async fn proxy<S>(
    tls: tokio_rustls::client::TlsStream<S>,
    protocol: u32,
    recorded: bool,
    allow: Allow,
    from_browser: &mut mpsc::Receiver<Vec<u8>>,
    to_browser: &mpsc::Sender<Vec<u8>>,
    feed: Option<mpsc::Sender<Feed>>,
    audited: mpsc::Sender<Event>,
) -> Option<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connection = std::sync::Mutex::new(Connection::default());
    let read = || {
        connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let clipboard = std::sync::Mutex::new(Clipboard::new(allow));
    let audit = |events: Vec<Event>| {
        let audited = audited.clone();
        async move {
            for event in events {
                let _ = audited.send(event).await;
            }
        }
    };
    let (mut reader, mut writer) = tokio::io::split(tls);
    let up = async {
        let mut framer = Framer::client();
        while let Some(bytes) = from_browser.recv().await {
            let pdus = match framer.push(&bytes) {
                Ok(pdus) => pdus,
                Err(error) => return Some(error.to_string()),
            };
            let mut out = Vec::with_capacity(bytes.len());
            let mut feeds = Vec::new();
            let mut events = Vec::new();
            for pdu in pdus {
                let what = read().from_client(&pdu);
                match what {
                    FromClient::Pointer { x, y } => feeds.push(Feed::Pointer { x, y }),
                    FromClient::Compression(c) => feeds.push(Feed::Compression(c)),
                    FromClient::Channel { name } => {
                        let frames = channel(
                            &clipboard,
                            allow,
                            Side::Client,
                            &name,
                            pdu.bytes,
                            &mut events,
                        );
                        frames.iter().for_each(|f| out.extend_from_slice(f));
                        continue;
                    }
                    FromClient::Other => {}
                }
                if recorded && read().wants_graphics_pipeline {
                    return Some(GRAPHICS_PIPELINE.to_owned());
                }
                out.extend_from_slice(&pdu.bytes);
            }
            audit(events).await;
            if let Some(feed) = &feed {
                for next in feeds {
                    // Gone: the recorder failed, and says why.
                    if feed.send(next).await.is_err() {
                        return None;
                    }
                }
            }
            if !out.is_empty() && writer.write_all(&out).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
        None
    };
    let down = async {
        let mut framer = Framer::service(protocol);
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let n = match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => n,
            };
            let pdus = match framer.push(&buffer[..n]) {
                Ok(pdus) => pdus,
                Err(error) => return Some(error.to_string()),
            };
            let mut out = Vec::with_capacity(n);
            let mut events = Vec::new();
            for pdu in pdus {
                let what = read().from_service(&pdu);
                if let FromService::Channel { name } = &what {
                    let frames = channel(
                        &clipboard,
                        allow,
                        Side::Service,
                        name,
                        pdu.bytes,
                        &mut events,
                    );
                    frames.iter().for_each(|f| out.extend_from_slice(f));
                    continue;
                }
                if let Some(feed) = &feed {
                    let next = match what {
                        FromService::Screen => Some(Feed::Screen(pdu.bytes.clone())),
                        FromService::Io => Some(Feed::Io(pdu.bytes.clone())),
                        FromService::Channel { .. } | FromService::Other => None,
                    };
                    // A slow recorder slows the service, as a slow browser
                    // does.
                    if let Some(next) = next {
                        if feed.send(next).await.is_err() {
                            return None;
                        }
                    }
                }
                out.extend_from_slice(&pdu.bytes);
            }
            audit(events).await;
            if !out.is_empty() && to_browser.send(out).await.is_err() {
                return None;
            }
        }
    };
    // Either side ending ends the session; its task says why.
    tokio::select! {
        reason = up => reason,
        reason = down => reason,
    }
}

/// Between the TLS stream's end and the session's channels.
async fn pump(
    far: DuplexStream,
    session: mpsc::Sender<FromBrowser>,
    mut output: mpsc::Receiver<ToBrowser>,
    opened: oneshot::Sender<()>,
) {
    let (mut read, mut write) = tokio::io::split(far);
    let up = async {
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            match read.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if session
                        .send(FromBrowser::Input(buffer[..n].to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        let _ = session.send(FromBrowser::Close).await;
    };
    let down = async {
        let mut opened = Some(opened);
        while let Some(message) = output.recv().await {
            match message {
                ToBrowser::Waiting => {}
                ToBrowser::Opened => {
                    if let Some(opened) = opened.take() {
                        let _ = opened.send(());
                    }
                }
                ToBrowser::Output(bytes) => {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                ToBrowser::Closed(_) => break,
            }
        }
        let _ = write.shutdown().await;
    };
    tokio::select! {
        () = up => {}
        () = down => {}
    }
}

enum Handshake {
    /// The service's X.224 confirm says no to the client's protocols.
    Refused(Vec<u8>),
    Failed(String),
}

/// The X.224 exchange then TLS with the service, on `stream`; returns the
/// TLS stream and the service's X.224 confirm.
async fn handshake<S>(
    mut stream: S,
    x224: &[u8],
) -> Result<(tokio_rustls::client::TlsStream<S>, Vec<u8>), Handshake>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let confirm = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        stream.write_all(x224).await?;
        read_tpkt(&mut stream).await
    })
    .await
    .map_err(|_| Handshake::Failed("the remote desktop service did not answer".into()))?
    .map_err(|e| {
        Handshake::Failed(format!(
            "the remote desktop service did not answer as RDP does: {e}"
        ))
    })?;
    if negotiation_failed(&confirm) {
        return Err(Handshake::Refused(confirm));
    }
    let connector = tokio_rustls::TlsConnector::from(client_config());
    let name = ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
    let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name, stream))
        .await
        .map_err(|_| Handshake::Failed("TLS with the remote desktop service timed out".into()))?
        .map_err(|e| {
            Handshake::Failed(format!("TLS with the remote desktop service failed: {e}"))
        })?;
    Ok((tls, confirm))
}

/// One TPKT, header included.
async fn read_tpkt<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<Vec<u8>> {
    let mut packet = vec![0u8; 4];
    stream.read_exact(&mut packet).await?;
    let length = tpkt_length(&packet).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "not an X.224 answer")
    })?;
    packet.resize(length, 0);
    stream.read_exact(&mut packet[4..]).await?;
    Ok(packet)
}

/// TLS 1.2 or 1.3 with any certificate the service shows, its handshake
/// signatures still checked (see the module's comment).
fn client_config() -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = AnyServiceCertificate {
        provider: provider.clone(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports TLS 1.2 and 1.3")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.enable_sni = false;
    Arc::new(config)
}

#[derive(Debug)]
struct AnyServiceCertificate {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AnyServiceCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An X.224 connection request asking for TLS and CredSSP, as
    /// IronRDP sends it.
    const X224_REQUEST: [u8; 19] = [3, 0, 0, 19, 14, 0xe0, 0, 0, 0, 0, 0, 1, 0, 8, 0, 3, 0, 0, 0];

    fn request(token: &str, x224: &[u8], pcb: Option<String>) -> Vec<u8> {
        RDCleanPathPdu::new_request(x224.to_vec(), "web-1".into(), token.into(), pcb)
            .unwrap()
            .to_der()
            .unwrap()
    }

    #[test]
    fn the_request_gives_the_token_and_a_whole_x224_pdu() {
        let parsed = parse_request(&request("abc", &X224_REQUEST, None)).unwrap();
        assert_eq!(parsed.token, "abc");
        assert_eq!(parsed.x224, X224_REQUEST);
        assert_eq!(
            parse_request(b"{\"token\":\"abc\"}"),
            Err(DesktopError::BadRequest)
        );
        // A preconnection blob would reach the service unread: refused.
        let pcb = request("abc", &X224_REQUEST, Some("x".into()));
        assert_eq!(parse_request(&pcb), Err(DesktopError::BadRequest));
        // A TPKT whose length is not its own.
        let mut short = X224_REQUEST.to_vec();
        short.pop();
        assert_eq!(
            parse_request(&request("abc", &short, None)),
            Err(DesktopError::BadRequest)
        );
        let response = RDCleanPathPdu::new_response("x".into(), X224_REQUEST.to_vec(), [])
            .unwrap()
            .to_der()
            .unwrap();
        assert_eq!(parse_request(&response), Err(DesktopError::BadRequest));
    }

    #[test]
    fn a_negotiation_failure_is_told_apart() {
        let mut confirm = X224_REQUEST;
        confirm[5] = 0xd0;
        confirm[11] = 0x02;
        assert!(!negotiation_failed(&confirm));
        confirm[11] = 0x03;
        assert!(negotiation_failed(&confirm));
        assert!(!negotiation_failed(&confirm[..11]));
    }

    #[test]
    fn tpkt_lengths_are_bounded() {
        assert_eq!(tpkt_length(&X224_REQUEST), Some(19));
        assert_eq!(tpkt_length(&[3, 0, 0x10, 0]), None);
        assert_eq!(tpkt_length(&[2, 0, 0, 19]), None);
        assert_eq!(tpkt_length(&[3, 0]), None);
    }

    #[test]
    fn errors_reach_the_browser_as_rdcleanpath() {
        let pdu = RDCleanPathPdu::from_der(&error_pdu(401)).unwrap();
        assert_eq!(pdu.error.unwrap().http_status_code, Some(401));
    }
}
