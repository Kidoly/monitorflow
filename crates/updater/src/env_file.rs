//! The image pins in the compose project's `.env` (ADR-0056):
//! `SERVER_IMAGE`, `WEB_IMAGE`, `UPDATER_IMAGE` and `MIRROR_IMAGE`. Every
//! other line is kept as it is.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use monitorflow_common::server_update::{write_atomic, Images};
use serde::{Deserialize, Serialize};

pub const SERVER: &str = "SERVER_IMAGE";
pub const WEB: &str = "WEB_IMAGE";
pub const UPDATER: &str = "UPDATER_IMAGE";
pub const MIRROR: &str = "MIRROR_IMAGE";

/// The pins; `None` is no line, the compose file's local build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    pub server: Option<String>,
    pub web: Option<String>,
    pub updater: Option<String>,
    pub mirror: Option<String>,
}

impl From<&Images> for Pins {
    fn from(images: &Images) -> Self {
        Self {
            server: Some(images.server.to_string()),
            web: Some(images.web.to_string()),
            updater: Some(images.updater.to_string()),
            mirror: images.mirror.as_ref().map(ToString::to_string),
        }
    }
}

impl Pins {
    fn entries(&self) -> [(&'static str, &Option<String>); 4] {
        [
            (SERVER, &self.server),
            (WEB, &self.web),
            (UPDATER, &self.updater),
            (MIRROR, &self.mirror),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct EnvFile {
    path: PathBuf,
}

impl EnvFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pins(&self) -> io::Result<Pins> {
        let text = std::fs::read_to_string(&self.path)?;
        let mut pins = Pins::default();
        for line in text.lines() {
            let Some((key, value)) = assignment(line) else {
                continue;
            };
            let value = Some(unquote(value).to_owned()).filter(|v| !v.is_empty());
            match key {
                SERVER => pins.server = value,
                WEB => pins.web = value,
                UPDATER => pins.updater = value,
                MIRROR => pins.mirror = value,
                _ => {}
            }
        }
        Ok(pins)
    }

    /// Replaces the pin lines with `pins`, appending the missing ones, and
    /// keeps the file's mode and owner.
    pub fn set_pins(&self, pins: &Pins) -> io::Result<()> {
        let text = std::fs::read_to_string(&self.path)?;
        let updated = with_pins(&text, pins);
        let metadata = std::fs::metadata(&self.path)?;
        let (dir, name) = self.dir_and_name()?;
        write_atomic(dir, name, updated.as_bytes(), metadata.mode() & 0o7777)?;
        std::os::unix::fs::chown(&self.path, Some(metadata.uid()), Some(metadata.gid()))
    }

    fn dir_and_name(&self) -> io::Result<(&Path, &str)> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::other("invalid .env path"))?;
        Ok((dir, name))
    }
}

/// `KEY=value` of a line, `export` and spaces around the key allowed.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// `text` with every pin line replaced by `pins`, in place of its first
/// occurrence; a pin set to `None` loses its line.
pub fn with_pins(text: &str, pins: &Pins) -> String {
    let mut written = [false; 4];
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let slot = assignment(line)
            .and_then(|(key, _)| pins.entries().iter().position(|(k, _)| *k == key));
        match slot {
            None => {
                out.push_str(line);
                out.push('\n');
            }
            Some(i) => {
                let (key, value) = pins.entries()[i];
                if let (false, Some(value)) = (written[i], value) {
                    out.push_str(&format!("{key}={value}\n"));
                }
                written[i] = true;
            }
        }
    }
    let missing: Vec<_> = pins
        .entries()
        .into_iter()
        .zip(written)
        .filter_map(|((key, value), done)| (!done).then_some((key, value.as_ref()?)))
        .collect();
    if !missing.is_empty() {
        out.push_str("\n# Images of the installed release, set by the updater (ADR-0056).\n");
        for (key, value) in missing {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(server: Option<&str>, web: Option<&str>, updater: Option<&str>) -> Pins {
        Pins {
            server: server.map(Into::into),
            web: web.map(Into::into),
            updater: updater.map(Into::into),
            mirror: None,
        }
    }

    #[test]
    fn pins_are_appended_then_replaced_in_place() {
        let original = "# settings\nDOMAIN=mf.example\nPOSTGRES_PASSWORD=s3cret\n";
        let first = with_pins(original, &pins(Some("s@1"), Some("w@1"), Some("u@1")));
        assert!(first.starts_with(original), "{first}");
        assert!(first.ends_with("SERVER_IMAGE=s@1\nWEB_IMAGE=w@1\nUPDATER_IMAGE=u@1\n"));
        let second = with_pins(&first, &pins(Some("s@2"), Some("w@2"), Some("u@2")));
        assert_eq!(second, first.replace("@1", "@2"));
        // Back to local builds: the lines go, the rest stays.
        let mirrored = Pins {
            mirror: Some("m@2".into()),
            ..pins(Some("s@2"), Some("w@2"), Some("u@2"))
        };
        let with_mirror = with_pins(&second, &mirrored);
        assert!(with_mirror.contains("MIRROR_IMAGE=m@2\n"));
        let local = with_pins(&with_mirror, &Pins::default());
        assert!(!local.contains("_IMAGE="), "{local}");
        assert!(local.contains("POSTGRES_PASSWORD=s3cret\n"));
    }

    #[test]
    fn duplicates_collapse_and_other_lines_are_untouched() {
        let text = "export SERVER_IMAGE=\"old\"\nSERVER_IMAGE=older\n  WEB_IMAGE = x\nSERVER_IMAGE_X=keep\n";
        let out = with_pins(text, &pins(Some("s@2"), None, None));
        assert_eq!(out, "SERVER_IMAGE=s@2\nSERVER_IMAGE_X=keep\n");
    }

    #[test]
    fn reads_and_writes_keeping_mode_and_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "A=1\nexport WEB_IMAGE='w@0'\nSERVER_IMAGE=\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let env = EnvFile::new(path.clone());
        assert_eq!(env.pins().unwrap(), pins(None, Some("w@0"), None));
        env.set_pins(&pins(Some("s@1"), Some("w@1"), None)).unwrap();
        assert_eq!(env.pins().unwrap(), pins(Some("s@1"), Some("w@1"), None));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
