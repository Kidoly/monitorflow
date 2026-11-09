//! A restore keeps what the server knows of its retired certificate
//! authorities (ADR-0088): `ca/retired/` holds the certificate of each CA
//! a change of certificate authority replaced. A backup taken before such
//! a change brings the old CA back, and the agents and relays that moved
//! cannot connect to it; keeping the directory lets the server say so, and
//! the restore says it first.
//!
//! The server's user owns that data and the restore runs as root: `ca`
//! and `ca/retired` are opened as directories without following links,
//! and files are read and created below those descriptors, by the names
//! the server gives them only. A link planted there, in the live data or
//! in the backup, is never followed out of the server data.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{fchown, MetadataExt};
use std::path::Path;

use monitorflow_common::tls::{fingerprint, fingerprint_hex, load_certs};
use rustix::fs::{mkdirat, openat, Dir, Mode, OFlags};

const CA_DIR: &str = "ca";
const RETIRED_DIR: &str = "retired";
const CA_CERT: &str = "ca-cert.pem";
/// More than the server writes: a certificate and a few fields each, one
/// per change of certificate authority.
const MAX_FILE: u64 = 64 * 1024;
const MAX_FILES: usize = 64;

/// The files of `ca/retired/` in the server data, by name.
#[derive(Debug, Default)]
pub struct Retired(BTreeMap<String, Vec<u8>>);

/// `<fingerprint>.json`, the fingerprint as 64 lower-case hex digits: the
/// only names the server writes in `ca/retired/`.
fn is_retired_name(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(|hex| {
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// `name` in `parent`, when it is a directory and not a link to one.
fn open_dir(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(openat(parent, name, flags, Mode::empty())?)
}

/// `ca/` of the server data.
fn open_ca(serverdata: &Path) -> io::Result<OwnedFd> {
    open_dir(&File::open(serverdata)?.into(), CA_DIR)
}

/// A regular file of `dir` up to [`MAX_FILE`] bytes; `None` for a link,
/// anything else that is not a file, and a larger one.
fn read_file(dir: &OwnedFd, name: &str) -> io::Result<Option<Vec<u8>>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match openat(dir, name, flags, Mode::empty()) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NXIO) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_FILE).then_some(bytes))
}

/// Reads `ca/retired/` before the restore replaces the server data.
pub fn read(serverdata: &Path) -> io::Result<Retired> {
    let dir = match open_ca(serverdata).and_then(|ca| open_dir(&ca, RETIRED_DIR)) {
        Ok(dir) => dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Retired::default()),
        Err(error) => return Err(error),
    };
    let mut files = BTreeMap::new();
    for entry in Dir::read_from(&dir)? {
        let entry = entry?;
        let Some(name) = entry
            .file_name()
            .to_str()
            .ok()
            .filter(|n| is_retired_name(n))
        else {
            continue;
        };
        if files.len() == MAX_FILES {
            break;
        }
        if let Some(bytes) = read_file(&dir, name)? {
            files.insert(name.to_owned(), bytes);
        }
    }
    Ok(Retired(files))
}

/// Puts back the files the restored data lacks, owned like `ca/`, and
/// returns a warning when the restored CA is one of them.
pub fn put_back(serverdata: &Path, retired: &Retired) -> io::Result<Option<String>> {
    if retired.0.is_empty() {
        return Ok(None);
    }
    let Ok(ca) = open_ca(serverdata) else {
        return Ok(None);
    };
    let owner = File::from(ca.try_clone()?).metadata()?;
    let dir = match open_dir(&ca, RETIRED_DIR) {
        Ok(dir) => dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            mkdirat(&ca, RETIRED_DIR, Mode::from_raw_mode(owner.mode() & 0o777))?;
            let dir = open_dir(&ca, RETIRED_DIR)?;
            fchown(&dir, Some(owner.uid()), Some(owner.gid()))?;
            dir
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("{CA_DIR}/{RETIRED_DIR} of the backup is not a directory: {error}"),
            ))
        }
    };
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for (name, bytes) in retired.0.iter().filter(|(name, _)| is_retired_name(name)) {
        // Exclusive: a file of the backup, or a link in its place, stays.
        let mut file = match openat(&dir, name.as_str(), flags, Mode::from_raw_mode(0o644)) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        };
        file.write_all(bytes)?;
        fchown(&file, Some(owner.uid()), Some(owner.gid()))?;
    }
    // The server reads it the same way; a restore must not wait on it.
    let cert = serverdata.join(CA_DIR).join(CA_CERT);
    if !std::fs::symlink_metadata(&cert).is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let Ok(certs) = load_certs(&cert) else {
        return Ok(None);
    };
    let Some(cert) = certs.first() else {
        return Ok(None);
    };
    let restored = format!("{}.json", fingerprint_hex(&fingerprint(cert)));
    Ok(retired.0.contains_key(&restored).then(|| {
        "this backup was taken before the certificate authority changed: the restored server \
         has the old one, and servers and relays that moved to the new one cannot connect. \
         Restore a newer backup, or install each of them again with a new install command"
            .to_owned()
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    const RETIRED: &str = "ca/retired";
    const CERT: &str = "ca/ca-cert.pem";

    fn name(digit: char) -> String {
        format!("{}.json", digit.to_string().repeat(64))
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The audit's chain: the backup holds `ca/retired` as a link to the
    /// updater's state, the live directory a crafted `pending.json`.
    #[test]
    fn a_link_in_the_backup_is_not_followed_into_the_updater_state() {
        let work = tempfile::TempDir::new().unwrap();
        let (data, state) = (work.path().join("serverdata"), work.path().join("state"));
        fs::create_dir_all(data.join(RETIRED)).unwrap();
        fs::create_dir(&state).unwrap();
        fs::write(data.join(RETIRED).join("pending.json"), b"forged").unwrap();
        fs::write(data.join(RETIRED).join(name('a')), b"{}").unwrap();
        let retired = read(&data).unwrap();
        // Only what the server names so is kept at all.
        assert_eq!(retired.0.keys().collect::<Vec<_>>(), [&name('a')]);

        // What tar extracts.
        fs::remove_dir_all(data.join(RETIRED)).unwrap();
        symlink(&state, data.join(RETIRED)).unwrap();
        let error = put_back(&data, &retired).unwrap_err();
        assert!(error.to_string().contains("is not a directory"), "{error}");
        assert_eq!(names(&state), [] as [&str; 0]);

        // `ca` itself a link: nothing to put back into.
        fs::remove_file(data.join(RETIRED)).unwrap();
        fs::remove_dir(data.join("ca")).unwrap();
        fs::create_dir(state.join("retired")).unwrap();
        symlink(&state, data.join("ca")).unwrap();
        assert_eq!(put_back(&data, &retired).unwrap(), None);
        assert_eq!(names(&state.join("retired")), [] as [&str; 0]);
    }

    #[test]
    fn links_and_other_names_in_the_live_data_are_not_read() {
        let work = tempfile::TempDir::new().unwrap();
        let data = work.path().join("serverdata");
        let secret = work.path().join("secret");
        fs::write(&secret, b"secret").unwrap();
        fs::create_dir_all(data.join(RETIRED)).unwrap();
        let dir = data.join(RETIRED);
        symlink(&secret, dir.join(name('a'))).unwrap();
        fs::write(dir.join(name('b')), vec![b' '; MAX_FILE as usize + 1]).unwrap();
        fs::create_dir(dir.join(name('c'))).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            dir.join(name('d')),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        fs::write(dir.join(name('e')), b"{}").unwrap();
        for other in ["pending.json", "A.json", &name('f').to_uppercase(), "x"] {
            fs::write(dir.join(other), b"{}").unwrap();
        }
        let retired = read(&data).unwrap();
        assert_eq!(retired.0.keys().collect::<Vec<_>>(), [&name('e')]);

        // A linked directory is not read at all.
        let elsewhere = work.path().join("elsewhere");
        fs::rename(&dir, &elsewhere).unwrap();
        symlink(&elsewhere, &dir).unwrap();
        assert!(read(&data).is_err());
    }

    #[test]
    fn files_of_the_backup_and_links_in_their_place_are_kept() {
        let work = tempfile::TempDir::new().unwrap();
        let data = work.path().join("serverdata");
        let victim = work.path().join("victim");
        fs::write(&victim, b"keep").unwrap();
        fs::create_dir_all(data.join(RETIRED)).unwrap();
        let dir = data.join(RETIRED);
        for digit in ['a', 'b', 'c', 'd'] {
            fs::write(dir.join(name(digit)), b"live").unwrap();
        }
        let retired = read(&data).unwrap();
        // The backup has one of them, and links where two others go.
        fs::write(dir.join(name('a')), b"backup").unwrap();
        fs::remove_file(dir.join(name('b'))).unwrap();
        symlink(&victim, dir.join(name('b'))).unwrap();
        fs::remove_file(dir.join(name('c'))).unwrap();
        symlink(work.path().join("created"), dir.join(name('c'))).unwrap();
        fs::remove_file(dir.join(name('d'))).unwrap();
        assert_eq!(put_back(&data, &retired).unwrap(), None);
        assert_eq!(fs::read(dir.join(name('a'))).unwrap(), b"backup");
        assert_eq!(fs::read(&victim).unwrap(), b"keep");
        assert!(!work.path().join("created").exists());
        assert_eq!(fs::read(dir.join(name('d'))).unwrap(), b"live");
    }

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
        fs::create_dir_all(data.join(RETIRED)).unwrap();
        fs::write(data.join(RETIRED).join(&old_name), b"{}").unwrap();
        let retired = read(&data).unwrap();

        // The backup holds the old CA, and no retired directory.
        fs::remove_dir_all(&data).unwrap();
        fs::create_dir_all(data.join(CA_DIR)).unwrap();
        fs::write(data.join(CERT), &old_pem).unwrap();
        let warning = put_back(&data, &retired).unwrap();
        assert!(warning
            .unwrap()
            .contains("before the certificate authority changed"));
        assert_eq!(fs::read(data.join(RETIRED).join(&old_name)).unwrap(), b"{}");

        // A backup with another CA: kept, without a warning.
        fs::write(data.join(CERT), pem(&[4, 5, 6])).unwrap();
        assert_eq!(put_back(&data, &retired).unwrap(), None);
    }

    #[test]
    fn nothing_retired_nothing_to_do() {
        let work = tempfile::TempDir::new().unwrap();
        assert!(read(work.path()).unwrap().0.is_empty());
        assert_eq!(put_back(work.path(), &Retired::default()).unwrap(), None);
    }
}
