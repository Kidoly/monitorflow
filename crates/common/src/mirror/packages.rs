//! The package list of an on-demand snapshot (ADR-0142): every package
//! its signed indexes name, with the path, size and checksum a download
//! must match, and the name, version and architecture prefetching looks
//! up. It is the only thing the listener fetches for: a path it does not
//! name is not fetched.
//!
//! Stored as `.packages` in the snapshot's directory, hidden from the
//! listener, one package per line sorted by path:
//! `path TAB size TAB checksum TAB name TAB version TAB arch`.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use super::repo::meta::{is_safe_path, Checksum, Entry, Package};

/// File name of the list in a snapshot's directory.
pub const PACKAGES: &str = ".packages";

/// First line of the file: its format, so a later one can change it.
const HEADER: &str = "# monitorflow package list 1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ListError {
    #[error("{path} is listed twice with different sizes or checksums")]
    Conflict { path: String },
    #[error("package `{0}` has a tab or a line break in a field")]
    Field(String),
    #[error("not a package list: {0}")]
    Format(String),
}

/// The packages of one snapshot, sorted by path, each path once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageList {
    packages: Vec<Package>,
}

/// One field of a line: no separator inside.
fn plain(field: &str) -> bool {
    !field.contains(['\t', '\n', '\r'])
}

impl PackageList {
    /// The list of `packages`, which may come from several indexes and
    /// name a path more than once: the same package twice is kept once, a
    /// path with two different files is refused.
    pub fn new(mut packages: Vec<Package>) -> Result<Self, ListError> {
        if let Some(bad) = packages.iter().find(|p| {
            ![&p.entry.path, &p.name, &p.version, &p.arch]
                .iter()
                .all(|f| plain(f))
        }) {
            return Err(ListError::Field(bad.entry.path.clone()));
        }
        packages.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
        let mut unique: Vec<Package> = Vec::with_capacity(packages.len());
        for package in packages {
            match unique.last() {
                Some(last) if last.entry.path == package.entry.path => {
                    if last.entry != package.entry {
                        return Err(ListError::Conflict {
                            path: package.entry.path,
                        });
                    }
                }
                _ => unique.push(package),
            }
        }
        Ok(Self { packages: unique })
    }

    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter()
    }

    /// The package at `path`, relative to the snapshot's root.
    pub fn get(&self, path: &str) -> Option<&Package> {
        self.packages
            .binary_search_by(|p| p.entry.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.packages[i])
    }

    /// The packages by `(name, version, arch)`, for prefetching what agents
    /// report as pending. Packages whose index says none of these are left
    /// out.
    pub fn by_version(&self) -> HashMap<(&str, &str, &str), &Package> {
        self.packages
            .iter()
            .filter(|p| !p.name.is_empty() && !p.version.is_empty())
            .map(|p| ((p.name.as_str(), p.version.as_str(), p.arch.as_str()), p))
            .collect()
    }

    /// Total size of the packages listed.
    pub fn bytes(&self) -> u64 {
        self.packages
            .iter()
            .fold(0u64, |total, p| total.saturating_add(p.entry.size))
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut text = String::with_capacity(64 + self.packages.len() * 160);
        text.push_str(HEADER);
        text.push('\n');
        for p in &self.packages {
            let e = &p.entry;
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                e.path, e.size, e.checksum, p.name, p.version, p.arch
            ));
        }
        text
    }

    /// Reads the file's text, checking every path and checksum: the file
    /// is ours, but on a relay it was built from what the upstream sent.
    pub fn parse(text: &str) -> Result<Self, ListError> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(ListError::Format("unknown header".into()));
        }
        let mut packages = Vec::new();
        for (n, line) in lines.enumerate() {
            let bad = || ListError::Format(format!("line {}", n + 2));
            let fields: Vec<&str> = line.split('\t').collect();
            let [path, size, checksum, name, version, arch] = fields[..] else {
                return Err(bad());
            };
            if !is_safe_path(path) {
                return Err(bad());
            }
            packages.push(Package {
                entry: Entry {
                    path: path.to_owned(),
                    size: size.parse().map_err(|_| bad())?,
                    checksum: checksum.parse::<Checksum>().map_err(|_| bad())?,
                },
                name: name.to_owned(),
                version: version.to_owned(),
                arch: arch.to_owned(),
            });
        }
        Self::new(packages)
    }

    /// Writes the list to `path` atomically. Blocking.
    #[cfg(unix)]
    pub fn write(&self, path: &Path) -> io::Result<()> {
        super::write_atomically(path, self.to_text().as_bytes())
    }

    /// The list at `path`; `None` when there is none. Blocking.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::repo::meta::Algorithm;

    fn package(path: &str, hex: char, name: &str, version: &str) -> Package {
        Package {
            entry: Entry {
                path: path.into(),
                size: 3,
                checksum: Checksum::new(Algorithm::Sha256, &hex.to_string().repeat(64)).unwrap(),
            },
            name: name.into(),
            version: version.into(),
            arch: "amd64".into(),
        }
    }

    #[test]
    fn a_list_is_sorted_looked_up_and_round_trips() {
        let list = PackageList::new(vec![
            package("pool/main/b/b_2_amd64.deb", 'b', "b", "2"),
            package("pool/main/a/a_1_amd64.deb", 'a', "a", "1:1.0-1"),
            // The same package listed by two indexes.
            package("pool/main/b/b_2_amd64.deb", 'b', "b", "2"),
        ])
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.bytes(), 6);
        assert_eq!(
            list.get("pool/main/b/b_2_amd64.deb")
                .map(|p| p.name.as_str()),
            Some("b")
        );
        assert!(list.get("pool/main/c.deb").is_none());
        assert_eq!(
            list.by_version()[&("a", "1:1.0-1", "amd64")].entry.path,
            "pool/main/a/a_1_amd64.deb"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PACKAGES);
        assert_eq!(PackageList::read(&path).unwrap(), None);
        list.write(&path).unwrap();
        assert_eq!(PackageList::read(&path).unwrap(), Some(list.clone()));
        assert!(list.to_text().starts_with(HEADER));
    }

    #[test]
    fn conflicts_and_bad_files_are_refused() {
        assert_eq!(
            PackageList::new(vec![
                package("pool/a.deb", 'a', "a", "1"),
                package("pool/a.deb", 'b', "a", "1"),
            ]),
            Err(ListError::Conflict {
                path: "pool/a.deb".into()
            })
        );
        assert!(matches!(
            PackageList::new(vec![package("pool/a.deb", 'a', "a\tb", "1")]),
            Err(ListError::Field(_))
        ));
        let good = PackageList::new(vec![package("pool/a.deb", 'a', "a", "1")])
            .unwrap()
            .to_text();
        for bad in [
            String::new(),
            good.replace(HEADER, "# other"),
            good.replace("pool/a.deb", "../a.deb"),
            good.replace("pool/a.deb", ".hidden"),
            good.replace("\t3\t", "\tx\t"),
            good.replace("sha256:", "md5:"),
            good.replace("\tamd64", ""),
        ] {
            assert!(PackageList::parse(&bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PACKAGES);
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(
            PackageList::read(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
