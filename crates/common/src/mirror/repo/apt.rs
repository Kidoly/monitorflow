//! apt metadata (ADR-0034). `dists/<suite>/Release`, covered by the
//! signature in `InRelease` and `Release.gpg`, lists every index file of
//! the suite with its size and SHA-256; each `Packages` index lists the
//! packages under `pool/` the same way.

use std::io::BufRead;
use std::path::Path;

use super::compress::{self, Compression};
use super::meta::{join, Algorithm, Checksum, Entry, MetaError, Package};

/// Signed files of a suite, not listed in `Release` itself, all
/// required.
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

/// The package lists among `entries`: for each directory, every
/// `Packages` variant listed, in the order of [`Compression::ALL`].
/// Publishers list compressions they do not generate, so any of them may
/// be missing upstream.
pub fn package_lists(entries: &[Entry]) -> Vec<Vec<Entry>> {
    let mut groups: Vec<(String, Vec<Entry>)> = Vec::new();
    for compression in Compression::ALL {
        for entry in entries {
            let (dir, name) = entry.path.rsplit_once('/').unwrap_or(("", &entry.path));
            if Compression::of(name) != (compression, "Packages") {
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

/// The packages of the `Packages` index `file`, stored at `path`, in any
/// compression. Blocking.
pub fn read_packages(file: &str, path: &Path) -> Result<Vec<Entry>, MetaError> {
    packages(file, compress::open(file, path)?)
}

/// As [`read_packages`], with each package's name, version and
/// architecture (ADR-0142). Blocking.
pub fn read_package_list(file: &str, path: &Path) -> Result<Vec<Package>, MetaError> {
    package_list(file, compress::open(file, path)?)
}

/// The packages a `Packages` index lists, with paths from the snapshot
/// root. Blocking when `reader` is.
pub fn packages(file: &str, reader: impl BufRead) -> Result<Vec<Entry>, MetaError> {
    Ok(package_list(file, reader)?
        .into_iter()
        .map(|p| p.entry)
        .collect())
}

/// The fields of one stanza that [`package_list`] reads.
#[derive(Default)]
struct Stanza {
    path: Option<String>,
    size: Option<String>,
    sha: Option<String>,
    name: Option<String>,
    version: Option<String>,
    arch: Option<String>,
}

impl Stanza {
    /// The package, `None` for an empty stanza.
    fn finish(&mut self, file: &str) -> Result<Option<Package>, MetaError> {
        let stanza = std::mem::take(self);
        match (stanza.path, stanza.size, stanza.sha) {
            (None, None, None) => Ok(None),
            (Some(p), Some(s), Some(h)) => Ok(Some(Package {
                entry: Entry {
                    path: join(file, "", &p)?,
                    size: s
                        .parse()
                        .map_err(|_| MetaError::invalid(file, format!("bad size `{s}`")))?,
                    checksum: Checksum::new(Algorithm::Sha256, &h)
                        .ok_or_else(|| MetaError::invalid(file, format!("bad SHA256 `{h}`")))?,
                },
                name: stanza.name.unwrap_or_default(),
                version: stanza.version.unwrap_or_default(),
                arch: stanza.arch.unwrap_or_default(),
            })),
            (p, _, _) => Err(MetaError::invalid(
                file,
                format!(
                    "package {} lacks Filename, Size or SHA256",
                    p.unwrap_or_default()
                ),
            )),
        }
    }
}

/// As [`packages`], with each package's `Package`, `Version` and
/// `Architecture`. Blocking when `reader` is.
pub fn package_list(file: &str, reader: impl BufRead) -> Result<Vec<Package>, MetaError> {
    let mut packages = Vec::new();
    let mut stanza = Stanza::default();
    for line in reader.lines() {
        let line = line.map_err(|e| compress::read_error(file, &e))?;
        if line.trim().is_empty() {
            packages.extend(stanza.finish(file)?);
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
            "filename" => stanza.path = value,
            "size" => stanza.size = value,
            "sha256" => stanza.sha = value,
            "package" => stanza.name = value,
            "version" => stanza.version = value,
            "architecture" => stanza.arch = value,
            _ => {}
        }
    }
    packages.extend(stanza.finish(file)?);
    Ok(packages)
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
    fn package_lists_group_every_compression() {
        let listed = [
            "main/binary-amd64/Packages.bz2",
            "main/binary-amd64/Packages.xz",
            "main/binary-amd64/Release",
            "main/binary-arm64/Packages.zst",
            "main/binary-amd64/Packages.diff/Index",
            "main/binary-amd64/Packages.lz4",
            "main/binary-amd64/Packages",
            "main/i18n/Translation-en.xz",
            "contrib/binary-amd64/Packages.gz",
        ];
        let text = listed.iter().fold("SHA256:\n".to_owned(), |text, path| {
            format!("{text} {H1} 1 {path}\n")
        });
        let groups: Vec<Vec<String>> = package_lists(&release("b", &text).unwrap())
            .into_iter()
            .map(|g| g.into_iter().map(|e| e.path).collect())
            .collect();
        assert_eq!(
            groups,
            [
                vec![
                    "dists/b/main/binary-amd64/Packages",
                    "dists/b/main/binary-amd64/Packages.xz",
                    "dists/b/main/binary-amd64/Packages.bz2",
                ],
                vec!["dists/b/contrib/binary-amd64/Packages.gz"],
                vec!["dists/b/main/binary-arm64/Packages.zst"],
            ]
        );
    }

    #[test]
    fn read_packages_decompresses() {
        let text = format!("Filename: pool/a.deb\nSize: 3\nSHA256: {H1}\n");
        let dir = tempfile::tempdir().unwrap();
        for c in Compression::ALL {
            let path = dir.path().join("p");
            std::fs::write(&path, compress::tests::compress(c, text.as_bytes())).unwrap();
            let file = format!("Packages{}", c.extension());
            let entries = read_packages(&file, &path).unwrap();
            assert_eq!(entries.len(), 1, "{c:?}");
            assert_eq!(entries[0].path, "pool/a.deb");
        }
        assert!(read_packages("Packages.xz", &dir.path().join("absent")).is_err());
    }

    #[test]
    fn a_bomb_of_a_list_stops_at_the_limit() {
        let bomb = "Description: x\n".repeat(100_000);
        let raw = compress::tests::compress(Compression::Gzip, bomb.as_bytes());
        let reader = Compression::Gzip.decoder("P.gz", &raw[..], 1000).unwrap();
        assert_eq!(
            packages("P.gz", reader).unwrap_err(),
            MetaError::TooLarge {
                file: "P.gz".into(),
                limit: 1000
            }
        );
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
        let text = text.replace("Version: 1.0\n", "Version: 1:1.0-2\nArchitecture: amd64\n");
        let listed = package_list("P", text.as_bytes()).unwrap();
        let what: Vec<_> = listed
            .iter()
            .map(|p| (p.name.as_str(), p.version.as_str(), p.arch.as_str()))
            .collect();
        assert_eq!(what, [("a", "1:1.0-2", "amd64"), ("b", "", "")]);
        assert_eq!(listed[0].entry, entries[0]);

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
