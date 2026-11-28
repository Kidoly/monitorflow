use ironrdp_core::encode_vec;
use ironrdp_pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::MousePdu;

use super::sample::{self, FIRST_CHANNEL, IO_CHANNEL};
use super::*;

/// A CredSSP TSRequest's start: a DER SEQUENCE of 300 bytes.
fn ts_request() -> Vec<u8> {
    let mut der = vec![0x30, 0x82, 0x01, 0x2c];
    der.extend(std::iter::repeat_n(7, 300));
    der
}

/// The PDUs of `stream` pushed to `framer` one byte at a time.
fn byte_by_byte(framer: &mut Framer, stream: &[u8]) -> Vec<Pdu> {
    stream
        .iter()
        .flat_map(|b| framer.push(&[*b]).unwrap())
        .collect()
}

#[test]
fn whole_pdus_come_out_however_the_bytes_arrive() {
    let initial = sample::connect_initial(&["cliprdr"], false);
    let screen = sample::bitmap(0, 0, 4, 4, [1, 2, 3]);
    let mut stream = ts_request();
    stream.extend_from_slice(&initial);
    stream.extend_from_slice(&screen);
    let pdus = byte_by_byte(&mut Framer::client(), &stream);
    let kinds: Vec<Kind> = pdus.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [Kind::Security, Kind::X224, Kind::FastPath]);
    assert_eq!(pdus[1].bytes, initial);
    assert_eq!(pdus[2].bytes, screen);
    // All at once, the same.
    let mut framer = Framer::client();
    assert_eq!(framer.push(&stream).unwrap(), pdus);
}

#[test]
fn credssp_ends_with_the_first_x224_pdu() {
    let mut framer = Framer::client();
    framer.push(&sample::connect_initial(&[], false)).unwrap();
    // Fast-path input with twelve events starts with 0x30: not DER now.
    let input = [0x30, 0x03, 0x00];
    let pdus = framer.push(&input).unwrap();
    assert_eq!(pdus[0].kind, Kind::FastPath);
}

#[test]
fn the_early_user_authorization_result_is_four_bytes() {
    let mut stream = ts_request();
    stream.extend_from_slice(&[0, 0, 0, 0]);
    let response = sample::connect_response(1);
    stream.extend_from_slice(&response);
    let pdus = byte_by_byte(&mut Framer::service(0x08 | 0x02), &stream);
    let lengths: Vec<(Kind, usize)> = pdus.iter().map(|p| (p.kind, p.bytes.len())).collect();
    assert_eq!(
        lengths,
        [
            (Kind::Security, 304),
            (Kind::Security, 4),
            (Kind::X224, response.len())
        ]
    );
    // Without HYBRID_EX, the same four bytes are not RDP.
    let mut framer = Framer::service(0x02);
    assert!(framer.push(&[0x05, 0, 0, 0, 0x30]).is_err());
}

#[test]
fn what_is_not_rdp_is_refused() {
    assert!(Framer::client().push(b"GET / HTTP/1.1\r\n").is_err());
    // A DER length of five bytes.
    assert!(Framer::client().push(&[0x30, 0x85, 1, 1, 1, 1, 1]).is_err());
    // A TPKT shorter than its own header.
    assert!(Framer::client().push(&[3, 0, 0, 2]).is_err());
}

#[test]
fn the_confirm_names_the_protocol() {
    let tls = [
        3, 0, 0, 19, 14, 0xd0, 0, 0, 0x12, 0x34, 0, 2, 0, 8, 0, 1, 0, 0, 0,
    ];
    assert_eq!(selected_protocol(&tls), 1);
    let mut hybrid_ex = tls;
    hybrid_ex[15] = 0x0a;
    assert_eq!(selected_protocol(&hybrid_ex), 0x0a);
    assert_eq!(selected_protocol(&tls[..11]), 0);
}

fn pdu(kind: Kind, bytes: Vec<u8>) -> Pdu {
    Pdu { kind, bytes }
}

/// A Client Info PDU's start: the basic security header, the code page
/// and the flags.
fn client_info(flags: u32) -> Vec<u8> {
    let mut data = vec![0x40, 0, 0, 0, 0, 0, 0, 0];
    data.extend_from_slice(&flags.to_le_bytes());
    data.extend_from_slice(b"\0\0alice\0secret\0");
    data
}

#[test]
fn the_connection_sequence_names_channels_and_compression() {
    let mut connection = Connection::default();
    let initial = pdu(
        Kind::X224,
        sample::connect_initial(&["rdpsnd", "CLIPRDR"], true),
    );
    assert_eq!(connection.from_client(&initial), FromClient::Other);
    assert!(connection.wants_graphics_pipeline);
    assert_eq!(connection.io_channel(), None);
    let response = pdu(Kind::X224, sample::connect_response(2));
    assert_eq!(connection.from_service(&response), FromService::Other);
    assert_eq!(connection.io_channel(), Some(IO_CHANNEL));
    assert_eq!(connection.channel(FIRST_CHANNEL), Some("rdpsnd"));
    assert_eq!(connection.channel(FIRST_CHANNEL + 1), Some("cliprdr"));

    let info = pdu(
        Kind::X224,
        sample::request(IO_CHANNEL, client_info(0x80 | (2 << 9))),
    );
    assert_eq!(
        connection.from_client(&info),
        FromClient::Compression(Some(CompressionType::Rdp6))
    );
    assert_eq!(connection.compression, Some(CompressionType::Rdp6));
    let info = pdu(Kind::X224, sample::request(IO_CHANNEL, client_info(0)));
    connection.from_client(&info);
    assert_eq!(connection.compression, None);

    let clip = pdu(
        Kind::X224,
        sample::request(FIRST_CHANNEL + 1, vec![1, 2, 3]),
    );
    assert_eq!(
        connection.from_client(&clip),
        FromClient::Channel {
            name: "cliprdr".into()
        }
    );
    let clip = pdu(
        Kind::X224,
        sample::indication(FIRST_CHANNEL + 1, vec![1, 2, 3]),
    );
    assert_eq!(
        connection.from_service(&clip),
        FromService::Channel {
            name: "cliprdr".into()
        }
    );
    let io = pdu(Kind::X224, sample::demand_active(800, 600));
    assert_eq!(connection.from_service(&io), FromService::Io);
    let screen = pdu(Kind::FastPath, sample::bitmap(0, 0, 1, 1, [0, 0, 0]));
    assert_eq!(connection.from_service(&screen), FromService::Screen);
    let unknown = pdu(Kind::X224, sample::indication(2000, vec![1]));
    assert_eq!(connection.from_service(&unknown), FromService::Other);
}

#[test]
fn the_pointer_follows_the_clients_mouse() {
    let input = FastPathInput::new(vec![
        FastPathInputEvent::MouseEvent(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: 10,
            y_position: 20,
        }),
        FastPathInputEvent::MouseEvent(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: 30,
            y_position: 40,
        }),
    ])
    .unwrap();
    let mut connection = Connection::default();
    let moved = pdu(Kind::FastPath, encode_vec(&input).unwrap());
    assert_eq!(
        connection.from_client(&moved),
        FromClient::Pointer { x: 30, y: 40 }
    );
}
