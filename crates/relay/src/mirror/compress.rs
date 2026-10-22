//! Compressions of apt `Packages` indexes and dnf primary metadata
//! (ADR-0034), told by the file name's extension. Every decoder is pure
//! Rust.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use super::meta::MetaError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Plain,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
}

impl Compression {
    /// Every compression, cheapest to read first.
    pub const ALL: [Self; 5] = [Self::Plain, Self::Gzip, Self::Zstd, Self::Xz, Self::Bzip2];

    /// The extension, with its dot; empty for [`Compression::Plain`].
    pub fn extension(self) -> &'static str {
        match self {
            Self::Plain => "",
            Self::Gzip => ".gz",
            Self::Xz => ".xz",
            Self::Bzip2 => ".bz2",
            Self::Zstd => ".zst",
        }
    }

    /// The compression of `name` and the name without its extension; any
    /// unknown extension is part of the name.
    pub fn of(name: &str) -> (Self, &str) {
        Self::ALL
            .into_iter()
            .filter(|c| *c != Self::Plain)
            .find_map(|c| Some((c, name.strip_suffix(c.extension())?)))
            .unwrap_or((Self::Plain, name))
    }

    /// Decompresses `raw`, the content of `file`.
    pub fn decoder<'a>(
        self,
        file: &str,
        raw: impl Read + 'a,
    ) -> Result<Box<dyn BufRead + 'a>, MetaError> {
        Ok(match self {
            Self::Plain => Box::new(BufReader::new(raw)),
            Self::Gzip => Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(raw))),
            Self::Xz => Box::new(BufReader::new(lzma_rust2::XzReader::new(raw, true))),
            Self::Bzip2 => Box::new(BufReader::new(bzip2::read::MultiBzDecoder::new(raw))),
            Self::Zstd => Box::new(BufReader::new(
                ruzstd::decoding::StreamingDecoder::new(raw)
                    .map_err(|e| MetaError::invalid(file, e.to_string()))?,
            )),
        })
    }
}

/// The decompressed content of `path`, the file `file` of a snapshot,
/// by its extension. Blocking.
pub fn open(file: &str, path: &Path) -> Result<Box<dyn BufRead>, MetaError> {
    let raw = File::open(path).map_err(|e: io::Error| MetaError::invalid(file, e.to_string()))?;
    Compression::of(file).0.decoder(file, raw)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    /// `data` compressed with `compression`.
    pub(crate) fn compress(compression: Compression, data: &[u8]) -> Vec<u8> {
        match compression {
            Compression::Plain => data.to_vec(),
            Compression::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), Default::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            Compression::Xz => {
                let options = lzma_rust2::XzOptions::with_preset(1);
                let mut e = lzma_rust2::XzWriter::new(Vec::new(), options).unwrap();
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            Compression::Bzip2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), Default::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            Compression::Zstd => {
                ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
            }
        }
    }

    fn read(compression: Compression, raw: &[u8]) -> Result<String, String> {
        let mut out = String::new();
        compression
            .decoder("f", raw)
            .map_err(|e| e.to_string())?
            .read_to_string(&mut out)
            .map_err(|e| e.to_string())?;
        Ok(out)
    }

    #[test]
    fn names_tell_the_compression() {
        assert_eq!(
            Compression::of("Packages"),
            (Compression::Plain, "Packages")
        );
        assert_eq!(
            Compression::of("a/Packages.xz"),
            (Compression::Xz, "a/Packages")
        );
        assert_eq!(Compression::of("p.xml.bz2"), (Compression::Bzip2, "p.xml"));
        assert_eq!(Compression::of("p.xml.gz"), (Compression::Gzip, "p.xml"));
        assert_eq!(Compression::of("p.xml.zst"), (Compression::Zstd, "p.xml"));
        assert_eq!(
            Compression::of("Packages.lz4"),
            (Compression::Plain, "Packages.lz4")
        );
        for c in Compression::ALL {
            assert_eq!(Compression::of(&format!("x{}", c.extension())).0, c);
        }
    }

    #[test]
    fn every_compression_round_trips() {
        let text = "Package: a\n".repeat(1000);
        for c in Compression::ALL {
            let raw = compress(c, text.as_bytes());
            assert_eq!(read(c, &raw).unwrap(), text, "{c:?}");
            // Concatenated members, as `cat a.gz b.gz` makes.
            if matches!(c, Compression::Gzip | Compression::Xz | Compression::Bzip2) {
                let twice = [raw.clone(), raw].concat();
                assert_eq!(read(c, &twice).unwrap(), text.repeat(2), "{c:?}");
            }
            if c != Compression::Plain {
                assert!(read(c, text.as_bytes()).is_err(), "{c:?} of plain text");
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p");
        std::fs::write(&path, compress(Compression::Xz, b"xz")).unwrap();
        let mut out = String::new();
        open("p.xz", &path)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "xz");
        assert!(open("p.xz", &dir.path().join("absent")).is_err());
    }
}
