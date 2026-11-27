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
    let login = login7("db.example.com", "monitorflow", "a");
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
        let mut client = Client::over(client, &plain()).await?;
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
    let task = tokio::spawn(async move { Client::over(client, &plain()).await.map(|_| ()) });
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
    let task = tokio::spawn(async move { Client::over(client, &login).await.map(|_| ()) });
    let request = exchange(&mut server, 0x04, &prelogin_answer(encrypt::NOT_SUP)).await;
    assert_eq!(parse_prelogin(&request).unwrap().encryption, encrypt::ON);
    assert!(matches!(task.await.unwrap(), Err(TdsError::Tls(_))));
    // Required by the server and turned off by the check.
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move { Client::over(client, &plain()).await.map(|_| ()) });
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
