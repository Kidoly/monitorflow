//! apt metadata (ADR-0034). `dists/<suite>/Release`, covered by the
//! signature in `InRelease` and `Release.gpg`, lists every index file of
//! the suite with its size and SHA-256; each `Packages` index lists the
//! packages under `pool/` the same way.

use std::io::BufRead;

use super::meta::{join, Algorithm, Checksum, Entry, MetaError};

/// Signed files of a suite, not listed in `Release` itself; only
/// `Release` is required.
pub const ROOT_FILES: [&str; 3] = ["Release", "InRelease", "Release.gpg"];

/// `dists/<suite>`.
pub fn suite_dir(suite: &str) -> String {
    format!("dists/{suite}")
}

/// The files `Release` lists, with paths from the snapshot root.
pub fn release(suite: &str, text: &str) -> Result<Vec<Entry>, MetaError> {
    let dir = suite_dir(suite);
    let file = format!("{dir}/Release");
    let mut lines = text.lines().skip_while(|l| !l.starts_with("SHA256:"));
    if lines.next().is_none() {
        return Err(MetaError::invalid(&file, "no SHA256 field"));
    }
    let mut entries = Vec::new();
    for line in lines.take_while(|l| l.starts_with([' ', '\t'])) {
        let mut words = line.split_whitespace();
        let (Some(hex), Some(size), Some(path), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(MetaError::invalid(&file, format!("bad line `{line}`")));
        };
        entries.push(Entry {
            path: join(&file, &dir, path)?,
            size: size
                .parse()
                .map_err(|_| MetaError::invalid(&file, format!("bad size `{size}`")))?,
            checksum: Checksum::new(Algorithm::Sha256, hex)
                .ok_or_else(|| MetaError::invalid(&file, format!("bad SHA256 `{hex}`")))?,
        });
    }
    Ok(entries)
}

/// The package lists among `entries`: for each directory, its `Packages`
/// variants we can read, uncompressed first. One of each group is enough.
pub fn package_lists(entries: &[Entry]) -> Vec<Vec<Entry>> {
    let mut groups: Vec<(String, Vec<Entry>)> = Vec::new();
    for preferred in ["Packages", "Packages.gz"] {
        for entry in entries {
            let (dir, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
            if name != preferred {
                continue;
            }
            match groups.iter_mut().find(|(d, _)| d == dir) {
                Some((_, group)) => group.push(entry.clone()),
                None => groups.push((dir.to_owned(), vec![entry.clone()])),
            }
        }
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

/// The packages a `Packages` index lists, with paths from the snapshot
/// root. Blocking when `reader` is.
pub fn packages(file: &str, reader: impl BufRead) -> Result<Vec<Entry>, MetaError> {
    let mut entries = Vec::new();
    let (mut path, mut size, mut sha) = (None, None, None);
    let mut finish = |path: &mut Option<String>,
                      size: &mut Option<String>,
                      sha: &mut Option<String>|
     -> Result<(), MetaError> {
        match (path.take(), size.take(), sha.take()) {
            (None, None, None) => Ok(()),
            (Some(p), Some(s), Some(h)) => {
                entries.push(Entry {
                    path: join(file, "", &p)?,
                    size: s
                        .parse()
                        .map_err(|_| MetaError::invalid(file, format!("bad size `{s}`")))?,
                    checksum: Checksum::new(Algorithm::Sha256, &h)
                        .ok_or_else(|| MetaError::invalid(file, format!("bad SHA256 `{h}`")))?,
                });
                Ok(())
            }
            (p, _, _) => Err(MetaError::invalid(
                file,
                format!(
                    "package {} lacks Filename, Size or SHA256",
                    p.unwrap_or_default()
                ),
            )),
        }
    };
    for line in reader.lines() {
        let line = line.map_err(|e| MetaError::invalid(file, e.to_string()))?;
        if line.trim().is_empty() {
            finish(&mut path, &mut size, &mut sha)?;
            continue;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(MetaError::invalid(file, format!("bad line `{line}`")));
        };
        let value = Some(value.trim().to_owned());
        match key.to_ascii_lowercase().as_str() {
            "filename" => path = value,
            "size" => size = value,
            "sha256" => sha = value,
            _ => {}
        }
    }
    finish(&mut path, &mut size, &mut sha)?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn release_lists_suite_files() {
        let text = format!(
            "Origin: MonitorFlow\nMD5Sum:\n 00 1 main/binary-amd64/Packages\nSHA256:\n {H1} 120 main/binary-amd64/Packages\n {H2}   80 main/binary-amd64/Packages.gz\nSHA512:\n x 1 y\n"
        );
        let entries = release("bookworm", &text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "dists/bookworm/main/binary-amd64/Packages");
        assert_eq!(entries[1].size, 80);
        assert_eq!(entries[1].checksum.to_string(), format!("sha256:{H2}"));
        let groups = package_lists(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0][0].path, entries[0].path);
        assert_eq!(groups[0][1].path, entries[1].path);

        for bad in [
            "Origin: x\n".to_owned(),
            format!("SHA256:\n {H1} 1\n"),
            format!("SHA256:\n {H1} x a\n"),
            "SHA256:\n 12 1 a\n".to_owned(),
            format!("SHA256:\n {H1} 1 ../../x\n"),
        ] {
            assert!(release("bookworm", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn packages_lists_pool_files() {
        let text = format!(
            "Package: a\nVersion: 1.0\nFilename: pool/main/a/a_1.0_amd64.deb\nSize: 3\nSHA256: {H1}\nDescription: x\n more\n\n\nPackage: b\nfilename: pool/main/b/b_2_all.deb\nsize: 10\nsha256: {H2}\n"
        );
        let entries = packages("P", text.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "pool/main/a/a_1.0_amd64.deb");
        assert_eq!(entries[0].size, 3);
        assert_eq!(entries[1].path, "pool/main/b/b_2_all.deb");
        assert!(packages("P", &b""[..]).unwrap().is_empty());

        for bad in [
            "Package: a\nFilename: pool/a.deb\nSize: 3\n".to_owned(),
            format!("Filename: /etc/shadow\nSize: 3\nSHA256: {H1}\n"),
            format!("Filename: pool/a.deb\nSize: -1\nSHA256: {H1}\n"),
            "no colon\n".to_owned(),
        ] {
            assert!(packages("P", bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
