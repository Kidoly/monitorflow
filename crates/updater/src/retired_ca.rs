//! A restore keeps what the server knows of its retired certificate
//! authorities (ADR-0088): `ca/retired/` holds the certificate of each CA
//! a change of certificate authority replaced. A backup taken before such
//! a change brings the old CA back, and the agents and relays that moved
//! cannot connect to it; keeping the directory lets the server say so, and
//! the restore says it first.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{chown, MetadataExt};
use std::path::Path;

use monitorflow_common::tls::{fingerprint, fingerprint_hex, load_certs};

const CA_DIR: &str = "ca";
const RETIRED_DIR: &str = "ca/retired";
const CA_CERT: &str = "ca/ca-cert.pem";

/// The files of `ca/retired/` in the server data, by name.
#[derive(Debug, Default)]
pub struct Retired(BTreeMap<String, Vec<u8>>);

/// Reads `ca/retired/` before the restore replaces the server data.
pub fn read(serverdata: &Path) -> io::Result<Retired> {
    let dir = serverdata.join(RETIRED_DIR);
    let mut files = BTreeMap::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Retired::default()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && entry.file_type()?.is_file() {
            files.insert(name, fs::read(entry.path())?);
        }
    }
    Ok(Retired(files))
}

/// Puts back the files the restored data lacks, owned like `ca/`, and
/// returns a warning when the restored CA is one of them.
pub fn put_back(serverdata: &Path, retired: &Retired) -> io::Result<Option<String>> {
    let ca_dir = serverdata.join(CA_DIR);
    if retired.0.is_empty() || !ca_dir.is_dir() {
        return Ok(None);
    }
    let owner = fs::metadata(&ca_dir)?;
    let dir = serverdata.join(RETIRED_DIR);
    if !dir.is_dir() {
        fs::create_dir(&dir)?;
        chown(&dir, Some(owner.uid()), Some(owner.gid()))?;
    }
    for (name, bytes) in &retired.0 {
        let path = dir.join(name);
        if !path.exists() {
            fs::write(&path, bytes)?;
            chown(&path, Some(owner.uid()), Some(owner.gid()))?;
        }
    }
    let Ok(certs) = load_certs(&serverdata.join(CA_CERT)) else {
        return Ok(None);
    };
    let restored = format!("{}.json", fingerprint_hex(&fingerprint(&certs[0])));
    Ok(retired.0.contains_key(&restored).then(|| {
        "this backup was taken before the certificate authority changed: the restored server \
         has the old one, and servers and relays that moved to the new one cannot connect. \
         Restore a newer backup, or install each of them again with a new install command"
            .to_owned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the DER bytes count for the fingerprint.
    fn pem(der: &[u8]) -> String {
        let b64 = |bytes: &[u8]| -> String {
            const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            bytes
                .chunks(3)
                .flat_map(|c| {
                    let n = (c[0] as u32) << 16
                        | (*c.get(1).unwrap_or(&0) as u32) << 8
                        | *c.get(2).unwrap_or(&0) as u32;
                    (0..4).map(move |i| match i <= c.len() {
                        true => ABC[(n >> (18 - 6 * i) & 63) as usize] as char,
                        false => '=',
                    })
                })
                .collect()
        };
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            b64(der)
        )
    }

    #[test]
    fn keeps_retired_cas_and_warns_when_the_backup_brings_one_back() {
        let work = tempfile::TempDir::new().unwrap();
        let old_pem = pem(&[1, 2, 3]);
        fs::write(work.path().join("old.pem"), &old_pem).unwrap();
        let old = load_certs(&work.path().join("old.pem")).unwrap();
        let old_name = format!("{}.json", fingerprint_hex(&fingerprint(&old[0])));

        // The current data retired the old CA.
        let data = work.path().join("serverdata");
        fs::create_dir_all(data.join(RETIRED_DIR)).unwrap();
        fs::write(data.join(RETIRED_DIR).join(&old_name), b"{}").unwrap();
        let retired = read(&data).unwrap();

        // The backup holds the old CA, and no retired directory.
        fs::remove_dir_all(&data).unwrap();
        fs::create_dir_all(data.join(CA_DIR)).unwrap();
        fs::write(data.join(CA_CERT), &old_pem).unwrap();
        let warning = put_back(&data, &retired).unwrap();
        assert!(warning
            .unwrap()
            .contains("before the certificate authority changed"));
        assert_eq!(
            fs::read(data.join(RETIRED_DIR).join(&old_name)).unwrap(),
            b"{}"
        );

        // A backup with another CA: kept, without a warning.
        fs::write(data.join(CA_CERT), pem(&[4, 5, 6])).unwrap();
        assert_eq!(put_back(&data, &retired).unwrap(), None);
    }

    #[test]
    fn nothing_retired_nothing_to_do() {
        let work = tempfile::TempDir::new().unwrap();
        assert!(read(work.path()).unwrap().0.is_empty());
        assert_eq!(put_back(work.path(), &Retired::default()).unwrap(), None);
    }
}
