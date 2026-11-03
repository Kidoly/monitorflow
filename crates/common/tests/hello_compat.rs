//! `Hello.command_kinds` (ADR-0076) and the hardware fields (ADR-0093)
//! between versions. `OldHello` is the
//! message of 0.1.0, field for field. Relays decode every envelope and
//! encode it again, and prost keeps no unknown fields, so a 0.1.0 relay
//! passes on what `OldHello` holds.

use monitorflow_common::proto::v1::{CommandAcceptance, CommandKindState, Hello};
use prost::Message;

#[derive(Clone, PartialEq, Message)]
struct OldHello {
    #[prost(string, tag = "1")]
    agent_id: String,
    #[prost(string, tag = "2")]
    protocol_version: String,
    #[prost(string, tag = "3")]
    agent_version: String,
    #[prost(string, tag = "4")]
    hostname: String,
    #[prost(string, tag = "5")]
    os_family: String,
    #[prost(string, tag = "6")]
    os_version: String,
    #[prost(bool, tag = "7")]
    no_package_manager: bool,
}

fn reporting() -> Hello {
    Hello {
        agent_id: "web-01".into(),
        protocol_version: "1.1".into(),
        agent_version: "0.2.0".into(),
        hostname: "web-01.example".into(),
        os_family: "debian".into(),
        os_version: "debian 12".into(),
        no_package_manager: true,
        command_kinds: vec![CommandKindState {
            kind: "shell".into(),
            acceptance: CommandAcceptance::Disabled.into(),
        }],
        system_uuid: "1b3e2f80-6c0a-4e1b-9c3d-2a4b5c6d7e8f".into(),
        mac_addresses: vec!["bc:24:11:00:00:01".into()],
    }
}

#[test]
fn an_old_reader_takes_a_new_hello_whole() {
    let new = reporting();
    let old = OldHello::decode(new.encode_to_vec().as_slice()).unwrap();
    assert_eq!(old.agent_id, new.agent_id);
    assert_eq!(old.protocol_version, new.protocol_version);
    assert_eq!(old.agent_version, new.agent_version);
    assert_eq!(old.hostname, new.hostname);
    assert_eq!(old.os_family, new.os_family);
    assert_eq!(old.os_version, new.os_version);
    assert!(old.no_package_manager);
}

#[test]
fn an_old_relay_loses_the_report_and_nothing_else() {
    let new = reporting();
    let old = OldHello::decode(new.encode_to_vec().as_slice()).unwrap();
    let passed_on = Hello::decode(old.encode_to_vec().as_slice()).unwrap();
    assert!(passed_on.command_kinds.is_empty());
    assert_eq!(
        passed_on,
        Hello {
            command_kinds: Vec::new(),
            system_uuid: String::new(),
            mac_addresses: Vec::new(),
            ..new
        }
    );
}

#[test]
fn an_old_agent_reports_nothing() {
    let old = OldHello {
        agent_id: "web-01".into(),
        ..OldHello::default()
    };
    let hello = Hello::decode(old.encode_to_vec().as_slice()).unwrap();
    assert!(hello.command_kinds.is_empty());
}

/// A value from a newer agent decodes, and reads as unspecified.
#[test]
fn unknown_values_survive_decoding() {
    let mut hello = reporting();
    hello.command_kinds[0].acceptance = 99;
    let decoded = Hello::decode(hello.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.command_kinds[0].acceptance, 99);
    assert_eq!(
        decoded.command_kinds[0].acceptance(),
        CommandAcceptance::Unspecified
    );
}
