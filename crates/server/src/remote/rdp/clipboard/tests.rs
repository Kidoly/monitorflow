use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardFormatName, FileContentsFlags,
    FileContentsRequest, FileContentsResponse, FileDescriptor, FormatDataRequest, FormatList,
    LockDataId, PackedFileList,
};
use ironrdp_core::encode_vec;

use super::super::sample;
use super::*;

const CLIP: u16 = sample::FIRST_CHANNEL;
const TEXT: u32 = 13;
const FILES: u32 = 0xc0a0;

/// `pdu` from `side` in chunks of at most `size` bytes, each a whole
/// Send Data PDU.
fn chunks(side: Side, pdu: &ClipboardPdu<'_>, size: usize) -> Vec<Vec<u8>> {
    let data = encode_vec(pdu).unwrap();
    let parts: Vec<&[u8]> = data.chunks(size).collect();
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let mut flags = 0u32;
            if i == 0 {
                flags |= FIRST;
            }
            if i + 1 == parts.len() {
                flags |= LAST;
            }
            let mut user_data = (data.len() as u32).to_le_bytes().to_vec();
            user_data.extend_from_slice(&flags.to_le_bytes());
            user_data.extend_from_slice(part);
            match side {
                Side::Client => sample::request(CLIP, user_data),
                Side::Service => sample::indication(CLIP, user_data),
            }
        })
        .collect()
}

fn whole(side: Side, pdu: &ClipboardPdu<'_>) -> Vec<u8> {
    chunks(side, pdu, usize::MAX).remove(0)
}

/// Whether `pdu` from `side` passes, and what it says.
fn send(clipboard: &mut Clipboard, side: Side, pdu: ClipboardPdu<'_>) -> (bool, Vec<Event>) {
    let frame = whole(side, &pdu);
    let (frames, events) = clipboard.pdu(side, frame.clone());
    assert!(frames.is_empty() || frames == [frame]);
    (!frames.is_empty(), events)
}

/// The service offers text and a file list.
fn offered(clipboard: &mut Clipboard) {
    let list = FormatList::new_unicode(
        &[
            ClipboardFormat::new(ClipboardFormatId(TEXT)),
            ClipboardFormat::new(ClipboardFormatId(FILES))
                .with_name(ClipboardFormatName::new("FileGroupDescriptorW")),
        ],
        true,
    )
    .unwrap();
    assert!(send(clipboard, Side::Service, ClipboardPdu::FormatList(list)).0);
}

fn ask(format: u32) -> ClipboardPdu<'static> {
    ClipboardPdu::FormatDataRequest(FormatDataRequest {
        format: ClipboardFormatId(format),
    })
}

fn text(value: &str) -> ClipboardPdu<'static> {
    ClipboardPdu::FormatDataResponse(FormatDataResponse::new_unicode_string(value))
}

fn file_list() -> ClipboardPdu<'static> {
    let list = PackedFileList {
        files: vec![FileDescriptor::new("report.pdf").with_file_size(2048)],
    };
    ClipboardPdu::FormatDataResponse(FormatDataResponse::new_file_list(&list).unwrap())
}

fn contents(index: i32, position: u64) -> ClipboardPdu<'static> {
    ClipboardPdu::FileContentsRequest(FileContentsRequest {
        stream_id: 1,
        index,
        flags: FileContentsFlags::RANGE,
        position,
        requested_size: 1024,
        data_id: None,
    })
}

#[test]
fn without_clipboard_and_files_nothing_passes() {
    let mut clipboard = Clipboard::new(Allow {
        clipboard: false,
        files: false,
    });
    assert!(!send(&mut clipboard, Side::Service, ClipboardPdu::MonitorReady).0);
    assert!(!send(&mut clipboard, Side::Client, ask(TEXT)).0);
}

#[test]
fn the_clipboard_passes_and_is_audited_but_not_files() {
    let mut clipboard = Clipboard::new(Allow {
        clipboard: true,
        files: false,
    });
    assert!(send(&mut clipboard, Side::Service, ClipboardPdu::MonitorReady).0);
    offered(&mut clipboard);
    assert!(send(&mut clipboard, Side::Client, ask(TEXT)).0);
    let (passed, events) = send(&mut clipboard, Side::Service, text("hello"));
    assert!(passed);
    assert_eq!(
        events,
        [Event::Copy {
            from: Side::Service,
            kind: "text".into(),
            bytes: 12
        }]
    );
    // An answer nobody asked for.
    assert!(!send(&mut clipboard, Side::Service, text("again")).0);
    // Files: neither the list nor their contents.
    assert!(!send(&mut clipboard, Side::Client, ask(FILES)).0);
    assert!(!send(&mut clipboard, Side::Service, file_list()).0);
    assert!(!send(&mut clipboard, Side::Client, contents(0, 0)).0);
    let response = FileContentsResponse::new_data_response(1, vec![1, 2, 3]);
    assert!(
        !send(
            &mut clipboard,
            Side::Service,
            ClipboardPdu::FileContentsResponse(response)
        )
        .0
    );
    assert!(
        !send(
            &mut clipboard,
            Side::Client,
            ClipboardPdu::LockData(LockDataId(1))
        )
        .0
    );
}

#[test]
fn files_pass_and_each_is_audited_once_but_not_the_clipboard() {
    let mut clipboard = Clipboard::new(Allow {
        clipboard: false,
        files: true,
    });
    offered(&mut clipboard);
    assert!(!send(&mut clipboard, Side::Client, ask(TEXT)).0);
    assert!(!send(&mut clipboard, Side::Service, text("secret")).0);
    assert!(send(&mut clipboard, Side::Client, ask(FILES)).0);
    let (passed, events) = send(&mut clipboard, Side::Service, file_list());
    assert!(passed && events.is_empty());
    assert!(
        send(
            &mut clipboard,
            Side::Client,
            ClipboardPdu::LockData(LockDataId(1))
        )
        .0
    );
    let (passed, events) = send(&mut clipboard, Side::Client, contents(0, 0));
    assert!(passed);
    assert_eq!(
        events,
        [Event::File {
            from: Side::Service,
            name: "report.pdf".into(),
            size: Some(2048)
        }]
    );
    let (passed, events) = send(&mut clipboard, Side::Client, contents(0, 1024));
    assert!(passed && events.is_empty());
    assert!(send(&mut clipboard, Side::Client, contents(0, 0))
        .1
        .is_empty());
    // A file nobody offered passes unaudited: the other side answers it
    // with an error.
    assert!(send(&mut clipboard, Side::Client, contents(5, 0))
        .1
        .is_empty());
}

#[test]
fn a_message_in_chunks_passes_whole_or_not_at_all() {
    let mut clipboard = Clipboard::new(Allow {
        clipboard: true,
        files: false,
    });
    offered(&mut clipboard);
    assert!(send(&mut clipboard, Side::Client, ask(TEXT)).0);
    let long = "x".repeat(5000);
    let parts = chunks(Side::Service, &text(&long), 1600);
    assert_eq!(parts.len(), 7);
    let mut passed = Vec::new();
    let mut events = Vec::new();
    for part in &parts {
        let (frames, said) = clipboard.pdu(Side::Service, part.clone());
        passed.extend(frames);
        events.extend(said);
    }
    assert_eq!(passed, parts);
    assert_eq!(events.len(), 1);

    // Compressed: unread, dropped.
    let mut frame = whole(Side::Client, &ask(TEXT));
    let at = frame.len() - encode_vec(&ask(TEXT)).unwrap().len() - 4;
    frame[at + 2] |= 0x20;
    assert!(clipboard.pdu(Side::Client, frame).0.is_empty());
    // Not a channel chunk at all.
    assert!(clipboard
        .pdu(Side::Client, sample::request(CLIP, vec![1, 2]))
        .0
        .is_empty());
}

#[test]
fn kinds_are_named_in_a_word() {
    assert_eq!(kind(13, None), "text");
    assert_eq!(kind(8, None), "image");
    assert_eq!(kind(0xc001, Some(&"HTML Format".to_owned())), "html");
    assert_eq!(
        kind(0xc002, Some(&"Rich Text Format".to_owned())),
        "Rich Text Format"
    );
    assert_eq!(kind(0xc003, None), "format 49155");
}
