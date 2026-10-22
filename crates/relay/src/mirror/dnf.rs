//! dnf metadata (ADR-0034). `repodata/repomd.xml`, signed in
//! `repomd.xml.asc`, lists the metadata files with their size and
//! checksum; the `primary` one lists every package the same way.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::meta::{join, Algorithm, Checksum, Entry, MetaError};

pub const REPOMD: &str = "repodata/repomd.xml";
/// Signed files of a repository, not listed in `repomd.xml` itself; only
/// `repomd.xml` is required.
pub const ROOT_FILES: [&str; 2] = [REPOMD, "repodata/repomd.xml.asc"];

/// A metadata file of `repomd.xml` and its `type` (`primary`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub kind: String,
    pub entry: Entry,
}

/// The files `repomd.xml` lists. Blocking when `reader` is.
pub fn repomd(reader: impl BufRead) -> Result<Vec<Metadata>, MetaError> {
    let records = records(REPOMD, reader, b"data", Size::Element)?;
    Ok(records
        .into_iter()
        .map(|(kind, entry)| Metadata { kind, entry })
        .collect())
}

/// The packages the `primary` metadata at `path` lists, decompressed by
/// its extension (`.xml`, `.gz`, `.zst`). Blocking.
pub fn primary(file: &str, path: &Path) -> Result<Vec<Entry>, MetaError> {
    let io = |e: io::Error| MetaError::invalid(file, e.to_string());
    let raw = File::open(path).map_err(io)?;
    let reader: Box<dyn BufRead> = if file.ends_with(".gz") {
        Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(raw)))
    } else if file.ends_with(".zst") {
        let decoder = ruzstd::decoding::StreamingDecoder::new(raw)
            .map_err(|e| MetaError::invalid(file, e.to_string()))?;
        Box::new(BufReader::new(decoder))
    } else if file.ends_with(".xml") {
        Box::new(BufReader::new(raw))
    } else {
        return Err(MetaError::invalid(file, "unsupported compression"));
    };
    let records = records(file, reader, b"package", Size::Attribute)?;
    Ok(records.into_iter().map(|(_, entry)| entry).collect())
}

/// Where a record keeps its size: `<size>N</size>` in `repomd.xml`,
/// `<size package="N"/>` in `primary`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    Element,
    Attribute,
}

#[derive(Default)]
struct Record {
    kind: String,
    algorithm: Option<String>,
    checksum: Option<String>,
    size: Option<String>,
    location: Option<String>,
}

/// The `(type, entry)` of every `<tag>` element in `reader`: its direct
/// `checksum`, `size` and `location` children.
fn records(
    file: &str,
    reader: impl BufRead,
    tag: &[u8],
    size_in: Size,
) -> Result<Vec<(String, Entry)>, MetaError> {
    let fail = |reason: String| MetaError::invalid(file, reason);
    let attr = |e: &BytesStart, name: &[u8]| -> Result<Option<String>, MetaError> {
        match e.try_get_attribute(name).map_err(|e| fail(e.to_string()))? {
            Some(a) => Ok(Some(
                a.unescape_value()
                    .map_err(|e| fail(e.to_string()))?
                    .into_owned(),
            )),
            None => Ok(None),
        }
    };
    let mut reader = Reader::from_reader(reader);
    let mut buf = Vec::new();
    let mut entries = Vec::new();
    let mut record: Option<Record> = None;
    // Depth below the record element, and the child whose text we read.
    let mut depth = 0usize;
    let mut text_of: Option<&'static str> = None;
    let mut text = String::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| fail(e.to_string()))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.name();
                if record.is_none() {
                    if name.as_ref() == tag && !empty {
                        record = Some(Record {
                            kind: attr(e, b"type")?.unwrap_or_default(),
                            ..Record::default()
                        });
                        depth = 0;
                    }
                } else if let Some(r) = record.as_mut() {
                    if depth == 0 {
                        match name.as_ref() {
                            b"checksum" => {
                                r.algorithm = attr(e, b"type")?;
                                text_of = (!empty).then_some("checksum");
                            }
                            b"size" if size_in == Size::Attribute => {
                                r.size = attr(e, b"package")?;
                            }
                            b"size" => text_of = (!empty).then_some("size"),
                            b"location" => {
                                if attr(e, b"xml:base")?.is_some() {
                                    return Err(fail(
                                        "xml:base locations are not supported".into(),
                                    ));
                                }
                                r.location = attr(e, b"href")?;
                            }
                            _ => {}
                        }
                    }
                    if !empty {
                        depth += 1;
                    }
                }
                text.clear();
            }
            Event::Text(ref t) if text_of.is_some() => {
                text.push_str(&t.decode().map_err(|e| fail(e.to_string()))?);
            }
            Event::End(_) => {
                let Some(r) = record.as_mut() else {
                    continue;
                };
                if depth == 0 {
                    let r = record.take().unwrap_or_default();
                    entries.push(entry(file, r)?);
                    continue;
                }
                depth -= 1;
                match text_of.take() {
                    Some("checksum") => r.checksum = Some(text.trim().to_owned()),
                    Some("size") => r.size = Some(text.trim().to_owned()),
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if record.is_some() {
        return Err(fail("truncated document".into()));
    }
    Ok(entries)
}

fn entry(file: &str, r: Record) -> Result<(String, Entry), MetaError> {
    let fail = |what: &str| MetaError::invalid(file, format!("{what} of `{}`", r.kind));
    let location = r.location.as_deref().ok_or_else(|| fail("no location"))?;
    let algorithm = r
        .algorithm
        .as_deref()
        .and_then(Algorithm::from_name)
        .ok_or_else(|| fail("unsupported checksum type"))?;
    let checksum = r
        .checksum
        .as_deref()
        .and_then(|hex| Checksum::new(algorithm, hex))
        .ok_or_else(|| fail("bad checksum"))?;
    let size = r
        .size
        .as_deref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| fail("bad size"))?;
    let entry = Entry {
        path: join(file, "", location)?,
        size,
        checksum,
    };
    Ok((r.kind, entry))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn repomd_xml(location: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<repomd xmlns="http://linux.duke.edu/metadata/repo" xmlns:rpm="http://linux.duke.edu/metadata/rpm">
  <revision>1</revision>
  <data type="primary">
    <checksum type="sha256">{H1}</checksum>
    <open-checksum type="sha256">{H2}</open-checksum>
    <location href="{location}"/>
    <timestamp>1</timestamp>
    <size>120</size>
    <open-size>999</open-size>
  </data>
  <data type="filelists">
    <checksum type="sha256">{H2}</checksum>
    <location href="repodata/f-filelists.xml.zst"/>
    <size>7</size>
  </data>
</repomd>"#
        )
    }

    #[test]
    fn repomd_lists_metadata_files() {
        let files = repomd(repomd_xml("repodata/p-primary.xml.gz").as_bytes()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].kind, "primary");
        assert_eq!(files[0].entry.path, "repodata/p-primary.xml.gz");
        assert_eq!(files[0].entry.size, 120);
        assert_eq!(files[0].entry.checksum.to_string(), format!("sha256:{H1}"));
        assert_eq!(files[1].kind, "filelists");
        assert_eq!(files[1].entry.size, 7);

        assert!(repomd(repomd_xml("../x").as_bytes()).is_err());
        assert!(repomd(
            repomd_xml("repodata/a.xml")
                .replace("sha256", "md5")
                .as_bytes()
        )
        .is_err());
        assert!(repomd(&b"<repomd><data type=\"x\"><size>1</size>"[..]).is_err());
    }

    fn primary_xml() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="2">
<package type="rpm">
  <name>a</name>
  <checksum type="sha256" pkgid="YES">{H1}</checksum>
  <size package="3" installed="10" archive="12"/>
  <location href="Packages/a/a-1-1.x86_64.rpm"/>
  <format><rpm:license>MIT</rpm:license><rpm:header-range start="1" end="2"/><file>/usr/bin/a</file></format>
</package>
<package type="rpm">
  <checksum type="sha512" pkgid="YES">{H1}{H1}</checksum>
  <size package="5"/>
  <location href="Packages/b/b&amp;c-2-1.noarch.rpm"/>
</package>
</metadata>"#
        )
    }

    #[test]
    fn primary_lists_packages_in_every_compression() {
        let dir = tempfile::tempdir().unwrap();
        let xml = primary_xml();
        let plain = dir.path().join("p.xml");
        std::fs::write(&plain, &xml).unwrap();
        let gz = dir.path().join("p.xml.gz");
        let mut encoder =
            flate2::write::GzEncoder::new(File::create(&gz).unwrap(), Default::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        encoder.finish().unwrap();
        let zst = dir.path().join("p.xml.zst");
        let compressed = ruzstd::encoding::compress_to_vec(
            xml.as_bytes(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        std::fs::write(&zst, compressed).unwrap();

        let expected = primary("p.xml", &plain).unwrap();
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].path, "Packages/a/a-1-1.x86_64.rpm");
        assert_eq!(expected[0].size, 3);
        assert_eq!(expected[1].path, "Packages/b/b&c-2-1.noarch.rpm");
        assert_eq!(expected[1].checksum.algorithm, Algorithm::Sha512);
        assert_eq!(primary("p.xml.gz", &gz).unwrap(), expected);
        assert_eq!(primary("p.xml.zst", &zst).unwrap(), expected);
        assert!(primary("p.xml.bz2", &plain).is_err());
        assert!(primary("p.xml.zst", &plain).is_err(), "not zstd");

        let based = xml.replace(
            r#"<location href="Packages/a"#,
            r#"<location xml:base="http://elsewhere/" href="Packages/a"#,
        );
        std::fs::write(&plain, based).unwrap();
        assert!(primary("p.xml", &plain).is_err());
    }
}
