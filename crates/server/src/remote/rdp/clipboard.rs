//! The clipboard and file transfers of a desktop (ADR-0192), on the
//! proxy: what the `cliprdr` channel may carry, and what of it the audit
//! log says.
//!
//! IronRDP's web client copies text and pictures, and files, over the
//! static virtual channel `cliprdr` (MS-RDPECLIP). The proxy reassembles
//! each message of it, in each direction, and passes it on or drops it as
//! the session's switches say: without the clipboard, no clipboard data
//! but file lists; without files, no file contents, no locks, and no file
//! list. A format the proxy cannot name counts as clipboard data. What
//! passes is audited: each copy with its direction, kind and size, each
//! file with its name and size, never what they hold.

use std::collections::HashMap;

use ironrdp_cliprdr::pdu::{
    ClipboardGeneralCapabilityFlags, ClipboardPdu, FileContentsFlags, FormatDataResponse,
};
use ironrdp_core::decode;
use ironrdp_pdu::mcs::McsMessage;
use ironrdp_pdu::x224::X224;

/// The channel's name.
pub const CHANNEL: &str = "cliprdr";
/// The format of a file list, as Windows and IronRDP name it.
const FILE_LIST: &str = "FileGroupDescriptorW";
/// The largest message reassembled; a larger one is dropped.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// `CHANNEL_FLAG_FIRST`, `CHANNEL_FLAG_LAST` and `CHANNEL_PACKET_COMPRESSED`.
const FIRST: u32 = 0x01;
const LAST: u32 = 0x02;
const COMPRESSED: u32 = 0x0020_0000;

/// Which side sent a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The browser: what goes to the server.
    Client,
    /// The host's service: what comes from the server.
    Service,
}

impl Side {
    fn index(self) -> usize {
        self as usize
    }

    fn other(self) -> Self {
        match self {
            Side::Client => Side::Service,
            Side::Service => Side::Client,
        }
    }

    /// The direction of what this side sends, for the audit log.
    pub fn direction(self) -> &'static str {
        match self {
            Side::Client => "to_server",
            Side::Service => "from_server",
        }
    }
}

/// What the session allows on the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allow {
    pub clipboard: bool,
    pub files: bool,
}

/// What passed, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A copy from `from`: `kind` is `text`, `html`, `image` or the
    /// format's name.
    Copy {
        from: Side,
        kind: String,
        bytes: usize,
    },
    /// A file from `from`, the first time its contents were asked for.
    File {
        from: Side,
        name: String,
        size: Option<u64>,
    },
}

/// A message of one side, until its last chunk.
#[derive(Default)]
struct Partial {
    frames: Vec<Vec<u8>>,
    data: Vec<u8>,
}

/// A file a side offered.
#[derive(Debug, Clone)]
struct Offered {
    name: String,
    size: Option<u64>,
}

/// The channel's state, both sides.
pub struct Clipboard {
    allow: Allow,
    partial: [Option<Partial>; 2],
    /// The formats each side offered last, by ID.
    formats: [HashMap<u32, String>; 2],
    /// The format each side asked the other for, waiting for its answer.
    asked: [Option<u32>; 2],
    /// The files each side offered last, and whether each was audited.
    files: [Vec<(Offered, bool)>; 2],
    long_names: bool,
}

impl Clipboard {
    pub fn new(allow: Allow) -> Self {
        Self {
            allow,
            partial: [None, None],
            formats: [HashMap::new(), HashMap::new()],
            asked: [None, None],
            files: [Vec::new(), Vec::new()],
            long_names: true,
        }
    }

    /// A PDU of the channel from `side`, a whole X.224 `frame`: the frames
    /// to pass on now, the chunks of a message held until its last, and
    /// what to audit.
    pub fn pdu(&mut self, side: Side, frame: Vec<u8>) -> (Vec<Vec<u8>>, Vec<Event>) {
        if !self.allow.clipboard && !self.allow.files {
            return (Vec::new(), Vec::new());
        }
        let Some(chunk) = user_data(&frame) else {
            return (Vec::new(), Vec::new());
        };
        let Some((flags, data)) = channel_chunk(&chunk) else {
            return (Vec::new(), Vec::new());
        };
        let slot = &mut self.partial[side.index()];
        // Compressed data cannot be read: dropped, as anything unread.
        if flags & COMPRESSED != 0 {
            *slot = None;
            return (Vec::new(), Vec::new());
        }
        if flags & FIRST != 0 {
            *slot = Some(Partial::default());
        }
        let Some(partial) = slot.as_mut() else {
            return (Vec::new(), Vec::new());
        };
        if partial.data.len() + data.len() > MAX_MESSAGE {
            *slot = None;
            return (Vec::new(), Vec::new());
        }
        partial.frames.push(frame);
        partial.data.extend_from_slice(data);
        if flags & LAST == 0 {
            return (Vec::new(), Vec::new());
        }
        let Partial { frames, data } = slot.take().expect("just filled");
        let mut events = Vec::new();
        if self.message(side, &data, &mut events) {
            (frames, events)
        } else {
            (Vec::new(), Vec::new())
        }
    }

    /// Whether the whole message `data` from `side` passes; what passes
    /// and says something goes to `events`.
    fn message(&mut self, side: Side, data: &[u8], events: &mut Vec<Event>) -> bool {
        let Ok(pdu) = decode::<ClipboardPdu<'_>>(data) else {
            return false;
        };
        let (me, other) = (side.index(), side.other().index());
        match pdu {
            ClipboardPdu::MonitorReady
            | ClipboardPdu::FormatListResponse(_)
            | ClipboardPdu::TemporaryDirectory(_) => true,
            ClipboardPdu::Capabilities(caps) => {
                self.long_names &= caps
                    .flags()
                    .contains(ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES);
                true
            }
            ClipboardPdu::FormatList(list) => {
                let formats = list
                    .get_formats(self.long_names)
                    .or_else(|_| list.get_formats(!self.long_names))
                    .unwrap_or_default();
                self.formats[me] = formats
                    .iter()
                    .map(|f| {
                        let name = f.name().map(|n| n.value().to_owned()).unwrap_or_default();
                        (f.id().value(), name)
                    })
                    .collect();
                self.files[me].clear();
                true
            }
            ClipboardPdu::FormatDataRequest(request) => {
                let id = request.format.value();
                let allowed = if self.is_file_list(other, id) {
                    self.allow.files
                } else {
                    self.allow.clipboard
                };
                self.asked[me] = allowed.then_some(id);
                allowed
            }
            ClipboardPdu::FormatDataResponse(response) => {
                // Only the answer to a request that passed.
                let Some(id) = self.asked[other].take() else {
                    return false;
                };
                if response.is_error() {
                    return true;
                }
                if self.is_file_list(me, id) {
                    self.files[me] = file_list(&response);
                } else {
                    events.push(Event::Copy {
                        from: side,
                        kind: kind(id, self.formats[me].get(&id)),
                        bytes: response.data().len(),
                    });
                }
                true
            }
            ClipboardPdu::FileContentsRequest(request) => {
                if !self.allow.files {
                    return false;
                }
                // The other side's file, audited when its contents are first
                // asked for.
                let first =
                    request.flags.contains(FileContentsFlags::RANGE) && request.position == 0;
                if let Some((file, audited)) = usize::try_from(request.index)
                    .ok()
                    .and_then(|i| self.files[other].get_mut(i))
                {
                    if first && !*audited {
                        *audited = true;
                        events.push(Event::File {
                            from: side.other(),
                            name: file.name.clone(),
                            size: file.size,
                        });
                    }
                }
                true
            }
            ClipboardPdu::FileContentsResponse(_)
            | ClipboardPdu::LockData(_)
            | ClipboardPdu::UnlockData(_) => self.allow.files,
        }
    }

    /// Whether `id` is the file list format among those `side` offered.
    fn is_file_list(&self, side: usize, id: u32) -> bool {
        self.formats[side].get(&id).is_some_and(|n| n == FILE_LIST)
    }
}

/// The user data of a Send Data Request or Indication.
fn user_data(frame: &[u8]) -> Option<Vec<u8>> {
    match decode::<X224<McsMessage<'_>>>(frame).ok()?.0 {
        McsMessage::SendDataRequest(data) => Some(data.user_data.into_owned()),
        McsMessage::SendDataIndication(data) => Some(data.user_data.into_owned()),
        _ => None,
    }
}

/// A virtual channel chunk's flags and data, past its header.
fn channel_chunk(chunk: &[u8]) -> Option<(u32, &[u8])> {
    let header = chunk.get(..8)?;
    let flags = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    Some((flags, &chunk[8..]))
}

/// The files of a file list.
fn file_list(response: &FormatDataResponse<'_>) -> Vec<(Offered, bool)> {
    response
        .to_file_list()
        .map(|list| {
            list.files
                .into_iter()
                .map(|f| {
                    let name = match f.relative_path {
                        Some(path) if !path.is_empty() => format!("{path}\\{}", f.name),
                        _ => f.name,
                    };
                    (
                        Offered {
                            name,
                            size: f.file_size,
                        },
                        false,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a clipboard format holds, in a word.
fn kind(id: u32, name: Option<&String>) -> String {
    match (id, name.map(String::as_str)) {
        (1 | 7 | 13, _) => "text".into(),
        (2 | 8 | 17, _) | (_, Some("PNG" | "image/png" | "JFIF")) => "image".into(),
        (_, Some("HTML Format" | "text/html")) => "html".into(),
        (_, Some(name)) if !name.is_empty() => name.chars().take(64).collect(),
        _ => format!("format {id}"),
    }
}

#[cfg(test)]
mod tests;
