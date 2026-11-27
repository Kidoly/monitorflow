use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::*;

/// A token stream written the way SQL Server writes it.
#[derive(Default)]
struct Tokens(Vec<u8>);

impl Tokens {
    fn b_varchar(&mut self, text: &str) {
        self.0
            .push(u8::try_from(text.encode_utf16().count()).unwrap());
        self.0.extend(utf16(text));
    }

    /// COLMETADATA of `types`, each a type byte and its info.
    fn columns(mut self, types: &[&[u8]]) -> Self {
        self.0.push(0x81);
        self.0
            .extend_from_slice(&u16::try_from(types.len()).unwrap().to_le_bytes());
        for (i, ty) in types.iter().enumerate() {
            self.0.extend_from_slice(&[0; 4]);
            self.0.extend_from_slice(&[0x09, 0x00]);
            self.0.extend_from_slice(ty);
            self.b_varchar(&format!("c{i}"));
        }
        self
    }

    fn row(mut self, values: &[&[u8]]) -> Self {
        self.0.push(0xD1);
        for v in values {
            self.0.extend_from_slice(v);
        }
        self
    }

    fn raw(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }

    fn message(mut self, token: u8, number: i32, text: &str) -> Self {
        let mut body = Vec::new();
        body.extend_from_slice(&number.to_le_bytes());
        body.extend_from_slice(&[1, 14]);
        body.extend_from_slice(
            &u16::try_from(text.encode_utf16().count())
                .unwrap()
                .to_le_bytes(),
        );
        body.extend(utf16(text));
        body.extend_from_slice(&[0, 0]); // Server and procedure names.
        body.extend_from_slice(&1u32.to_le_bytes());
        self.0.push(token);
        self.0
            .extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
        self.0.extend(body);
        self
    }

    fn env_change(mut self) -> Self {
        // Database changed to master.
        let mut body = vec![1];
        body.push(6);
        body.extend(utf16("master"));
        body.push(0);
        self.0.push(0xE3);
        self.0
            .extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
        self.0.extend(body);
        self
    }

    fn login_ack(mut self) -> Self {
        let mut body = vec![1];
        body.extend_from_slice(&TDS_74.to_be_bytes());
        body.push(0);
        body.extend_from_slice(&[16, 0, 0x10, 0xE5]);
        self.0.push(0xAD);
        self.0
            .extend_from_slice(&u16::try_from(body.len()).unwrap().to_le_bytes());
        self.0.extend(body);
        self
    }

    fn done(mut self) -> Self {
        self.0.push(0xFD);
        self.0.extend_from_slice(&[0x10, 0, 0xC1, 0]);
        self.0.extend_from_slice(&1u64.to_le_bytes());
        self
    }
}

fn nvarchar(text: &str) -> Vec<u8> {
    let bytes = utf16(text);
    let mut out = u16::try_from(bytes.len()).unwrap().to_le_bytes().to_vec();
    out.extend(bytes);
    out
}

const NVARCHAR: &[u8] = &[0xE7, 0x00, 0x01, 0x09, 0x04, 0xD0, 0x00, 0x34];
const INTN8: &[u8] = &[0x26, 8];
const FLTN8: &[u8] = &[0x6D, 8];
const INT8: &[u8] = &[0x7F];
const DECIMAL: &[u8] = &[0x6A, 17, 38, 2];

#[test]
fn rows_of_every_type_the_check_asks_for() {
    let mut decimal = vec![5, 1];
    decimal.extend_from_slice(&12345u32.to_le_bytes());
    let stream = Tokens::default()
        .columns(&[NVARCHAR, INTN8, FLTN8, INT8, DECIMAL])
        .row(&[
            &nvarchar("Buffer Manager"),
            &[8, 42, 0, 0, 0, 0, 0, 0, 0],
            &[8, 0, 0, 0, 0, 0, 0, 0xF8, 0x3F],
            &7i64.to_le_bytes(),
            &decimal,
        ])
        .row(&[&[0xFF, 0xFF], &[0], &[0], &(-1i64).to_le_bytes(), &[0]])
        // NBCROW: the first, third and fifth are NULL.
        .raw(&[0xD2, 0b0001_0101])
        .raw(&[8, 1, 0, 0, 0, 0, 0, 0, 0])
        .raw(&3i64.to_le_bytes())
        .done();
    let answer = parse_tokens(&stream.0).unwrap();
    assert_eq!(
        answer.rows,
        vec![
            vec![
                Value::Text("Buffer Manager".into()),
                Value::Int(42),
                Value::Float(1.5),
                Value::Int(7),
                Value::Float(123.45),
            ],
            vec![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int(-1),
                Value::Null
            ],
            vec![
                Value::Null,
                Value::Int(1),
                Value::Null,
                Value::Int(3),
                Value::Null
            ],
        ]
    );
    assert_eq!(answer.rows[0][0].text(), Some("Buffer Manager"));
    assert_eq!(answer.rows[0][4].number(), Some(123.45));
    assert!(!answer.logged_in);
}

#[test]
fn an_error_fails_the_answer_and_an_info_does_not() {
    let info = Tokens::default()
        .message(0xAB, 5701, "Changed database context to 'master'.")
        .columns(&[INT8])
        .row(&[&1i64.to_le_bytes()])
        .done();
    assert_eq!(parse_tokens(&info.0).unwrap().rows.len(), 1);
    let error = Tokens::default()
        .message(0xAA, 300, "VIEW SERVER STATE permission was denied")
        .done();
    match parse_tokens(&error.0) {
        Err(TdsError::Server { number, message }) => {
            assert_eq!(number, 300);
            assert!(message.contains("VIEW SERVER STATE"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn answers_out_of_bounds_are_refused() {
    // A column of unbounded text, an unknown token, a cut row.
    let max = Tokens::default().columns(&[&[0xE7, 0xFF, 0xFF, 0, 0, 0, 0, 0]]);
    assert!(matches!(parse_tokens(&max.0), Err(TdsError::Protocol(_))));
    assert!(matches!(parse_tokens(&[0x42]), Err(TdsError::Protocol(_))));
    let cut = Tokens::default().columns(&[INT8]).raw(&[0xD1, 1, 2]);
    assert!(matches!(parse_tokens(&cut.0), Err(TdsError::Protocol(_))));
    assert!(parse_prelogin(&[0x01, 0, 50, 0, 1, 0xFF]).is_err());
    assert!(parse_prelogin(&[0xFF]).is_err());
}

#[test]
fn the_login_and_its_password() {
    let login = login7(&Login7 {
        version: TDS_74,
        server: "db.example.com",
        user: "monitorflow",
        password: "a",
        sspi: None,
    });
    let length = u32::from_le_bytes([login[0], login[1], login[2], login[3]]) as usize;
    assert_eq!(length, login.len());
    assert_eq!(&login[4..8], &TDS_74.to_le_bytes());
    // Offset and length of the password, the third field.
    let field = |n: usize| {
        let at = 36 + n * 4;
        let offset = usize::from(u16::from_le_bytes([login[at], login[at + 1]]));
        let count = usize::from(u16::from_le_bytes([login[at + 2], login[at + 3]]));
        &login[offset..offset + count * 2]
    };
    assert_eq!(field(1), utf16("monitorflow").as_slice());
    // "a" is 0x61 0x00 in UTF-16: halves swapped, then XOR 0xA5.
    assert_eq!(field(2), &[0xB3, 0xA5]);
    assert_eq!(field(4), utf16("db.example.com").as_slice());
    assert_eq!(field(8), utf16("master").as_slice());
    // No integrated security, no SSPI.
    assert_eq!(login[25] & 0x80, 0);
    assert_eq!(&login[80..82], &[0; 2]);
}

/// A Windows sign-in: no user nor password, integrated security on, the
/// first SSPI token in the login, and the instance after the host.
#[test]
fn a_windows_login_carries_its_token() {
    let token = [0x4E, 0x54, 0x4C, 0x4D, 0x53, 0x53, 0x50, 0];
    let login = login7(&Login7 {
        version: TDS_80,
        server: "db.example.com\\SALES",
        user: "",
        password: "",
        sspi: Some(&token),
    });
    assert_eq!(&login[4..8], &TDS_80.to_le_bytes());
    assert_eq!(login[25] & 0x80, 0x80);
    let word = |at: usize| usize::from(u16::from_le_bytes([login[at], login[at + 1]]));
    // User and password: empty.
    assert_eq!(word(42), 0);
    assert_eq!(word(46), 0);
    assert_eq!(
        &login[word(52)..word(52) + word(54) * 2],
        utf16("db.example.com\\SALES").as_slice()
    );
    // SSPI, its length in bytes, after the client ID.
    let (offset, length) = (word(78), word(80));
    assert_eq!(&login[offset..offset + length], &token);
    assert_eq!(&login[90..94], &[0; 4]);
    // A token too long for two bytes: its length in the long field.
    let long = vec![7u8; 70_000];
    let login = login7(&Login7 {
        version: TDS_74,
        server: "db",
        user: "",
        password: "",
        sspi: Some(&long),
    });
    assert_eq!(u16::from_le_bytes([login[80], login[81]]), 0xFFFF);
    assert_eq!(
        u32::from_le_bytes([login[90], login[91], login[92], login[93]]),
        70_000
    );
}

#[test]
fn the_server_s_sspi_token_is_kept() {
    let mut challenge = Tokens::default();
    challenge.0.push(0xED);
    challenge.0.extend_from_slice(&3u16.to_le_bytes());
    challenge.0.extend_from_slice(b"abc");
    let answer = parse_tokens(&challenge.0).unwrap();
    assert_eq!(answer.sspi.as_deref(), Some(&b"abc"[..]));
    assert!(!answer.logged_in);
}

#[tokio::test]
async fn messages_are_cut_into_packets() {
    let mut packets = Vec::new();
    let mut id = 1;
    send(&mut packets, packet::SQL_BATCH, &[7; 5000], &mut id)
        .await
        .unwrap();
    assert_eq!(id, 3);
    assert_eq!(packets.len(), 5000 + 2 * HEADER);
    assert_eq!(&packets[..2], &[packet::SQL_BATCH, 0]);
    assert_eq!(u16::from_be_bytes([packets[2], packets[3]]), 4096);
    let second = &packets[4096..];
    assert_eq!(&second[..2], &[packet::SQL_BATCH, 1]);
    assert_eq!(second[6], 2);
    // Nothing to send is still a packet.
    let mut packets = Vec::new();
    send(&mut packets, packet::SSPI, &[], &mut id)
        .await
        .unwrap();
    assert_eq!(packets, [packet::SSPI, 1, 0, 8, 0, 0, 3, 0]);
}

/// The server end of a session: reads one message, answers with
/// `answer`.
async fn exchange(server: &mut DuplexStream, kind: u8, answer: &[u8]) -> Vec<u8> {
    let message = read_message(server).await.unwrap();
    let mut id = 1;
    send(server, kind, answer, &mut id).await.unwrap();
    message
}

fn prelogin_answer(encryption: u8) -> Vec<u8> {
    let mut answer = prelogin(encryption);
    // The server's version: 16.0.4325.
    answer[26..30].copy_from_slice(&[16, 0, 0x10, 0xE5]);
    answer
}

fn plain() -> Login<'static> {
    Login {
        host: "127.0.0.1",
        instance: None,
        user: "monitorflow",
        password: "pw",
        encryption: Encryption::Off,
        roots: None,
    }
}

#[tokio::test]
async fn a_session_without_encryption() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let mut client = Client::over(client, &plain(), None).await?;
        let rows = client.query("SELECT 1").await?;
        client.close().await;
        Ok::<_, TdsError>(rows)
    });
    let request = exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    assert_eq!(
        parse_prelogin(&request).unwrap().encryption,
        encrypt::NOT_SUP
    );
    let login = Tokens::default().env_change().login_ack().done();
    let sent = exchange(&mut server, 0x04, &login.0).await;
    assert_eq!(&sent[4..8], &TDS_74.to_le_bytes());
    let rows = Tokens::default()
        .columns(&[INT8])
        .row(&[&1i64.to_le_bytes()])
        .done();
    let batch = exchange(&mut server, 0x04, &rows.0).await;
    assert_eq!(&batch[22..], utf16("SELECT 1").as_slice());
    assert_eq!(task.await.unwrap().unwrap(), vec![vec![Value::Int(1)]]);
}

#[tokio::test]
async fn a_refused_login_says_so() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move { Client::over(client, &plain(), None).await.map(|_| ()) });
    exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    let refused = Tokens::default()
        .message(0xAA, 18456, "Login failed for user 'monitorflow'.")
        .done();
    exchange(&mut server, 0x04, &refused.0).await;
    match task.await.unwrap() {
        Err(TdsError::Server { number: 18456, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn encryption_is_never_dropped_silently() {
    // Asked for and not offered: no login in clear.
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let login = Login {
        encryption: Encryption::Unverified,
        ..plain()
    };
    let task = tokio::spawn(async move { Client::over(client, &login, None).await.map(|_| ()) });
    let request = exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    assert_eq!(parse_prelogin(&request).unwrap().encryption, encrypt::ON);
    assert!(matches!(task.await.unwrap(), Err(TdsError::Tls(_))));
    // Required by the server and turned off by the check.
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move { Client::over(client, &plain(), None).await.map(|_| ()) });
    exchange(&mut server, 0x04, &prelogin_answer(encrypt::REQ)).await;
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("requires encryption"), "{error}");
    // Nothing more was sent: the server sees the end.
    let mut rest = Vec::new();
    server.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty());
}

/// The handshake inside pre-login packets: what TLS writes leaves as
/// one packet on flush, what comes back is unwrapped, and after the
/// handshake bytes pass as they are.
#[tokio::test]
async fn the_handshake_travels_in_prelogin_packets() {
    let (inner, mut far) = tokio::io::duplex(64 * 1024);
    let mut wrapped = Wrapped::new(inner);
    wrapped.handshake = true;
    wrapped.write_all(b"hello").await.unwrap();
    wrapped.write_all(b" tls").await.unwrap();
    wrapped.flush().await.unwrap();
    let mut packet = [0u8; HEADER + 9];
    far.read_exact(&mut packet).await.unwrap();
    assert_eq!(&packet[..4], &[packet::PRELOGIN, 1, 0, 17]);
    assert_eq!(&packet[HEADER..], b"hello tls");
    // Two packets back, read as one stream.
    far.write_all(&header(packet::PRELOGIN, false, 3, 1))
        .await
        .unwrap();
    far.write_all(b"abc").await.unwrap();
    far.write_all(&header(packet::PRELOGIN, true, 2, 2))
        .await
        .unwrap();
    far.write_all(b"de").await.unwrap();
    let mut back = [0u8; 5];
    wrapped.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"abcde");
    wrapped.handshake = false;
    wrapped.write_all(b"raw").await.unwrap();
    wrapped.flush().await.unwrap();
    let mut raw = [0u8; 3];
    far.read_exact(&mut raw).await.unwrap();
    assert_eq!(&raw, b"raw");
}

/// Tokens SSPI would give, and the server's it was given.
struct FakeSspi {
    out: Vec<Vec<u8>>,
    seen: Vec<Option<Vec<u8>>>,
}

#[async_trait::async_trait]
impl sspi::Sspi for FakeSspi {
    async fn step(&mut self, challenge: Option<Vec<u8>>) -> Result<Vec<u8>, TdsError> {
        self.seen.push(challenge);
        Ok(self.out.remove(0))
    }
}

fn sspi_token(bytes: &[u8]) -> Tokens {
    let mut tokens = Tokens::default();
    tokens.0.push(0xED);
    tokens
        .0
        .extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_le_bytes());
    tokens.0.extend_from_slice(bytes);
    tokens
}

/// NTLM's three messages: the first in the login, the server's
/// challenge back, the answer in an SSPI packet, then the login's
/// acknowledgement.
#[tokio::test]
async fn a_windows_sign_in_exchanges_tokens() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let mut fake = FakeSspi {
            out: vec![b"negotiate".to_vec(), b"authenticate".to_vec()],
            seen: Vec::new(),
        };
        let login = Login {
            user: "",
            password: "",
            instance: Some("SALES"),
            ..plain()
        };
        let client = Client::over(client, &login, Some(&mut fake)).await?;
        client.close().await;
        Ok::<_, TdsError>(fake.seen)
    });
    exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    let login = exchange(&mut server, 0x04, &sspi_token(b"challenge").0).await;
    assert_eq!(login[25] & 0x80, 0x80);
    let word = |at: usize| usize::from(u16::from_le_bytes([login[at], login[at + 1]]));
    assert_eq!(&login[word(78)..word(78) + word(80)], b"negotiate");
    assert_eq!(
        &login[word(52)..word(52) + word(54) * 2],
        utf16("127.0.0.1\\SALES").as_slice()
    );
    // The answer comes in a packet of type SSPI.
    let mut head = [0u8; HEADER];
    server.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], packet::SSPI);
    let mut body = vec![0u8; usize::from(u16::from_be_bytes([head[2], head[3]])) - HEADER];
    server.read_exact(&mut body).await.unwrap();
    assert_eq!(body, b"authenticate");
    let mut id = 1;
    let ack = Tokens::default().login_ack().done();
    send(&mut server, 0x04, &ack.0, &mut id).await.unwrap();
    let seen = task.await.unwrap().unwrap();
    assert_eq!(seen, vec![None, Some(b"challenge".to_vec())]);
}

/// A server that keeps asking is given up on, not answered forever.
#[tokio::test]
async fn an_endless_sign_in_ends() {
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let mut fake = FakeSspi {
            out: vec![Vec::new(); MAX_SSPI_ROUNDS + 1],
            seen: Vec::new(),
        };
        Client::over(client, &plain(), Some(&mut fake))
            .await
            .map(|_| ())
    });
    exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    for _ in 0..MAX_SSPI_ROUNDS {
        exchange(&mut server, 0x04, &sspi_token(b"again").0).await;
    }
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("did not end"), "{error}");
}

/// A server's TLS settings, its certificate issued by a CA of the test's
/// own for `localhost`, and the roots that trust it.
fn tls_server(alpn: &[&[u8]]) -> (tokio_rustls::TlsAcceptor, Arc<RootCertStore>) {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let mut ca = CertificateParams::default();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&key, &ca)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone()], key)
    .unwrap();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).unwrap();
    (
        tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        Arc::new(roots),
    )
}

fn strict(roots: Arc<RootCertStore>) -> Login<'static> {
    Login {
        host: "localhost",
        encryption: Encryption::Strict,
        roots: Some(roots),
        ..plain()
    }
}

/// TDS 8.0: TLS first with `tds/8.0`, then the pre-login and the login,
/// version 8.0, inside it.
#[tokio::test]
async fn a_strict_session_starts_with_tls() {
    let (acceptor, roots) = tls_server(&[ALPN_TDS_80]);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let mut client = Client::over(client, &strict(roots), None).await?;
        let rows = client.query("SELECT 1").await?;
        client.close().await;
        Ok::<_, TdsError>(rows)
    });
    let mut tls = acceptor.accept(server).await.unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(ALPN_TDS_80));
    let request = read_message(&mut tls).await.unwrap();
    assert_eq!(
        parse_prelogin(&request).unwrap().encryption,
        encrypt::STRICT
    );
    let mut id = 1;
    send(&mut tls, 0x04, &prelogin_answer(encrypt::REQ), &mut id)
        .await
        .unwrap();
    let login = read_message(&mut tls).await.unwrap();
    assert_eq!(&login[4..8], &TDS_80.to_le_bytes());
    let ack = Tokens::default().login_ack().done();
    send(&mut tls, 0x04, &ack.0, &mut id).await.unwrap();
    read_message(&mut tls).await.unwrap();
    let rows = Tokens::default()
        .columns(&[INT8])
        .row(&[&1i64.to_le_bytes()])
        .done();
    send(&mut tls, 0x04, &rows.0, &mut id).await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), vec![vec![Value::Int(1)]]);
}

/// A strict session never accepts a certificate its roots do not vouch
/// for, nor a server that does not speak TLS first.
#[tokio::test]
async fn a_strict_session_verifies_the_server() {
    let (acceptor, _) = tls_server(&[ALPN_TDS_80]);
    let (_, other_roots) = tls_server(&[]);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        Client::over(client, &strict(other_roots), None)
            .await
            .map(|_| ())
    });
    let _ = acceptor.accept(server).await;
    assert!(matches!(task.await.unwrap(), Err(TdsError::Tls(_))));

    // A TDS 7 server reads the client hello as a pre-login and fails.
    let (_, roots) = tls_server(&[]);
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task =
        tokio::spawn(async move { Client::over(client, &strict(roots), None).await.map(|_| ()) });
    let mut hello = [0u8; 5];
    server.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello[0], 0x16, "a TLS handshake record first");
    drop(server);
    assert!(matches!(task.await.unwrap(), Err(TdsError::Tls(_))));
}
