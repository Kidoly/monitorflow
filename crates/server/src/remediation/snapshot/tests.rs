use std::time::Duration;

use monitorflow_common::redact::Secret;
use uuid::Uuid;

use super::store::{beyond_retention, VmRef};
use super::*;

fn vm(cluster: &str, node: &str, vmid: u32) -> VmRef {
    VmRef {
        cluster: cluster.into(),
        node: node.into(),
        vmid,
    }
}

fn options() -> ProxmoxOptions {
    ProxmoxOptions {
        request_timeout: Duration::from_secs(5),
        task_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(10),
    }
}

#[test]
fn names_are_deterministic_bounded_and_parse_back() {
    let incident = Uuid::parse_str("0123abcd-ef45-6789-abcd-ef0123456789").unwrap();
    let name = SnapshotName::for_step(incident, 7);
    assert_eq!(name.as_str(), "mf-0123abcdef45-7");
    assert_eq!(name, SnapshotName::for_step(incident, 7));
    assert_ne!(name, SnapshotName::for_step(incident, 8));
    let longest = SnapshotName::for_step(incident, u32::MAX);
    assert!(longest.as_str().len() <= 40, "Proxmox caps names at 40");
    assert_eq!(SnapshotName::parse(longest.as_str()), Some(longest));
    assert_eq!(SnapshotName::parse("mf-0123abcdef45-7"), Some(name));
    for bad in [
        "",
        "current",
        "mf-0123abcdef45",
        "mf-0123ABCDEF45-1",
        "mf-0123abcdef4-1",
        "mf-0123abcdef45-",
        "mf-0123abcdef45-+1",
        "mf-0123abcdef45-99999999999",
        "before-upgrade",
    ] {
        assert_eq!(SnapshotName::parse(bad), None, "{bad}");
    }
}

#[test]
fn requests_name_and_describe_the_step() {
    let incident = Uuid::from_u128(0xabc);
    let request = SnapshotRequest { incident, step: 2 };
    assert_eq!(request.name(), SnapshotName::for_step(incident, 2));
    assert!(request.description().contains(&incident.to_string()));
    assert!(request.description().contains("step 2"));
}

#[test]
fn support_needs_the_integration_a_mapping_and_its_cluster() {
    let clusters = vec!["pve".to_owned()];
    let mapped = Some(vm("pve", "node1", 101));
    assert_eq!(
        Support::of(true, &clusters, mapped.clone()),
        Support::Available(vm("pve", "node1", 101))
    );
    assert_eq!(
        Support::of(false, &clusters, mapped).reason(),
        Some(Unsupported::Disabled)
    );
    assert_eq!(
        Support::of(true, &clusters, None).reason(),
        Some(Unsupported::NotMapped)
    );
    let other = Support::of(true, &clusters, Some(vm("lab", "node1", 101)));
    assert_eq!(other.reason(), Some(Unsupported::UnknownCluster));
    assert!(!other.is_available());
    assert_eq!(
        serde_json::to_value(Unsupported::UnknownCluster).unwrap(),
        "unknown_cluster"
    );
}

#[test]
fn retention_keeps_the_newest() {
    let live: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
    assert_eq!(beyond_retention(&live, 3), ["a"]);
    assert_eq!(beyond_retention(&live, 1), ["a", "b", "c"]);
    assert!(beyond_retention(&live, 4).is_empty());
    assert!(beyond_retention(&live, 10).is_empty());
}

#[test]
fn vm_refs_follow_the_table_checks() {
    assert!(vm("pve-1", "node-1", 100).check().is_ok());
    assert!(vm("pve", "n", 999_999_999).check().is_ok());
    assert!(vm("", "node", 100).check().is_err());
    assert!(vm("p ve", "node", 100).check().is_err());
    assert!(vm("pve", "", 100).check().is_err());
    assert!(vm("pve", "-node", 100).check().is_err());
    assert!(vm("pve", "node.lan", 100).check().is_err());
    assert!(vm("pve", &"n".repeat(64), 100).check().is_err());
    assert!(vm("pve", "node", 99).check().is_err());
    assert!(vm("pve", "node", 1_000_000_000).check().is_err());
}

#[test]
fn clients_refuse_plain_http_bad_tokens_and_bad_cas() {
    let secret = Secret::new("0b3f2c4e-1111-2222-3333-444455556666");
    let token = "monitorflow@pve!snapshots";
    let new = |url: &str, token: &str, secret: &Secret, ca: Option<&[u8]>| {
        ProxmoxClient::new(url, token, secret, ca, options())
    };
    assert!(new("https://pve:8006", token, &secret, None).is_ok());
    for url in ["http://pve:8006", "pve:8006", "not a url"] {
        assert!(matches!(
            new(url, token, &secret, None),
            Err(SnapshotError::Config(_))
        ));
    }
    assert!(new("https://pve:8006", "monitorflow@pve", &secret, None).is_err());
    for bad in ["", "two words", "line\nbreak"] {
        assert!(new("https://pve:8006", token, &Secret::new(bad), None).is_err());
    }
    let error = new("https://pve:8006", token, &secret, Some(b"not pem")).unwrap_err();
    assert!(matches!(error, SnapshotError::Config(_)));
}

#[test]
fn the_token_secret_never_shows_in_debug_output() {
    let secret = Secret::new("s3cr3t-value");
    let client = ProxmoxClient::new(
        "https://pve:8006",
        "monitorflow@pve!snapshots",
        &secret,
        None,
        options(),
    )
    .unwrap();
    let shown = format!("{client:?}");
    assert!(!shown.contains("s3cr3t-value"), "{shown}");
}

#[tokio::test]
async fn secret_files_are_trimmed_and_checked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    std::fs::write(&path, "  abc-123\n").unwrap();
    assert_eq!(
        proxmox::read_secret(&path).await.unwrap().expose(),
        "abc-123"
    );
    std::fs::write(&path, "\n").unwrap();
    let error = proxmox::read_secret(&path).await.unwrap_err().to_string();
    assert!(error.contains("empty"), "{error}");
    assert!(proxmox::read_secret(&dir.path().join("missing"))
        .await
        .is_err());
}

#[test]
fn remote_messages_are_bounded_and_single_line() {
    assert_eq!(bounded(" snapshot\nfailed\r"), "snapshot failed");
    let long = "é".repeat(MAX_MESSAGE);
    let cut = bounded(&long);
    assert!(cut.len() <= MAX_MESSAGE && cut.chars().all(|c| c == 'é'));
}

#[tokio::test]
async fn without_a_platform_nothing_is_supported() {
    let none = NoSnapshots;
    let server = Uuid::from_u128(1);
    assert_eq!(
        none.support(server).await.unwrap().reason(),
        Some(Unsupported::Disabled)
    );
    let request = SnapshotRequest {
        incident: server,
        step: 1,
    };
    assert!(matches!(
        none.take(server, request).await,
        Err(SnapshotError::Unsupported(Unsupported::Disabled))
    ));
}
