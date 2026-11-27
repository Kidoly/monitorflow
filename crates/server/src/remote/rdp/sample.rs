//! Real RDP PDUs, made with IronRDP's encoders, for tests: of this
//! crate's, and of the agent's against a fake RDP service.

use std::borrow::Cow;

use ironrdp_core::encode_vec;
use ironrdp_pdu::bitmap::{BitmapData, BitmapUpdateData, Compression};
use ironrdp_pdu::fast_path::{
    EncryptionFlags, FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode,
};
use ironrdp_pdu::gcc::{
    ChannelDef, ChannelName, ChannelOptions, ClientCoreData, ClientCoreOptionalData,
    ClientEarlyCapabilityFlags, ClientGccBlocks, ClientNetworkData, ClientSecurityData, ColorDepth,
    ConferenceCreateRequest, ConferenceCreateResponse, HighColorDepth, KeyboardType, RdpVersion,
    SecureAccessSequence, ServerCoreData, ServerCoreOptionalData, ServerGccBlocks,
    ServerNetworkData, ServerSecurityData, SupportedColorDepths,
};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::mcs::{self, DomainParameters};
use ironrdp_pdu::rdp::capability_sets::{
    Bitmap, BitmapDrawingFlags, CapabilitySet, DemandActive, ServerDemandActive,
};
use ironrdp_pdu::rdp::headers::{ShareControlHeader, ShareControlPdu};
use ironrdp_pdu::x224::{X224Data, X224};

/// The I/O channel of [`connect_response`].
pub const IO_CHANNEL: u16 = 1003;
/// The user channel the service gives.
pub const USER_CHANNEL: u16 = 1007;
/// The first static channel's ID; the others follow.
pub const FIRST_CHANNEL: u16 = 1004;

fn x224_data(payload: Vec<u8>) -> Vec<u8> {
    encode_vec(&X224(X224Data {
        data: Cow::Owned(payload),
    }))
    .expect("encodes")
}

/// The client's MCS Connect Initial, asking for `channels`, and for the
/// graphics pipeline when `gfx`.
pub fn connect_initial(channels: &[&str], gfx: bool) -> Vec<u8> {
    let mut flags = ClientEarlyCapabilityFlags::SUPPORT_ERR_INFO_PDU;
    if gfx {
        flags |= ClientEarlyCapabilityFlags::SUPPORT_DYN_VC_GFX_PROTOCOL;
    }
    let core = ClientCoreData {
        version: RdpVersion::V5_PLUS,
        desktop_width: 1024,
        desktop_height: 576,
        color_depth: ColorDepth::Bpp8,
        sec_access_sequence: SecureAccessSequence::Del,
        keyboard_layout: 0,
        client_build: 0,
        client_name: "test".into(),
        keyboard_type: KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        optional_data: ClientCoreOptionalData {
            post_beta2_color_depth: Some(ColorDepth::Bpp8),
            client_product_id: Some(1),
            serial_number: Some(0),
            high_color_depth: Some(HighColorDepth::Bpp24),
            supported_color_depths: Some(SupportedColorDepths::BPP32),
            early_capability_flags: Some(flags),
            ..Default::default()
        },
    };
    let network = ClientNetworkData {
        channels: channels
            .iter()
            .map(|name| ChannelDef {
                name: ChannelName::from_utf8(name).expect("a short name"),
                options: ChannelOptions::INITIALIZED,
            })
            .collect(),
    };
    let blocks = ClientGccBlocks {
        core,
        security: ClientSecurityData::no_security(),
        network: Some(network),
        cluster: None,
        monitor: None,
        message_channel: None,
        multi_transport_channel: None,
        monitor_extended: None,
    };
    let initial = mcs::ConnectInitial {
        conference_create_request: ConferenceCreateRequest::new(blocks).expect("small"),
        calling_domain_selector: vec![1],
        called_domain_selector: vec![1],
        upward_flag: true,
        target_parameters: DomainParameters::target(),
        min_parameters: DomainParameters::min(),
        max_parameters: DomainParameters::max(),
    };
    x224_data(encode_vec(&initial).expect("encodes"))
}

/// The service's MCS Connect Response for `channels` static channels.
pub fn connect_response(channels: usize) -> Vec<u8> {
    let blocks = ServerGccBlocks {
        core: ServerCoreData {
            version: RdpVersion::V5_PLUS,
            optional_data: ServerCoreOptionalData::default(),
        },
        network: ServerNetworkData {
            channel_ids: (0..channels as u16).map(|i| FIRST_CHANNEL + i).collect(),
            io_channel: IO_CHANNEL,
        },
        security: ServerSecurityData::no_security(),
        message_channel: None,
        multi_transport_channel: None,
    };
    let response = mcs::ConnectResponse {
        conference_create_response: ConferenceCreateResponse::new(USER_CHANNEL, blocks)
            .expect("small"),
        called_connect_id: 1,
        domain_parameters: DomainParameters::target(),
    };
    x224_data(encode_vec(&response).expect("encodes"))
}

/// A Send Data Indication of the service on channel `channel`.
pub fn indication(channel: u16, user_data: Vec<u8>) -> Vec<u8> {
    encode_vec(&X224(mcs::SendDataIndication {
        initiator_id: USER_CHANNEL,
        channel_id: channel,
        user_data: Cow::Owned(user_data),
    }))
    .expect("encodes")
}

/// A Send Data Request of the client on channel `channel`.
pub fn request(channel: u16, user_data: Vec<u8>) -> Vec<u8> {
    encode_vec(&X224(mcs::SendDataRequest {
        initiator_id: USER_CHANNEL,
        channel_id: channel,
        user_data: Cow::Owned(user_data),
    }))
    .expect("encodes")
}

/// The service's Demand Active: a desktop of `width` by `height`.
pub fn demand_active(width: u16, height: u16) -> Vec<u8> {
    let header = ShareControlHeader {
        share_control_pdu: ShareControlPdu::ServerDemandActive(ServerDemandActive {
            pdu: DemandActive {
                source_descriptor: "RDP".into(),
                capability_sets: vec![CapabilitySet::Bitmap(Bitmap {
                    pref_bits_per_pix: 32,
                    desktop_width: width,
                    desktop_height: height,
                    desktop_resize_flag: true,
                    drawing_flags: BitmapDrawingFlags::empty(),
                })],
            },
        }),
        pdu_source: USER_CHANNEL,
        share_id: 0x0001_03ea,
    };
    indication(IO_CHANNEL, encode_vec(&header).expect("encodes"))
}

/// A fast-path bitmap update painting the rectangle at `x`, `y` of
/// `width` by `height` in the colour `rgb`, uncompressed.
pub fn bitmap(x: u16, y: u16, width: u16, height: u16, rgb: [u8; 3]) -> Vec<u8> {
    // Uncompressed 32 bits per pixel: blue, green, red, then unused.
    let pixel = [rgb[2], rgb[1], rgb[0], 0];
    let data: Vec<u8> = pixel
        .iter()
        .copied()
        .cycle()
        .take(usize::from(width) * usize::from(height) * 4)
        .collect();
    let update = BitmapUpdateData {
        rectangles: vec![BitmapData {
            rectangle: InclusiveRectangle {
                left: x,
                top: y,
                right: x + width - 1,
                bottom: y + height - 1,
            },
            width,
            height,
            bits_per_pixel: 32,
            compression_flags: Compression::empty(),
            compressed_data_header: None,
            bitmap_data: &data,
        }],
    };
    let update = encode_vec(&update).expect("encodes");
    let pdu = encode_vec(&FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: UpdateCode::Bitmap,
        compression_flags: None,
        compression_type: None,
        data: &update,
    })
    .expect("encodes");
    let mut out =
        encode_vec(&FastPathHeader::new(EncryptionFlags::empty(), pdu.len())).expect("encodes");
    out.extend_from_slice(&pdu);
    out
}
