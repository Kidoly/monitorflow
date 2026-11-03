//! What identifies the machine to its hypervisor, sent in `Hello` so the
//! server can find a server's Proxmox VM or container (ADR-0093): the
//! SMBIOS system UUID, which Proxmox sets from a VM's `smbios1`, and the
//! MAC addresses, which Proxmox sets on each VM or container network
//! device.
//!
//! On Linux the kernel keeps the UUID readable by root only. The systemd
//! unit copies it into the agent's runtime directory before starting it;
//! the agent also reads it directly when it runs as root. Inside a
//! container the agent reports no UUID: a privileged container would read
//! its host's. On Windows the UUID comes from the registry, where Windows
//! records the SMBIOS UUID of the hardware it booted on; MAC addresses are
//! not read there, as a Proxmox VM is found by its UUID.

use std::path::Path;

/// MAC addresses sent at most.
const MAX_MACS: usize = 32;

/// UUIDs firmware uses when it has none of its own.
const PLACEHOLDER_UUIDS: [&str; 3] = [
    "00000000-0000-0000-0000-000000000000",
    "ffffffff-ffff-ffff-ffff-ffffffffffff",
    "03000200-0400-0500-0006-000700080009",
];

/// The hardware identity of the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hardware {
    /// Lowercase, hyphenated.
    pub system_uuid: Option<String>,
    /// Lowercase `aa:bb:cc:dd:ee:ff`, sorted, without duplicates.
    pub mac_addresses: Vec<String>,
}

/// Reads the host's identity; what cannot be read is left out.
#[cfg(unix)]
pub fn detect() -> Hardware {
    detect_in(Path::new("/"))
}

/// The registry's `SYSTEM\HardwareConfig\LastConfig`, `{uuid}`.
#[cfg(windows)]
pub fn detect() -> Hardware {
    let uuid = windows_registry::LOCAL_MACHINE
        .open(r"SYSTEM\HardwareConfig")
        .and_then(|key| key.get_string("LastConfig"))
        .ok();
    Hardware {
        system_uuid: uuid.as_deref().and_then(system_uuid),
        mac_addresses: Vec::new(),
    }
}

/// [`detect`] on Linux with every path under `root`, for tests.
pub fn detect_in(root: &Path) -> Hardware {
    let read = |path: &str| std::fs::read_to_string(root.join(path)).ok();
    let in_container = ["run/systemd/container", "run/host/container-manager"]
        .iter()
        .any(|path| root.join(path).exists());
    let system_uuid = if in_container {
        None
    } else {
        [
            "run/monitorflow-agent/product_uuid",
            "sys/class/dmi/id/product_uuid",
        ]
        .iter()
        .find_map(|path| read(path).as_deref().and_then(system_uuid))
    };
    let mut mac_addresses: Vec<String> = std::fs::read_dir(root.join("sys/class/net"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name() != "lo")
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("address")).ok())
        .filter_map(|text| mac_address(&text))
        .collect();
    mac_addresses.sort();
    mac_addresses.dedup();
    mac_addresses.truncate(MAX_MACS);
    Hardware {
        system_uuid,
        mac_addresses,
    }
}

/// `text` as a lowercase hyphenated UUID, braces and whitespace removed,
/// unless it is a placeholder.
pub fn system_uuid(text: &str) -> Option<String> {
    let uuid = text
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .to_ascii_lowercase();
    let shaped = uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    (shaped && !PLACEHOLDER_UUIDS.contains(&uuid.as_str())).then_some(uuid)
}

/// `text` as a lowercase `aa:bb:cc:dd:ee:ff`, unless it is not an Ethernet
/// address or is all zeros.
pub fn mac_address(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let parts: Vec<&str> = text.split([':', '-']).collect();
    let valid = parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()));
    (valid && parts.iter().any(|p| *p != "00")).then(|| parts.join(":"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn uuids_are_normalized_and_placeholders_dropped() {
        assert_eq!(
            system_uuid("{4C4C4544-0042-3510-8048-B4C04F4E4B32}\n").as_deref(),
            Some("4c4c4544-0042-3510-8048-b4c04f4e4b32")
        );
        for bad in [
            "",
            "Not Settable",
            "00000000-0000-0000-0000-000000000000",
            "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
            "03000200-0400-0500-0006-000700080009",
        ] {
            assert_eq!(system_uuid(bad), None, "{bad}");
        }
    }

    #[test]
    fn macs_are_normalized_and_empty_ones_dropped() {
        assert_eq!(
            mac_address("BC:24:11:0A:0B:0C\n").as_deref(),
            Some("bc:24:11:0a:0b:0c")
        );
        assert_eq!(
            mac_address("bc-24-11-0a-0b-0c").as_deref(),
            Some("bc:24:11:0a:0b:0c")
        );
        for bad in [
            "00:00:00:00:00:00",
            "",
            "bc:24:11:0a:0b",
            "zz:24:11:0a:0b:0c",
        ] {
            assert_eq!(mac_address(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_vm_reports_its_uuid_and_interfaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "sys/class/dmi/id/product_uuid",
            "1B3E2F80-6C0A-4E1B-9C3D-2A4B5C6D7E8F\n",
        );
        write(root, "sys/class/net/lo/address", "00:00:00:00:00:00\n");
        write(root, "sys/class/net/ens18/address", "BC:24:11:AA:BB:01\n");
        write(root, "sys/class/net/docker0/address", "02:42:ac:11:00:01\n");
        write(root, "sys/class/net/bond0/address", "bc:24:11:aa:bb:01\n");
        let found = detect_in(root);
        assert_eq!(
            found.system_uuid.as_deref(),
            Some("1b3e2f80-6c0a-4e1b-9c3d-2a4b5c6d7e8f")
        );
        assert_eq!(
            found.mac_addresses,
            ["02:42:ac:11:00:01", "bc:24:11:aa:bb:01"]
        );
        // The copy the unit makes comes first.
        write(
            root,
            "run/monitorflow-agent/product_uuid",
            "2B3E2F80-6C0A-4E1B-9C3D-2A4B5C6D7E8F",
        );
        assert_eq!(
            detect_in(root).system_uuid.as_deref(),
            Some("2b3e2f80-6c0a-4e1b-9c3d-2a4b5c6d7e8f")
        );
    }

    #[test]
    fn a_container_reports_no_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "sys/class/dmi/id/product_uuid",
            "1B3E2F80-6C0A-4E1B-9C3D-2A4B5C6D7E8F\n",
        );
        write(root, "run/systemd/container", "lxc\n");
        write(root, "sys/class/net/eth0/address", "bc:24:11:aa:bb:02\n");
        let found = detect_in(root);
        assert_eq!(found.system_uuid, None);
        assert_eq!(found.mac_addresses, ["bc:24:11:aa:bb:02"]);
        assert_eq!(detect_in(&root.join("missing")), Hardware::default());
    }
}
