//! An RDP client for tests against a real RDP service (ADR-0192):
//! IronRDP's connector and session, the crates its web client is built
//! from, connecting and waiting for the first picture of the screen.
//!
//! [`direct`] speaks to the service itself, X.224 and TLS included, as
//! the central server's proxy does. [`through_cleanpath`] speaks to the
//! central server's proxy, as the web client does: an RDCleanPath request
//! first, then RDP in the clear.

#![allow(dead_code)] // Each test file uses part of it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ironrdp_connector::{
    ClientConnector, Config, ConnectionResult, Credentials, DesktopSize, Sequence,
};
use ironrdp_core::WriteBuf;
use ironrdp_pdu::gcc::KeyboardType;
use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp_pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{FramedWrite, TokioFramed};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite};

/// How long the whole connection and the first picture may take.
pub const TIMEOUT: Duration = Duration::from_secs(90);

/// Who signs in, from `MF_RDP_USERNAME`, `MF_RDP_PASSWORD`, and the
/// optional `MF_RDP_DOMAIN`; `MF_RDP_NLA=0` turns network level
/// authentication off, for services without CredSSP, and
/// `MF_RDP_COMPRESSION` (`8k`, `64k`, `rdp6`, `rdp61`) asks for bulk
/// compression.
pub struct Account {
    pub username: String,
    pub password: String,
    pub domain: Option<String>,
    pub nla: bool,
    pub compression: Option<CompressionType>,
}

impl Account {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        Self {
            username: var("MF_RDP_USERNAME"),
            password: var("MF_RDP_PASSWORD"),
            domain: std::env::var("MF_RDP_DOMAIN")
                .ok()
                .filter(|d| !d.is_empty()),
            nla: var("MF_RDP_NLA") != "0",
            compression: match var("MF_RDP_COMPRESSION").as_str() {
                "8k" => Some(CompressionType::K8),
                "64k" => Some(CompressionType::K64),
                "rdp6" => Some(CompressionType::Rdp6),
                "rdp61" => Some(CompressionType::Rdp61),
                _ => None,
            },
        }
    }
}

/// The port of the service, `MF_RDP_PORT`, 3389 by default.
pub fn port() -> u16 {
    std::env::var("MF_RDP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3389)
}

/// What the client saw.
#[derive(Debug)]
pub struct Screen {
    pub width: u16,
    pub height: u16,
    /// Pixels of the first pictures that are not black: at least 1%.
    pub lit: usize,
}

fn config(account: &Account) -> Config {
    Config {
        desktop_size: DesktopSize {
            width: 1024,
            height: 768,
        },
        monitor_layout: None,
        desktop_scale_factor: 0,
        desktop_physical_size: None,
        desktop_orientation: None,
        device_scale_factor: None,
        monitor_extended_layout: None,
        relative_mouse_input: false,
        enable_tls: true,
        enable_credssp: account.nla,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword {
            username: account.username.clone(),
            password: account.password.clone(),
        },
        domain: account.domain.clone(),
        client_build: 0,
        client_name: "monitorflow-test".into(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0,
        connection_type: ironrdp_pdu::gcc::ConnectionType::Lan,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: ironrdp_pdu::rdp::capability_sets::RailSupportLevel::empty(),
        platform: MajorPlatformType::UNIX,
        hardware_id: None,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        enable_audio_capture: false,
        performance_flags: PerformanceFlags::default(),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: account.compression,
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
        support_dyn_vc_gfx_protocol: false,
    }
}

fn client_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 50000))
}

/// No KDC: NTLM needs none, and the tests use local accounts.
struct NoNetwork;

impl ironrdp_tokio::NetworkClient for NoNetwork {
    async fn send(
        &mut self,
        _request: &ironrdp_connector::sspi::generator::NetworkRequest,
    ) -> ironrdp_connector::ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::general_err!("no network in tests"))
    }
}

/// The subject public key of a DER certificate, which CredSSP binds to.
fn public_key(certificate: &[u8]) -> Result<Vec<u8>, String> {
    let (_, cert) = x509_parser::parse_x509_certificate(certificate)
        .map_err(|e| format!("the service's certificate does not parse: {e}"))?;
    Ok(cert.public_key().subject_public_key.data.to_vec())
}

/// Connects to the RDP service at the other end of `stream`, X.224 and TLS
/// included, and returns once the first picture of its screen is drawn.
pub async fn direct<S>(stream: S, account: &Account) -> Result<Screen, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    let mut framed = TokioFramed::new(stream);
    let mut connector = ClientConnector::new(config(account), client_addr());
    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .map_err(|e| format!("X.224: {e}"))?;
    let stream = framed.into_inner_no_leftover();
    let tls = tokio_rustls::TlsConnector::from(tls_config())
        .connect(
            ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into()),
            stream,
        )
        .await
        .map_err(|e| format!("TLS: {e}"))?;
    let certificate = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .ok_or("the service showed no certificate")?
        .to_vec();
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = TokioFramed::new(tls);
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        "127.0.0.1".into(),
        public_key(&certificate)?,
        None,
    )
    .await
    .map_err(|e| format!("the connection sequence: {e:?}"))?
    .into_connected()
    .map_err(|e| format!("the connection sequence: {e}"))?;
    first_picture(framed, result, account).await
}

/// Connects through the central server's RDCleanPath proxy: `request`
/// sends the RDCleanPath request (the X.224 connection request as its
/// argument) and answers the proxy's X.224 confirm and the service's
/// certificate; `stream` then carries RDP in the clear.
pub async fn through_cleanpath<S, F, Fut>(
    request: F,
    stream: S,
    account: &Account,
) -> Result<Screen, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(Vec<u8>, Vec<u8>), String>>,
{
    let mut connector = ClientConnector::new(config(account), client_addr());
    let mut buffer = WriteBuf::new();
    connector
        .step_no_input(&mut buffer)
        .map_err(|e| format!("X.224: {e}"))?;
    let (confirm, certificate) = request(buffer.filled().to_vec()).await?;
    buffer.clear();
    connector
        .step(&confirm, None, &mut buffer)
        .map_err(|e| format!("X.224 confirm: {e}"))?;
    let should_upgrade = ironrdp_tokio::skip_connect_begin(&mut connector);
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = TokioFramed::new(stream);
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        "127.0.0.1".into(),
        public_key(&certificate)?,
        None,
    )
    .await
    .map_err(|e| format!("the connection sequence: {e:?}"))?
    .into_connected()
    .map_err(|e| format!("the connection sequence: {e}"))?;
    first_picture(framed, result, account).await
}

/// Reads the session until pictures cover at least 1% of the screen with
/// something not black (a sign-in screen or a desktop), answering what
/// the service asks for.
async fn first_picture<S>(
    mut framed: TokioFramed<S>,
    result: ConnectionResult,
    account: &Account,
) -> Result<Screen, String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync,
{
    let size = result.desktop_size;
    let mut image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        size.width,
        size.height,
    );
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: account.compression,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();
    let mut pictures = 0;
    loop {
        let (action, frame) = framed
            .read_pdu()
            .await
            .map_err(|e| format!("reading the session: {e}"))?;
        let outputs = stage
            .process(&mut image, action, &frame, None)
            .map_err(|e| format!("the session: {e}"))?;
        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(bytes) if !bytes.is_empty() => framed
                    .write_all(&bytes)
                    .await
                    .map_err(|e| format!("answering the service: {e}"))?,
                ActiveStageOutput::GraphicsUpdate(_) => pictures += 1,
                ActiveStageOutput::Terminate(reason) => {
                    return Err(format!("the service ended the session: {reason:?}"))
                }
                _ => {}
            }
        }
        if pictures > 0 {
            let lit = image
                .data()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] != [0, 0, 0])
                .count();
            // More than the pointer: a part of a screen.
            if lit * 100 >= image.data().len() / 4 {
                return Ok(Screen {
                    width: image.width(),
                    height: image.height(),
                    lit,
                });
            }
        }
    }
}

/// TLS 1.2 or 1.3 with whatever certificate the service shows, as the
/// central server's proxy does (ADR-0190).
fn tls_config() -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    config.enable_sni = false;
    Arc::new(config)
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
