//! Remote sessions (ADR-0178): the keys the central server and an agent
//! agree on, and the sealing of the frames between them, so the relays on
//! the way pass frames on without reading or changing them.
//!
//! 1. The server makes an X25519 key for the session and puts its public
//!    half in the signed `remote_session` command: the agent knows it is
//!    the server's.
//! 2. The agent makes its own, and answers with its public half, its
//!    certificate and a signature over both halves with its certificate's
//!    key: the server knows it is this agent's, not a relay's.
//! 3. Both derive one key per direction with HKDF-SHA256, and seal every
//!    frame with ChaCha20-Poly1305, numbered from 0 in each direction. A
//!    frame with another number, or that does not open, ends the session.

use prost::Message;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::hkdf;
use ring::rand::SystemRandom;
use ring::signature::{self, ECDSA_P256_SHA256_ASN1};
use rustls::pki_types::CertificateDer;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::proto::v1::{remote_data, RemoteData, RemoteOpened};
use crate::tls::{PeerIdentity, PeerRole};

/// `RemoteSessionOpen.kind` of a terminal.
pub const TERMINAL: &str = "terminal";
/// Most bytes of input or output in one frame, so a frame stays far below
/// what a PostgreSQL notification carries between server instances.
pub const MAX_CHUNK: usize = 4096;
/// Bounds of `RemoteSessionOpen.idle_timeout_seconds` an agent accepts.
pub const MIN_IDLE_SECONDS: u32 = 60;
pub const MAX_IDLE_SECONDS: u32 = 24 * 60 * 60;
/// Largest terminal either side accepts, in columns and rows.
pub const MAX_COLS: u32 = 1000;
pub const MAX_ROWS: u32 = 500;

const KEY_LEN: usize = 32;
/// Separates these keys and signatures from any other use of the same
/// keys.
const DOMAIN: &[u8] = b"monitorflow remote session v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    #[error("the key is not a 32-byte X25519 public key")]
    BadKey,
    #[error("the certificate does not name this agent")]
    WrongAgent,
    #[error("the certificate is not trusted: {0}")]
    Untrusted(String),
    #[error("the signature of the session keys does not verify")]
    BadSignature,
    #[error("a frame is out of order")]
    OutOfOrder,
    #[error("a frame does not open with the session's key")]
    Unsealed,
    #[error("cannot make a key")]
    Random,
}

/// The server's key for one session, until the agent answers.
pub struct ServerHandshake {
    private: EphemeralPrivateKey,
    public: [u8; KEY_LEN],
}

impl ServerHandshake {
    pub fn new() -> Result<Self, RemoteError> {
        let (private, public) = ephemeral()?;
        Ok(Self { private, public })
    }

    /// `RemoteSessionOpen.server_key`.
    pub fn public_key(&self) -> &[u8; KEY_LEN] {
        &self.public
    }

    /// The server's channel once `opened` proved to come from agent
    /// `agent_id`: its certificate names the agent and `trusted` accepts
    /// it, and its key signed both halves of the keys.
    pub fn finish(
        self,
        session_id: &str,
        agent_id: &str,
        opened: &RemoteOpened,
        trusted: impl FnOnce(&CertificateDer<'_>) -> Result<(), String>,
    ) -> Result<Channel, RemoteError> {
        let certificate = CertificateDer::from(opened.certificate.as_slice());
        match PeerIdentity::from_cert(&certificate) {
            Ok(id) if id.role() == PeerRole::Agent && id.id() == agent_id => {}
            _ => return Err(RemoteError::WrongAgent),
        }
        trusted(&certificate).map_err(RemoteError::Untrusted)?;
        let (_, cert) =
            X509Certificate::from_der(&opened.certificate).map_err(|_| RemoteError::WrongAgent)?;
        let message = opened_signed_bytes(session_id, &self.public, &opened.agent_key);
        signature::UnparsedPublicKey::new(
            &ECDSA_P256_SHA256_ASN1,
            &cert.public_key().subject_public_key.data,
        )
        .verify(&message, &opened.signature)
        .map_err(|_| RemoteError::BadSignature)?;
        let keys = agree(
            self.private,
            &opened.agent_key,
            session_id,
            &self.public,
            &opened.agent_key,
        )?;
        Ok(Channel::new(session_id, Side::Server, &keys))
    }
}

/// The agent's half: its public key and its channel. The caller signs
/// [`opened_signed_bytes`] with its certificate's key.
pub fn agent_handshake(
    session_id: &str,
    server_key: &[u8],
) -> Result<([u8; KEY_LEN], Channel), RemoteError> {
    if server_key.len() != KEY_LEN {
        return Err(RemoteError::BadKey);
    }
    let (private, public) = ephemeral()?;
    let keys = agree(private, server_key, session_id, server_key, &public)?;
    Ok((public, Channel::new(session_id, Side::Agent, &keys)))
}

/// What the agent's certificate key signs: the session and both keys,
/// each length-prefixed.
pub fn opened_signed_bytes(session_id: &str, server_key: &[u8], agent_key: &[u8]) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    for field in [session_id.as_bytes(), server_key, agent_key] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

fn ephemeral() -> Result<(EphemeralPrivateKey, [u8; KEY_LEN]), RemoteError> {
    let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new())
        .map_err(|_| RemoteError::Random)?;
    let public = private
        .compute_public_key()
        .map_err(|_| RemoteError::Random)?;
    let public: [u8; KEY_LEN] = public
        .as_ref()
        .try_into()
        .map_err(|_| RemoteError::Random)?;
    Ok((private, public))
}

/// One key per direction.
struct Keys {
    to_agent: [u8; KEY_LEN],
    to_server: [u8; KEY_LEN],
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn agree(
    private: EphemeralPrivateKey,
    peer: &[u8],
    session_id: &str,
    server_key: &[u8],
    agent_key: &[u8],
) -> Result<Keys, RemoteError> {
    if peer.len() != KEY_LEN {
        return Err(RemoteError::BadKey);
    }
    let info = opened_signed_bytes(session_id, server_key, agent_key);
    agreement::agree_ephemeral(private, &UnparsedPublicKey::new(&X25519, peer), |shared| {
        let mut out = [0u8; 2 * KEY_LEN];
        hkdf::Salt::new(hkdf::HKDF_SHA256, DOMAIN)
            .extract(shared)
            .expand(&[&info], Len(out.len()))
            .and_then(|okm| okm.fill(&mut out))
            .map(|()| out)
    })
    .map_err(|_| RemoteError::BadKey)?
    .map_err(|_| RemoteError::BadKey)
    .map(|out| {
        let mut keys = Keys {
            to_agent: [0; KEY_LEN],
            to_server: [0; KEY_LEN],
        };
        keys.to_agent.copy_from_slice(&out[..KEY_LEN]);
        keys.to_server.copy_from_slice(&out[KEY_LEN..]);
        keys
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Server,
    Agent,
}

/// Seals what one side sends and opens what it receives, in order.
pub struct Channel {
    session_id: String,
    side: Side,
    seal: LessSafeKey,
    open: LessSafeKey,
    sent: u64,
    received: u64,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("session_id", &self.session_id)
            .field("sent", &self.sent)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl Channel {
    fn new(session_id: &str, side: Side, keys: &Keys) -> Self {
        let key = |bytes: &[u8; KEY_LEN]| {
            LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, bytes).expect("32 bytes is a ChaCha20 key"),
            )
        };
        let (seal, open) = match side {
            Side::Server => (key(&keys.to_agent), key(&keys.to_server)),
            Side::Agent => (key(&keys.to_server), key(&keys.to_agent)),
        };
        Self {
            session_id: session_id.to_owned(),
            side,
            seal,
            open,
            sent: 0,
            received: 0,
        }
    }

    /// The bytes sealed with each frame besides its content: the session
    /// and the direction, so a frame is good for one place only.
    fn aad(&self, to_agent: bool) -> Vec<u8> {
        let mut aad = self.session_id.as_bytes().to_vec();
        aad.push(u8::from(to_agent));
        aad
    }

    /// `data` sealed, with its counter.
    pub fn seal(&mut self, data: &RemoteData) -> (u64, Vec<u8>) {
        let counter = self.sent;
        self.sent += 1;
        let mut buffer = data.encode_to_vec();
        let aad = self.aad(self.side == Side::Server);
        self.seal
            .seal_in_place_append_tag(nonce(counter), Aad::from(&aad), &mut buffer)
            .expect("a frame is far below ChaCha20-Poly1305's limits");
        (counter, buffer)
    }

    /// Opens the next frame from the other side.
    pub fn open(&mut self, counter: u64, sealed: &[u8]) -> Result<RemoteData, RemoteError> {
        if counter != self.received {
            return Err(RemoteError::OutOfOrder);
        }
        let mut buffer = sealed.to_vec();
        let aad = self.aad(self.side == Side::Agent);
        let plain = self
            .open
            .open_in_place(nonce(counter), Aad::from(&aad), &mut buffer)
            .map_err(|_| RemoteError::Unsealed)?;
        let data = RemoteData::decode(&*plain).map_err(|_| RemoteError::Unsealed)?;
        self.received += 1;
        Ok(data)
    }
}

fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0u8; aead::NONCE_LEN];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

/// Shorthands for the bodies of [`RemoteData`].
pub fn input(bytes: Vec<u8>) -> RemoteData {
    RemoteData {
        body: Some(remote_data::Body::Input(bytes)),
    }
}

pub fn output(bytes: Vec<u8>) -> RemoteData {
    RemoteData {
        body: Some(remote_data::Body::Output(bytes)),
    }
}

pub fn close(reason: impl Into<String>) -> RemoteData {
    RemoteData {
        body: Some(remote_data::Body::Close(reason.into())),
    }
}

/// A terminal size both sides accept.
pub fn valid_size(cols: u32, rows: u32) -> bool {
    (1..=MAX_COLS).contains(&cols) && (1..=MAX_ROWS).contains(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::v1::RemoteResize;

    struct Agent {
        cert: Vec<u8>,
        key: rcgen::KeyPair,
    }

    fn ca() -> rcgen::CertifiedIssuer<'static, rcgen::KeyPair> {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap()).unwrap()
    }

    fn agent(ca: &rcgen::CertifiedIssuer<'static, rcgen::KeyPair>, id: &str) -> Agent {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![rcgen::SanType::URI(
            format!("monitorflow://agent/{id}").try_into().unwrap(),
        )];
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, ca).unwrap();
        Agent {
            cert: cert.der().to_vec(),
            key,
        }
    }

    fn sign(key: &rcgen::KeyPair, message: &[u8]) -> Vec<u8> {
        let pair = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &key.serialize_der(),
            &SystemRandom::new(),
        )
        .unwrap();
        pair.sign(&SystemRandom::new(), message)
            .unwrap()
            .as_ref()
            .to_vec()
    }

    fn opened(agent: &Agent, session: &str, server_key: &[u8]) -> (RemoteOpened, Channel) {
        let (public, channel) = agent_handshake(session, server_key).unwrap();
        let signature = sign(
            &agent.key,
            &opened_signed_bytes(session, server_key, &public),
        );
        (
            RemoteOpened {
                agent_key: public.to_vec(),
                certificate: agent.cert.clone(),
                signature,
            },
            channel,
        )
    }

    fn trusted<'a>(
        ca: &'a rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
    ) -> impl FnOnce(&CertificateDer<'_>) -> Result<(), String> + 'a {
        move |cert| {
            if crate::tls::signed_by(cert, ca.der()) {
                Ok(())
            } else {
                Err("not ours".into())
            }
        }
    }

    #[test]
    fn both_sides_talk_in_order_and_nobody_else_can() {
        let ca = ca();
        let agent = agent(&ca, "web-1");
        let server = ServerHandshake::new().unwrap();
        let server_key = *server.public_key();
        let (opened, mut on_agent) = opened(&agent, "s1", &server_key);
        let mut on_server = server.finish("s1", "web-1", &opened, trusted(&ca)).unwrap();

        let (counter, sealed) = on_server.seal(&input(b"ls\r".to_vec()));
        assert_eq!(counter, 0);
        assert!(!sealed.windows(3).any(|w| w == b"ls\r"), "sealed in clear");
        assert_eq!(
            on_agent.open(counter, &sealed).unwrap(),
            input(b"ls\r".to_vec())
        );
        let resize = RemoteData {
            body: Some(remote_data::Body::Resize(RemoteResize {
                cols: 80,
                rows: 24,
            })),
        };
        let (counter, sealed) = on_server.seal(&resize);
        // Replayed, or out of order, it is refused.
        assert_eq!(
            on_agent.open(0, &sealed),
            Err(RemoteError::OutOfOrder),
            "replay"
        );
        assert_eq!(on_agent.open(counter, &sealed).unwrap(), resize);

        let (counter, sealed) = on_agent.seal(&output(b"file\n".to_vec()));
        assert_eq!(
            on_server.open(counter, &sealed).unwrap(),
            output(b"file\n".to_vec())
        );
        // A frame of one direction does not open in the other.
        let (counter, mut sealed) = on_agent.seal(&output(b"x".to_vec()));
        assert_eq!(on_agent.open(2, &sealed), Err(RemoteError::Unsealed));
        // Changed on the way, it is refused.
        sealed[0] ^= 1;
        assert_eq!(on_server.open(counter, &sealed), Err(RemoteError::Unsealed));
    }

    #[test]
    fn a_relay_cannot_stand_in_for_the_agent() {
        let ca = ca();
        let agent = agent(&ca, "web-1");
        let other = agent_of_another_ca();

        // Another agent's valid certificate.
        let neighbour = self::agent(&ca, "web-2");
        let server = ServerHandshake::new().unwrap();
        let (opened, _) = self::opened(&neighbour, "s1", server.public_key());
        assert_eq!(
            server
                .finish("s1", "web-1", &opened, trusted(&ca))
                .unwrap_err(),
            RemoteError::WrongAgent
        );

        // A certificate of a CA the server does not trust.
        let server = ServerHandshake::new().unwrap();
        let (opened, _) = self::opened(&other, "s1", server.public_key());
        assert!(matches!(
            server.finish("s1", "web-1", &opened, trusted(&ca)),
            Err(RemoteError::Untrusted(_))
        ));

        // The agent's certificate with the relay's own key.
        let server = ServerHandshake::new().unwrap();
        let (mut opened, _) = self::opened(&agent, "s1", server.public_key());
        let (relay_key, _) = agent_handshake("s1", server.public_key()).unwrap();
        opened.agent_key = relay_key.to_vec();
        assert_eq!(
            server
                .finish("s1", "web-1", &opened, trusted(&ca))
                .unwrap_err(),
            RemoteError::BadSignature
        );

        // A signature for another session.
        let server = ServerHandshake::new().unwrap();
        let (opened, _) = self::opened(&agent, "s2", server.public_key());
        assert_eq!(
            server
                .finish("s1", "web-1", &opened, trusted(&ca))
                .unwrap_err(),
            RemoteError::BadSignature
        );
    }

    fn agent_of_another_ca() -> Agent {
        agent(&ca(), "web-1")
    }

    #[test]
    fn bad_keys_and_sizes_are_refused() {
        assert_eq!(
            agent_handshake("s", &[1, 2, 3]).unwrap_err(),
            RemoteError::BadKey
        );
        assert!(valid_size(80, 24));
        assert!(!valid_size(0, 24));
        assert!(!valid_size(80, 0));
        assert!(!valid_size(MAX_COLS + 1, 24));
        assert!(!valid_size(80, MAX_ROWS + 1));
    }
}
