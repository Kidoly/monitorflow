//! dnf metadata (ADR-0034). `repodata/repomd.xml`, signed in
//! `repomd.xml.asc`, lists the metadata files with their size and
//! checksum; the `primary` one lists every package the same way.

use std::io::BufRead;
use std::path::Path;

use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::compress::{self, Compression};
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
    let records = records(REPOMD, reader, "data", Size::Element)?;
    Ok(records
        .into_iter()
        .map(|(kind, entry)| Metadata { kind, entry })
        .collect())
}

/// The packages the `primary` metadata at `path` lists, decompressed by
/// its extension (`.xml`, `.gz`, `.xz`, `.bz2`, `.zst`). Blocking.
pub fn primary(file: &str, path: &Path) -> Result<Vec<Entry>, MetaError> {
    if !Compression::of(file).1.ends_with(".xml") {
        return Err(MetaError::invalid(file, "unsupported compression"));
    }
    let records = records(
        file,
        compress::open(file, path)?,
        "package",
        Size::Attribute,
    )?;
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
    tag: &str,
    size_in: Size,
) -> Result<Vec<(String, Entry)>, MetaError> {
    let fail = |reason: String| MetaError::invalid(file, reason);
    let attr = |e: &BytesStart, name: &str| -> Result<Option<String>, MetaError> {
        match e.try_get_attribute(name).map_err(|e| fail(e.to_string()))? {
            // Not `normalized_value()`: a path keeps its whitespace as
            // written, and only the five predefined entities and character
            // references are resolved.
            Some(a) => Ok(Some(
                unescape(&a.value)
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
                            kind: attr(e, "type")?.unwrap_or_default(),
                            ..Record::default()
                        });
                        depth = 0;
                    }
                } else if let Some(r) = record.as_mut() {
                    if depth == 0 {
                        match name.as_ref() {
                            "checksum" => {
                                r.algorithm = attr(e, "type")?;
                                text_of = (!empty).then_some("checksum");
                            }
                            "size" if size_in == Size::Attribute => {
                                r.size = attr(e, "package")?;
                            }
                            "size" => text_of = (!empty).then_some("size"),
                            "location" => {
                                if attr(e, "xml:base")?.is_some() {
                                    return Err(fail(
                                        "xml:base locations are not supported".into(),
                                    ));
                                }
                                r.location = attr(e, "href")?;
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
                text.push_str(t);
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

        let expected = primary("p.xml", &plain).unwrap();
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].path, "Packages/a/a-1-1.x86_64.rpm");
        assert_eq!(expected[0].size, 3);
        assert_eq!(expected[1].path, "Packages/b/b&c-2-1.noarch.rpm");
        assert_eq!(expected[1].checksum.algorithm, Algorithm::Sha512);
        for c in Compression::ALL {
            let file = format!("p.xml{}", c.extension());
            let path = dir.path().join(&file);
            std::fs::write(&path, compress::tests::compress(c, xml.as_bytes())).unwrap();
            assert_eq!(primary(&file, &path).unwrap(), expected, "{c:?}");
        }
        assert!(primary("p.xml.lz4", &plain).is_err());
        assert!(primary("p.sqlite.xz", &dir.path().join("p.xml.xz")).is_err());
        assert!(primary("p.xml.zst", &plain).is_err(), "not zstd");
        assert!(primary("p.xml.xz", &plain).is_err(), "not xz");

        let based = xml.replace(
            r#"<location href="Packages/a"#,
            r#"<location xml:base="http://elsewhere/" href="Packages/a"#,
        );
        std::fs::write(&plain, based).unwrap();
        assert!(primary("p.xml", &plain).is_err());
    }

    /// The path of the `primary` record of `repomd_xml(href)`.
    fn location(href: &str) -> Result<String, MetaError> {
        let files = repomd(repomd_xml(href).as_bytes())?;
        Ok(files[0].entry.path.clone())
    }

    /// `repomd_xml` with its first `<size>` element replaced.
    fn size_element(size: &str) -> Result<u64, MetaError> {
        let xml = repomd_xml("repodata/a.xml").replace("<size>120</size>", size);
        Ok(repomd(xml.as_bytes())?[0].entry.size)
    }

    #[test]
    fn attribute_references_resolve_once_and_only_predefined_ones() {
        for (href, expected) in [
            ("a&amp;b", "a&b"),
            ("a&lt;b&gt;", "a<b>"),
            ("a&quot;b&apos;", "a\"b'"),
            ("&#97;&#x62;", "ab"),
            ("a&amp;amp;b", "a&amp;b"),
            ("a&amp;#46;b", "a&#46;b"),
        ] {
            assert_eq!(location(href).unwrap(), expected, "{href}");
        }
        for href in [
            "&unknown;",
            "&nbsp;",
            "a&b",
            "a&amp",
            "&#0;",
            "&#xD800;",
            "&#x110000;",
            "&#;",
            "&#-1;",
        ] {
            assert!(location(href).is_err(), "{href}");
        }
    }

    #[test]
    fn attribute_references_do_not_get_past_the_path_check() {
        for href in [
            "&#46;&#46;/x",
            "repodata/&#x2e;&#x2e;/x",
            "a&#47;&#46;&#46;/x",
            "&#47;etc/passwd",
            "a&#92;b",
            "&amp;#0;/&#46;x",
        ] {
            assert!(location(href).is_err(), "{href}");
        }
    }

    /// `Attribute::normalized_value` would turn these into spaces: the file
    /// the relay asks the mirror for must be the one the metadata names.
    #[test]
    fn attribute_whitespace_is_kept() {
        for (href, expected) in [
            ("a b", "a b"),
            ("a\tb", "a\tb"),
            ("a\nb", "a\nb"),
            ("a\r\nb", "a\r\nb"),
            ("a&#10;b", "a\nb"),
            ("a&#13;b", "a\rb"),
        ] {
            assert_eq!(location(href).unwrap(), expected, "{href:?}");
        }
    }

    #[test]
    fn duplicate_attributes_keep_the_first_and_xml_base_is_refused_anywhere() {
        let xml = repomd_xml("a").replace(r#"href="a""#, r#"href="a" href="../b""#);
        assert_eq!(repomd(xml.as_bytes()).unwrap()[0].entry.path, "a");
        for attributes in [
            r#"href="a" xml:base="""#,
            r#"xml:base="&#46;" href="a""#,
            r#"href="a" xml:base="x" xml:base="y""#,
        ] {
            let xml = repomd_xml("a").replace(r#"href="a""#, attributes);
            assert!(repomd(xml.as_bytes()).is_err(), "{attributes}");
        }
    }

    /// References and CDATA in element text are skipped, not resolved, so
    /// they never produce a checksum or a size the raw text does not hold.
    #[test]
    fn element_text_resolves_nothing() {
        assert_eq!(size_element("<size>\r\n 7\r\n</size>").unwrap(), 7);
        assert_eq!(size_element("<size>1&#50;0</size>").unwrap(), 10);
        assert_eq!(size_element("<size>1&unknown;0</size>").unwrap(), 10);
        assert!(size_element("<size>&#49;</size>").is_err());
        assert!(size_element("<size><![CDATA[120]]></size>").is_err());
        assert!(size_element("<size>1<![CDATA[2]]>x</size>").is_err());

        let referenced = repomd_xml("a").replacen(H1, &format!("&#49;{}", &H1[1..]), 1);
        assert!(repomd(referenced.as_bytes()).is_err());
    }

    #[test]
    fn a_document_type_declares_nothing() {
        let declared = |xml: String| {
            xml.replacen(
                "<repomd ",
                r#"<!DOCTYPE repomd [<!ENTITY e "repodata/a.xml">]><repomd "#,
                1,
            )
        };
        assert!(repomd(declared(repomd_xml("&e;")).as_bytes()).is_err());

        let in_text = repomd_xml("a").replace("<size>120</size>", "<size>&e;7</size>");
        assert_eq!(
            repomd(declared(in_text).as_bytes()).unwrap()[0].entry.size,
            7
        );

        let hidden = format!(
            r#"<!DOCTYPE repomd [<!ENTITY e "]>"><data type="x"></data>]>{}"#,
            repomd_xml("a").replace(r#"<?xml version="1.0" encoding="UTF-8"?>"#, "")
        );
        assert_eq!(repomd(hidden.as_bytes()).unwrap().len(), 2);
    }

    /// quick-xml validates every event since 0.42, including those of
    /// elements we skip, so a bad byte anywhere refuses the whole document.
    #[test]
    fn input_must_be_utf8_whatever_it_declares() {
        let with_byte = |xml: String, at: &str| {
            let (head, tail) = xml.split_once(at).unwrap();
            [head.as_bytes(), at.as_bytes(), b"\xe9", tail.as_bytes()].concat()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.xml");
        std::fs::write(&path, with_byte(primary_xml(), "<name>a")).unwrap();
        assert!(primary("p.xml", &path).is_err());

        assert!(repomd(&with_byte(repomd_xml("a"), r#"href="a"#)[..]).is_err());

        let utf16: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(repomd_xml("a").encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert!(repomd(&utf16[..]).is_err());

        let declared = repomd_xml("a").replace("UTF-8", "ISO-8859-1");
        assert_eq!(repomd(declared.as_bytes()).unwrap().len(), 2);
        let bom = format!("\u{feff}{}", repomd_xml("a"));
        assert_eq!(repomd(bom.as_bytes()).unwrap().len(), 2);
    }

    #[test]
    fn multibyte_characters_survive_any_read_size() {
        let xml = primary_xml().replace("<name>a</name>", "<name>caf\u{e9} \u{1f600}</name>");
        let xml = xml.replace("Packages/a/a-1-1", "Packages/\u{e9}/\u{e9}-1-1");
        let reader = std::io::BufReader::with_capacity(1, xml.as_bytes());
        let records = records("p.xml", reader, "package", Size::Attribute).unwrap();
        assert_eq!(records[0].1.path, "Packages/\u{e9}/\u{e9}-1-1.x86_64.rpm");
    }

    #[test]
    fn malformed_documents_are_refused() {
        for xml in [
            repomd_xml("a").replace("</checksum>", "</size>"),
            repomd_xml("a").replace(r#"href="a""#, r#"href="a"#),
            repomd_xml("a").replace(r#"href="a""#, "href=a"),
            repomd_xml("a").replace("</data>", ""),
        ] {
            assert!(repomd(xml.as_bytes()).is_err(), "{xml}");
        }
    }
}
