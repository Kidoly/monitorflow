//! The little of TDS 7.4 (MS-TDS, SQL Server's protocol) a check needs:
//! the pre-login and its encryption, a SQL Server login, batches of one
//! `SELECT` each, and the columns of the types the check's own queries
//! return, every message bounded.
//!
//! Written here rather than taken from a client crate, as Redis's is:
//! `tiberius`, the Rust client, builds its rustls backend on aws-lc-rs,
//! a second cryptography library next to the workspace's ring.
//!
//! Encryption: with TDS 7.x, the TLS handshake travels inside pre-login
//! packets, then TLS carries every packet after it. The check asks for
//! encryption of the whole session (`ENCRYPT_ON`) or none
//! (`ENCRYPT_NOT_SUP`), never the mode that encrypts the login only.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

/// Packet size asked for at login, and the most a packet sent holds.
const PACKET_SIZE: usize = 4096;
/// Longest message read: the check's answers are a few kilobytes.
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// Most rows kept from one answer.
const MAX_ROWS: usize = 5_000;
const HEADER: usize = 8;

/// TDS 7.4, SQL Server 2012 and later.
const TDS_74: u32 = 0x7400_0004;

mod packet {
    pub const SQL_BATCH: u8 = 0x01;
    pub const LOGIN7: u8 = 0x10;
    pub const PRELOGIN: u8 = 0x12;
}

mod encrypt {
    pub const ON: u8 = 0x01;
    pub const NOT_SUP: u8 = 0x02;
    pub const REQ: u8 = 0x03;
}

/// How the session is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// Plain text: the password then goes in clear.
    Off,
    /// Encrypted, the certificate not verified (SQL Server makes its own
    /// at start when it has none).
    Unverified,
    /// Encrypted, the certificate verified against `roots` and the name.
    Verified,
}

#[derive(Debug, thiserror::Error)]
pub enum TdsError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Tls(String),
    #[error("{0}")]
    Protocol(String),
    /// An error the server sent, with its number.
    #[error("{message} (error {number})")]
    Server { number: i32, message: String },
}

fn protocol(text: impl Into<String>) -> TdsError {
    TdsError::Protocol(text.into())
}

/// One value of a row.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
}

impl Value {
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Int(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Text(t) => t.trim().parse().ok(),
            Self::Null => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }
}

pub type Row = Vec<Value>;

/// What the pre-login answer says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prelogin {
    pub encryption: u8,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// A logged-in session.
pub struct Client {
    io: Box<dyn Io>,
    packet_id: u8,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

/// Where and as whom to log in.
pub struct Login<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub password: &'a str,
    pub encryption: Encryption,
    pub roots: Option<Arc<RootCertStore>>,
}

impl Client {
    /// Connects to `host:port` and logs in.
    pub async fn connect(port: u16, login: &Login<'_>) -> Result<Self, TdsError> {
        let tcp = TcpStream::connect((login.host, port)).await?;
        let _ = tcp.set_nodelay(true);
        Self::over(tcp, login).await
    }

    /// Logs in over `stream`: pre-login, TLS when asked, login.
    pub(crate) async fn over<S>(stream: S, login: &Login<'_>) -> Result<Self, TdsError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut wrapped = Wrapped::new(stream);
        let asked = if login.encryption == Encryption::Off {
            encrypt::NOT_SUP
        } else {
            encrypt::ON
        };
        let mut packet_id = 1;
        send(
            &mut wrapped,
            packet::PRELOGIN,
            &prelogin(asked),
            &mut packet_id,
        )
        .await?;
        let answer = parse_prelogin(&read_message(&mut wrapped).await?)?;
        let tls = match (asked, answer.encryption) {
            (encrypt::NOT_SUP, encrypt::NOT_SUP) => false,
            (encrypt::NOT_SUP, _) => {
                return Err(TdsError::Tls(
                    "the server requires encryption, which the check's settings turn off".into(),
                ))
            }
            (_, encrypt::ON | encrypt::REQ) => true,
            _ => {
                return Err(TdsError::Tls(
                    "the server does not offer encryption: it has no certificate, or encryption is turned off on it".into(),
                ))
            }
        };
        let mut io: Box<dyn Io> = if tls {
            wrapped.handshake = true;
            let config = tls_config(login.encryption, login.roots.clone());
            let name = ServerName::try_from(login.host.to_owned())
                .map_err(|e| TdsError::Tls(format!("{}: {e}", login.host)))?;
            let mut stream = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(name, wrapped)
                .await
                .map_err(|e| TdsError::Tls(format!("TLS handshake: {e}")))?;
            // From here, TLS records go on the connection as they are.
            stream.get_mut().0.handshake = false;
            Box::new(stream)
        } else {
            Box::new(wrapped)
        };
        send(
            &mut io,
            packet::LOGIN7,
            &login7(login.host, login.user, login.password),
            &mut packet_id,
        )
        .await?;
        let tokens = parse_tokens(&read_message(&mut io).await?)?;
        if !tokens.logged_in {
            return Err(protocol("the server did not accept the login"));
        }
        Ok(Self { io, packet_id })
    }

    /// Runs `sql`, one statement, and returns its rows.
    pub async fn query(&mut self, sql: &str) -> Result<Vec<Row>, TdsError> {
        send(
            &mut self.io,
            packet::SQL_BATCH,
            &batch(sql),
            &mut self.packet_id,
        )
        .await?;
        let answer = parse_tokens(&read_message(&mut self.io).await?)?;
        Ok(answer.rows)
    }

    /// Closes the connection, with TLS's closing alert when encrypted.
    pub async fn close(mut self) {
        let _ = self.io.shutdown().await;
    }
}

/// Sends `payload` as one message of `kind`, in packets of at most
/// [`PACKET_SIZE`].
async fn send<W: AsyncWrite + Unpin + ?Sized>(
    w: &mut W,
    kind: u8,
    payload: &[u8],
    packet_id: &mut u8,
) -> Result<(), TdsError> {
    let mut out = Vec::with_capacity(payload.len() + HEADER * (payload.len() / PACKET_SIZE + 1));
    let mut chunks = payload.chunks(PACKET_SIZE - HEADER).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        out.extend_from_slice(&header(kind, last, chunk.len(), *packet_id));
        out.extend_from_slice(chunk);
        *packet_id = packet_id.wrapping_add(1);
    }
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

fn header(kind: u8, last: bool, payload: usize, packet_id: u8) -> [u8; HEADER] {
    let length = u16::try_from(payload + HEADER)
        .unwrap_or(u16::MAX)
        .to_be_bytes();
    [
        kind,
        u8::from(last),
        length[0],
        length[1],
        0,
        0,
        packet_id,
        0,
    ]
}

/// Reads packets up to the last of a message, and returns its payload.
async fn read_message<R: AsyncRead + Unpin + ?Sized>(r: &mut R) -> Result<Vec<u8>, TdsError> {
    let mut message = Vec::new();
    loop {
        let mut head = [0u8; HEADER];
        r.read_exact(&mut head).await?;
        let length = usize::from(u16::from_be_bytes([head[2], head[3]]));
        if length < HEADER {
            return Err(protocol("a packet shorter than its header"));
        }
        if message.len() + length > MAX_MESSAGE {
            return Err(protocol("an answer larger than the check reads"));
        }
        let start = message.len();
        message.resize(start + length - HEADER, 0);
        r.read_exact(&mut message[start..]).await?;
        if head[1] & 0x01 != 0 {
            return Ok(message);
        }
    }
}

/// The pre-login request: version, encryption, the default instance,
/// no thread ID, no MARS.
fn prelogin(encryption: u8) -> Vec<u8> {
    let options: [(u8, Vec<u8>); 5] = [
        (0x00, vec![0, 0, 0, 0, 0, 0]),
        (0x01, vec![encryption]),
        (0x02, vec![0]),
        (0x03, vec![0, 0, 0, 0]),
        (0x04, vec![0]),
    ];
    let mut head = Vec::new();
    let mut data = Vec::new();
    let mut offset = options.len() * 5 + 1;
    for (token, value) in &options {
        head.push(*token);
        head.extend_from_slice(&u16::try_from(offset).unwrap_or(0).to_be_bytes());
        head.extend_from_slice(&u16::try_from(value.len()).unwrap_or(0).to_be_bytes());
        data.extend_from_slice(value);
        offset += value.len();
    }
    head.push(0xFF);
    head.extend(data);
    head
}

pub(crate) fn parse_prelogin(payload: &[u8]) -> Result<Prelogin, TdsError> {
    let mut i = 0;
    let mut encryption = None;
    while let Some(&token) = payload.get(i) {
        if token == 0xFF {
            break;
        }
        let field = payload
            .get(i + 1..i + 5)
            .ok_or_else(|| protocol("a cut pre-login answer"))?;
        let offset = usize::from(u16::from_be_bytes([field[0], field[1]]));
        let length = usize::from(u16::from_be_bytes([field[2], field[3]]));
        let value = payload
            .get(offset..offset + length)
            .ok_or_else(|| protocol("a pre-login option out of its answer"))?;
        if token == 0x01 {
            encryption = value.first().copied();
        }
        i += 5;
    }
    Ok(Prelogin {
        encryption: encryption.ok_or_else(|| {
            protocol("no encryption in the pre-login answer: is this SQL Server?")
        })?,
    })
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// UTF-16 little-endian text, a lone surrogate replaced.
fn from_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The password as LOGIN7 carries it: each byte's halves swapped, then
/// XORed with 0xA5. Not encryption; TLS is.
fn obfuscate(password: &str) -> Vec<u8> {
    utf16(password)
        .into_iter()
        .map(|b| b.rotate_left(4) ^ 0xA5)
        .collect()
}

/// A SQL Server login to `master`.
pub(crate) fn login7(server: &str, user: &str, password: &str) -> Vec<u8> {
    const FIXED: usize = 94;
    // Host, user, password, application, server, extension, library,
    // language, database: as UTF-16, in the order of the offsets.
    let fields: [(Vec<u8>, usize); 9] = {
        let text = |s: &str| (utf16(s), s.encode_utf16().count());
        let secret = (obfuscate(password), password.encode_utf16().count());
        [
            text("monitorflow"),
            text(user),
            secret,
            text("MonitorFlow"),
            text(server),
            (Vec::new(), 0),
            text("MonitorFlow"),
            text(""),
            text("master"),
        ]
    };
    let mut fixed = Vec::with_capacity(FIXED);
    let mut data = Vec::new();
    fixed.extend_from_slice(&[0; 4]); // Length, set below.
    fixed.extend_from_slice(&TDS_74.to_le_bytes());
    fixed.extend_from_slice(&u32::try_from(PACKET_SIZE).unwrap_or(4096).to_le_bytes());
    fixed.extend_from_slice(&[0; 4]); // Client program version.
    fixed.extend_from_slice(&std::process::id().to_le_bytes());
    fixed.extend_from_slice(&[0; 4]); // Connection ID.
                                      // USE_DB_ON, INIT_DB_FATAL, SET_LANG_ON; INIT_LANG_FATAL, ODBC_ON;
                                      // SQL type default; no option 3.
    fixed.extend_from_slice(&[0xE0, 0x03, 0x00, 0x00]);
    fixed.extend_from_slice(&[0; 4]); // Time zone.
    fixed.extend_from_slice(&0x0409u32.to_le_bytes()); // en-US.
    let mut offset = FIXED;
    let mut push = |fixed: &mut Vec<u8>, bytes: &[u8], count: usize| {
        fixed.extend_from_slice(&u16::try_from(offset).unwrap_or(0).to_le_bytes());
        fixed.extend_from_slice(&u16::try_from(count).unwrap_or(0).to_le_bytes());
        data.extend_from_slice(bytes);
        offset += bytes.len();
    };
    for (bytes, count) in &fields {
        push(&mut fixed, bytes, *count);
    }
    fixed.extend_from_slice(&[0; 6]); // Client ID.
    for _ in 0..3 {
        // SSPI, file to attach, new password: none.
        push(&mut fixed, &[], 0);
    }
    fixed.extend_from_slice(&[0; 4]); // Long SSPI length.
    debug_assert_eq!(fixed.len(), FIXED);
    let total = u32::try_from(fixed.len() + data.len()).unwrap_or(u32::MAX);
    fixed[..4].copy_from_slice(&total.to_le_bytes());
    fixed.extend(data);
    fixed
}

/// A batch: the transaction header TDS 7.2 and later require, then the
/// text.
fn batch(sql: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend(utf16(sql));
    out
}

/// What a token stream holds.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub logged_in: bool,
    pub rows: Vec<Row>,
}

/// How a column's values are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    /// Fixed-length integers, `bytes` long.
    Int(usize),
    /// Fixed-length floats.
    Float(usize),
    /// Nullable integers, floats and bits: a length byte, then the value.
    IntN,
    FloatN,
    /// DECIMAL and NUMERIC: a length byte, a sign, a little-endian integer.
    Decimal {
        scale: u8,
    },
    /// Unicode text: a two-byte length, then UTF-16.
    Unicode,
    /// Single-byte text, read as Latin-1.
    Ansi,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], TdsError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| protocol("a cut answer"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, TdsError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, TdsError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, TdsError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// UTF-16 text of `chars` characters.
    fn utf16(&mut self, chars: usize) -> Result<String, TdsError> {
        Ok(from_utf16(self.take(chars * 2)?))
    }

    fn b_varchar(&mut self) -> Result<String, TdsError> {
        let chars = usize::from(self.u8()?);
        self.utf16(chars)
    }

    fn us_varchar(&mut self) -> Result<String, TdsError> {
        let chars = usize::from(self.u16()?);
        self.utf16(chars)
    }

    fn skip_u16_length(&mut self) -> Result<(), TdsError> {
        let n = usize::from(self.u16()?);
        self.take(n).map(|_| ())
    }
}

fn column_type(r: &mut Reader<'_>) -> Result<Column, TdsError> {
    let ty = r.u8()?;
    Ok(match ty {
        0x30 | 0x32 => Column::Int(1),
        0x34 => Column::Int(2),
        0x38 => Column::Int(4),
        0x7F => Column::Int(8),
        0x3B => Column::Float(4),
        0x3E => Column::Float(8),
        // INTN and BITN, FLTN, with their length.
        0x26 | 0x68 => {
            r.u8()?;
            Column::IntN
        }
        0x6D => {
            r.u8()?;
            Column::FloatN
        }
        0x6A | 0x6C => {
            r.take(2)?; // Length and precision.
            Column::Decimal { scale: r.u8()? }
        }
        // NVARCHAR, NCHAR, VARCHAR, CHAR: a length and a collation.
        0xE7 | 0xEF | 0xA7 | 0xAF => {
            if r.u16()? == 0xFFFF {
                return Err(protocol(
                    "a column of unbounded text, which the check does not read",
                ));
            }
            r.take(5)?;
            if matches!(ty, 0xE7 | 0xEF) {
                Column::Unicode
            } else {
                Column::Ansi
            }
        }
        other => {
            return Err(protocol(format!(
                "a column of type {other:#04x}, which the check does not read"
            )))
        }
    })
}

fn int(bytes: &[u8]) -> i64 {
    match bytes.len() {
        1 => i64::from(bytes[0]),
        2 => i64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
        4 => i64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        _ => {
            let mut b = [0u8; 8];
            b[..bytes.len().min(8)].copy_from_slice(&bytes[..bytes.len().min(8)]);
            i64::from_le_bytes(b)
        }
    }
}

fn float(bytes: &[u8]) -> Result<f64, TdsError> {
    match bytes.len() {
        4 => Ok(f64::from(f32::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(bytes);
            Ok(f64::from_le_bytes(b))
        }
        _ => Err(protocol("a float of an unknown size")),
    }
}

fn value(r: &mut Reader<'_>, column: Column) -> Result<Value, TdsError> {
    Ok(match column {
        Column::Int(n) => Value::Int(int(r.take(n)?)),
        Column::Float(n) => Value::Float(float(r.take(n)?)?),
        Column::IntN | Column::FloatN | Column::Decimal { .. } => {
            let n = usize::from(r.u8()?);
            if n == 0 {
                return Ok(Value::Null);
            }
            let bytes = r.take(n)?;
            match column {
                Column::IntN if n <= 8 => Value::Int(int(bytes)),
                Column::FloatN => Value::Float(float(bytes)?),
                Column::Decimal { scale } => {
                    let mut magnitude = 0f64;
                    for b in bytes[1..].iter().rev() {
                        magnitude = magnitude * 256.0 + f64::from(*b);
                    }
                    let sign = if bytes[0] == 0 { -1.0 } else { 1.0 };
                    Value::Float(sign * magnitude / 10f64.powi(i32::from(scale)))
                }
                _ => return Err(protocol("an integer of an unknown size")),
            }
        }
        Column::Unicode | Column::Ansi => {
            let n = r.u16()?;
            if n == 0xFFFF {
                return Ok(Value::Null);
            }
            let bytes = r.take(usize::from(n))?;
            if column == Column::Unicode {
                Value::Text(from_utf16(bytes))
            } else {
                Value::Text(bytes.iter().map(|b| char::from(*b)).collect())
            }
        }
    })
}

/// Reads a token stream: rows of every result, the login's
/// acknowledgement, and the first error, which fails the whole answer.
pub(crate) fn parse_tokens(bytes: &[u8]) -> Result<Answer, TdsError> {
    let mut r = Reader { bytes, at: 0 };
    let mut answer = Answer::default();
    let mut columns: Vec<Column> = Vec::new();
    let mut error: Option<TdsError> = None;
    while r.at < bytes.len() {
        match r.u8()? {
            // COLMETADATA
            0x81 => {
                let count = r.u16()?;
                columns.clear();
                if count == 0xFFFF {
                    continue;
                }
                for _ in 0..count {
                    r.u32()?; // User type.
                    r.u16()?; // Flags.
                    columns.push(column_type(&mut r)?);
                    r.b_varchar()?; // Name.
                }
            }
            // ROW, NBCROW
            token @ (0xD1 | 0xD2) => {
                let nulls = if token == 0xD2 {
                    r.take(columns.len().div_ceil(8))?.to_vec()
                } else {
                    Vec::new()
                };
                let mut row = Vec::with_capacity(columns.len());
                for (i, column) in columns.iter().enumerate() {
                    let null = nulls.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0);
                    row.push(if null {
                        Value::Null
                    } else {
                        value(&mut r, *column)?
                    });
                }
                if answer.rows.len() < MAX_ROWS {
                    answer.rows.push(row);
                }
            }
            // ERROR, INFO
            token @ (0xAA | 0xAB) => {
                let length = usize::from(r.u16()?);
                let start = r.at;
                let number = r.u32()? as i32;
                r.take(2)?; // State and class.
                let message = r.us_varchar()?;
                r.at = start;
                r.take(length)?;
                if token == 0xAA && error.is_none() {
                    error = Some(TdsError::Server { number, message });
                }
            }
            // LOGINACK
            0xAD => {
                r.skip_u16_length()?;
                answer.logged_in = true;
            }
            // DONE, DONEPROC, DONEINPROC: status, command, row count.
            0xFD..=0xFF => {
                r.take(12)?;
            }
            // ENVCHANGE, ORDER, COLINFO, TABNAME, SSPI
            0xE3 | 0xA9 | 0xA5 | 0xA4 | 0xED => r.skip_u16_length()?,
            // RETURNSTATUS
            0x79 => {
                r.take(4)?;
            }
            // SESSIONSTATE, FEATUREEXTACK has none asked; FEDAUTHINFO
            0xE4 | 0xEE => {
                let n = r.u32()? as usize;
                r.take(n)?;
            }
            other => {
                return Err(protocol(format!(
                    "a token {other:#04x} the check does not read"
                )))
            }
        }
    }
    match error {
        Some(error) => Err(error),
        None => Ok(answer),
    }
}

/// The TLS settings: the workspace's ring, the server verified or not.
/// `.expect`: ring's provider supports TLS 1.2 and 1.3, the defaults.
fn tls_config(encryption: Encryption, roots: Option<Arc<RootCertStore>>) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions");
    match encryption {
        // Never unverified for want of roots: the machine's then.
        Encryption::Verified => builder
            .with_root_certificates(roots.unwrap_or_else(crate::roots))
            .with_no_client_auth(),
        Encryption::Unverified | Encryption::Off => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unverified(provider)))
            .with_no_client_auth(),
    }
}

/// Accepts any certificate, for `prefer`: what SQL Server's own clients
/// do with `TrustServerCertificate`. The handshake's signatures are not
/// checked either: with a certificate nobody vouches for, they prove
/// nothing an attacker could not sign, and the certificate SQL Server
/// makes itself at start is X.509 version 1, which WebPKI does not read.
#[derive(Debug)]
struct Unverified(Arc<CryptoProvider>);

impl ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The connection during the handshake: TLS records go out inside
/// pre-login packets, and come back inside them. Afterwards, bytes as
/// they are.
struct Wrapped<S> {
    inner: S,
    handshake: bool,
    /// Handshake bytes waiting for a flush.
    pending: Vec<u8>,
    /// Their packets being written, and how much of them is.
    framed: Vec<u8>,
    written: usize,
    /// The header of the packet being read, and what is left of its payload.
    header: [u8; HEADER],
    have: usize,
    left: usize,
}

impl<S> Wrapped<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            handshake: false,
            pending: Vec::new(),
            framed: Vec::new(),
            written: 0,
            header: [0; HEADER],
            have: 0,
            left: 0,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Wrapped<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.handshake {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        while this.left == 0 {
            while this.have < HEADER {
                let mut head = ReadBuf::new(&mut this.header[this.have..]);
                ready!(Pin::new(&mut this.inner).poll_read(cx, &mut head))?;
                if head.filled().is_empty() {
                    return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                }
                this.have += head.filled().len();
            }
            let length = usize::from(u16::from_be_bytes([this.header[2], this.header[3]]));
            this.left = length.checked_sub(HEADER).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a packet shorter than its header",
                )
            })?;
            this.have = 0;
        }
        let room = buf.remaining().min(this.left);
        let mut part = ReadBuf::new(buf.initialize_unfilled_to(room));
        ready!(Pin::new(&mut this.inner).poll_read(cx, &mut part))?;
        let n = part.filled().len();
        if n == 0 {
            return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
        }
        buf.advance(n);
        this.left -= n;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Wrapped<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if !this.handshake {
            return Pin::new(&mut this.inner).poll_write(cx, data);
        }
        // Packets on their way go first.
        ready!(this.poll_framed(cx))?;
        this.pending.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.handshake {
            if this.framed.is_empty() && !this.pending.is_empty() {
                let payload = std::mem::take(&mut this.pending);
                let mut chunks = payload.chunks(PACKET_SIZE - HEADER).peekable();
                while let Some(chunk) = chunks.next() {
                    let last = chunks.peek().is_none();
                    this.framed
                        .extend_from_slice(&header(packet::PRELOGIN, last, chunk.len(), 0));
                    this.framed.extend_from_slice(chunk);
                }
            }
            ready!(this.poll_framed(cx))?;
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<S: AsyncWrite + Unpin> Wrapped<S> {
    /// Writes the packets in `framed` to the end.
    fn poll_framed(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.framed.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.framed[self.written..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.written += n;
        }
        self.framed.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
pub(crate) mod tests;
