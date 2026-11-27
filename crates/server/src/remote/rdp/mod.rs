//! The RDP a desktop's proxy carries (ADR-0192), read as it passes.
//!
//! Past the TLS it does with the host's service, the central server sees
//! RDP in the clear both ways. [`Framer`] cuts each direction into whole
//! PDUs, so the proxy passes on whole PDUs only, and [`Connection`] reads
//! what the connection sequence says: the virtual channels and their IDs,
//! the bulk compression, the size of the desktop. The recorder
//! ([`recorder`]) decodes the screen from the service's PDUs with
//! IronRDP's own decoders, the ones its web client runs.
//!
//! Nothing here keeps or logs what it reads: the Client Info PDU, which
//! may carry the person's password, is read for its compression flags and
//! dropped.

pub mod recorder;
#[doc(hidden)]
pub mod sample;

use std::collections::HashMap;

use ironrdp_bulk::CompressionType;
use ironrdp_core::decode;
use ironrdp_pdu::gcc::ClientEarlyCapabilityFlags;
use ironrdp_pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
use ironrdp_pdu::mcs::{self, McsMessage};
use ironrdp_pdu::x224::{X224Data, X224};

/// Largest CredSSP message: a TSRequest is a few KiB.
const MAX_SECURITY: usize = 256 * 1024;
/// Bytes kept waiting for the rest of a PDU: TPKT and fast-path lengths
/// stop at 64 KiB.
const MAX_BUFFER: usize = MAX_SECURITY + 64 * 1024;
/// `PROTOCOL_HYBRID_EX` of the X.224 negotiation: the service sends an
/// Early User Authorization Result after CredSSP.
const HYBRID_EX: u32 = 0x08;
/// `SEC_INFO_PKT` of the basic security header: the Client Info PDU.
const SEC_INFO_PKT: u16 = 0x0040;
/// `INFO_COMPRESSION` and `CompressionTypeMask` of the Client Info flags.
const INFO_COMPRESSION: u32 = 0x0080;
const COMPRESSION_TYPE_MASK: u32 = 0x1e00;

/// What a PDU is, as far as framing goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// CredSSP, or the Early User Authorization Result: before RDP.
    Security,
    /// TPKT, X.224 and MCS: the slow path, virtual channels included.
    X224,
    /// The fast path: input from the client, screen from the service.
    FastPath,
}

/// One whole PDU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pdu {
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

/// Why a direction's bytes are not RDP.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the desktop's RDP could not be read: {0}")]
pub struct FrameError(&'static str);

/// Cuts one direction's bytes into whole PDUs.
pub struct Framer {
    buffer: Vec<u8>,
    /// No X.224 PDU yet: CredSSP messages, DER, may come.
    security: bool,
    /// The service's Early User Authorization Result is still to come.
    early_auth: bool,
}

impl Framer {
    /// The client's side.
    pub fn client() -> Self {
        Self {
            buffer: Vec::new(),
            security: true,
            early_auth: false,
        }
    }

    /// The service's side, for the security protocol its X.224 confirm
    /// selected.
    pub fn service(protocol: u32) -> Self {
        Self {
            early_auth: protocol & HYBRID_EX != 0,
            ..Self::client()
        }
    }

    /// The PDUs `bytes` completes, in order; the rest waits for more.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Pdu>, FrameError> {
        self.buffer.extend_from_slice(bytes);
        let mut pdus = Vec::new();
        let mut start = 0;
        while let Some((kind, length)) = self.next(&self.buffer[start..])? {
            if kind == Kind::X224 {
                self.security = false;
            }
            if kind == Kind::Security && self.early_auth && self.buffer[start] != 0x30 {
                self.early_auth = false;
            }
            pdus.push(Pdu {
                kind,
                bytes: self.buffer[start..start + length].to_vec(),
            });
            start += length;
        }
        self.buffer.drain(..start);
        if self.buffer.len() > MAX_BUFFER {
            return Err(FrameError("a PDU is too long"));
        }
        Ok(pdus)
    }

    /// The kind and length of the PDU at the start of `bytes`, once all of
    /// it is there.
    fn next(&self, bytes: &[u8]) -> Result<Option<(Kind, usize)>, FrameError> {
        let Some(&first) = bytes.first() else {
            return Ok(None);
        };
        let found = if self.security && first == 0x30 {
            der_length(bytes)?.map(|length| (Kind::Security, length))
        } else if self.security && self.early_auth && first != 0x03 {
            Some((Kind::Security, 4))
        } else {
            match ironrdp_pdu::find_size(bytes) {
                Ok(Some(info)) => {
                    let kind = match info.action {
                        ironrdp_pdu::Action::X224 => Kind::X224,
                        ironrdp_pdu::Action::FastPath => Kind::FastPath,
                    };
                    Some((kind, info.length))
                }
                Ok(None) => None,
                Err(_) => return Err(FrameError("a PDU has no valid header")),
            }
        };
        match found {
            Some((_, 0)) => Err(FrameError("a PDU is empty")),
            Some((kind, length)) if bytes.len() >= length => Ok(Some((kind, length))),
            _ => Ok(None),
        }
    }
}

/// The length of a DER SEQUENCE, header included, once its header is in.
fn der_length(bytes: &[u8]) -> Result<Option<usize>, FrameError> {
    let Some(&first) = bytes.get(1) else {
        return Ok(None);
    };
    let (header, length) = match first {
        0..=0x7f => (2, usize::from(first)),
        0x81..=0x83 => {
            let count = usize::from(first - 0x80);
            let Some(digits) = bytes.get(2..2 + count) else {
                return Ok(None);
            };
            let length = digits
                .iter()
                .fold(0usize, |n, &b| (n << 8) | usize::from(b));
            (2 + count, length)
        }
        _ => return Err(FrameError("a CredSSP message has no valid length")),
    };
    if header + length > MAX_SECURITY {
        return Err(FrameError("a CredSSP message is too long"));
    }
    Ok(Some(header + length))
}

/// The security protocol an X.224 connection confirm selected: 0 for
/// standard RDP security, without a negotiation response.
pub fn selected_protocol(confirm: &[u8]) -> u32 {
    // TPKT (4), X.224 header (7), then RDP_NEG_RSP: type 2, flags, length,
    // then the protocol.
    match confirm.get(11..19) {
        Some(rsp) if rsp[0] == 0x02 => u32::from_le_bytes([rsp[4], rsp[5], rsp[6], rsp[7]]),
        _ => 0,
    }
}

/// What a PDU of the client says, for whoever reads the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromClient {
    /// Where the pointer went, for the recording.
    Pointer {
        x: u16,
        y: u16,
    },
    /// The Client Info PDU: the bulk compression it asks for.
    Compression(Option<CompressionType>),
    /// Data of the static virtual channel `name`.
    Channel {
        name: String,
    },
    Other,
}

/// What a PDU of the service says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromService {
    /// The screen, the fast path.
    Screen,
    /// The I/O channel: activation, slow-path graphics and the rest.
    Io,
    /// Data of the static virtual channel `name`.
    Channel {
        name: String,
    },
    Other,
}

/// What the connection sequence said, read from both directions.
#[derive(Debug, Default)]
pub struct Connection {
    /// The static virtual channels the client asked for, in order.
    asked: Vec<String>,
    /// Their IDs, once the service answered.
    channels: HashMap<u16, String>,
    io_channel: Option<u16>,
    /// The client asked for the graphics pipeline (EGFX), which the
    /// recording cannot decode.
    pub wants_graphics_pipeline: bool,
    /// The bulk compression the client asked for.
    pub compression: Option<CompressionType>,
}

impl Connection {
    /// The I/O channel's ID, once the service said it.
    pub fn io_channel(&self) -> Option<u16> {
        self.io_channel
    }

    /// The name of the static virtual channel `id`.
    pub fn channel(&self, id: u16) -> Option<&str> {
        self.channels.get(&id).map(String::as_str)
    }

    pub fn from_client(&mut self, pdu: &Pdu) -> FromClient {
        match pdu.kind {
            Kind::Security => FromClient::Other,
            Kind::FastPath => pointer(&pdu.bytes).unwrap_or(FromClient::Other),
            Kind::X224 => {
                if self.asked.is_empty() && self.io_channel.is_none() && self.connect_initial(pdu) {
                    return FromClient::Other;
                }
                let Ok(X224(McsMessage::SendDataRequest(data))) =
                    decode::<X224<McsMessage<'_>>>(&pdu.bytes)
                else {
                    return FromClient::Other;
                };
                if Some(data.channel_id) == self.io_channel {
                    if let Some(compression) = client_info_compression(&data.user_data) {
                        self.compression = compression;
                        return FromClient::Compression(compression);
                    }
                    return FromClient::Other;
                }
                match self.channels.get(&data.channel_id) {
                    Some(name) => FromClient::Channel { name: name.clone() },
                    None => FromClient::Other,
                }
            }
        }
    }

    /// Reads the client's MCS Connect Initial; false if `pdu` is not one.
    fn connect_initial(&mut self, pdu: &Pdu) -> bool {
        let Ok(X224(data)) = decode::<X224<X224Data<'_>>>(&pdu.bytes) else {
            return false;
        };
        let Ok(initial) = decode::<mcs::ConnectInitial>(data.data.as_ref()) else {
            return false;
        };
        let blocks = initial.conference_create_request.gcc_blocks();
        self.asked = blocks
            .network
            .as_ref()
            .map(|network| {
                network
                    .channels
                    .iter()
                    .map(|c| c.name.as_str().unwrap_or_default().to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        self.wants_graphics_pipeline = blocks
            .core
            .optional_data
            .early_capability_flags
            .is_some_and(|f| f.contains(ClientEarlyCapabilityFlags::SUPPORT_DYN_VC_GFX_PROTOCOL));
        true
    }

    pub fn from_service(&mut self, pdu: &Pdu) -> FromService {
        match pdu.kind {
            Kind::Security => FromService::Other,
            Kind::FastPath => FromService::Screen,
            Kind::X224 => {
                if self.io_channel.is_none() && self.connect_response(pdu) {
                    return FromService::Other;
                }
                match decode::<X224<McsMessage<'_>>>(&pdu.bytes) {
                    Ok(X224(McsMessage::SendDataIndication(data))) => {
                        if Some(data.channel_id) == self.io_channel {
                            FromService::Io
                        } else if let Some(name) = self.channels.get(&data.channel_id) {
                            FromService::Channel { name: name.clone() }
                        } else {
                            FromService::Other
                        }
                    }
                    _ => FromService::Other,
                }
            }
        }
    }

    /// Reads the service's MCS Connect Response; false if `pdu` is not
    /// one.
    fn connect_response(&mut self, pdu: &Pdu) -> bool {
        let Ok(X224(data)) = decode::<X224<X224Data<'_>>>(&pdu.bytes) else {
            return false;
        };
        let Ok(response) = decode::<mcs::ConnectResponse>(data.data.as_ref()) else {
            return false;
        };
        let network = &response.conference_create_response.gcc_blocks().network;
        self.io_channel = Some(network.io_channel);
        self.channels = network
            .channel_ids
            .iter()
            .copied()
            .zip(self.asked.iter().cloned())
            .collect();
        true
    }
}

/// Where the last mouse event of a fast-path input PDU put the pointer.
fn pointer(bytes: &[u8]) -> Option<FromClient> {
    let input = decode::<FastPathInput>(bytes).ok()?;
    input
        .input_events()
        .iter()
        .rev()
        .find_map(|event| match event {
            FastPathInputEvent::MouseEvent(m) => Some(FromClient::Pointer {
                x: m.x_position,
                y: m.y_position,
            }),
            FastPathInputEvent::MouseEventEx(m) => Some(FromClient::Pointer {
                x: m.x_position,
                y: m.y_position,
            }),
            _ => None,
        })
}

/// The compression a Client Info PDU asks for: `Some(None)` for none,
/// `None` when `user_data` is not a Client Info PDU. Reads its flags only:
/// the rest holds the account and, often, the password.
fn client_info_compression(user_data: &[u8]) -> Option<Option<CompressionType>> {
    let header = user_data.get(..4)?;
    if u16::from_le_bytes([header[0], header[1]]) & SEC_INFO_PKT == 0 {
        return None;
    }
    // The basic security header (4), the code page (4), then the flags.
    let flags = user_data.get(8..12)?;
    let flags = u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]);
    if flags & INFO_COMPRESSION == 0 {
        return Some(None);
    }
    Some(Some(match (flags & COMPRESSION_TYPE_MASK) >> 9 {
        0 => CompressionType::Rdp4,
        1 => CompressionType::Rdp5,
        2 => CompressionType::Rdp6,
        _ => CompressionType::Rdp61,
    }))
}

#[cfg(test)]
mod tests;
