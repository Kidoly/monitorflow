//! The image pins in the compose project's `.env` (ADR-0056):
//! `SERVER_IMAGE`, `WEB_IMAGE`, `UPDATER_IMAGE` and `MIRROR_IMAGE`. Every
//! other line is kept as it is, but for the retention the web interface
//! sets (ADR-0069).

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use monitorflow_common::server_update::retention::{Kept, DEFAULT_LOGS, DEFAULT_METRICS};
use monitorflow_common::server_update::{write_atomic, Images};
use serde::{Deserialize, Serialize};

pub const SERVER: &str = "SERVER_IMAGE";
pub const WEB: &str = "WEB_IMAGE";
pub const UPDATER: &str = "UPDATER_IMAGE";
pub const MIRROR: &str = "MIRROR_IMAGE";
/// Set by hand, they win over the web interface's, and lock it.
pub const METRICS_RETENTION: &str = "METRICS_RETENTION";
pub const LOGS_RETENTION: &str = "LOGS_RETENTION";
/// The password of the server's own database role (ADR-0109), which the
/// compose file needs from the release that introduced it.
pub const POSTGRES_APP_PASSWORD: &str = "POSTGRES_APP_PASSWORD";
/// Written by the updater for the web interface.
pub const UI_METRICS_RETENTION: &str = "UI_METRICS_RETENTION";
pub const UI_LOGS_RETENTION: &str = "UI_LOGS_RETENTION";

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

/// Whether `pin` reads as an image reference, by tag or by digest, and
/// as nothing else in `.env`: no space, quote, `$`, `#` or control
/// character, a line feed above all, which would add a line.
fn is_reference(pin: &str) -> bool {
    pin.len() <= 512
        && pin
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && pin
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
}

impl Pins {
    /// Refuses a pin that is not an image reference, naming its line.
    /// Pins come from `.env` and from the updater's record of an update:
    /// neither may write more than an image's name back.
    pub fn check(&self) -> Result<(), String> {
        match self
            .entries()
            .into_iter()
            .find(|(_, pin)| pin.as_deref().is_some_and(|pin| !is_reference(pin)))
        {
            Some((key, _)) => Err(format!("{key} is not an image reference")),
            None => Ok(()),
        }
    }

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
    /// keeps the file's mode and owner. Refuses pins failing
    /// [`Pins::check`], whoever hands them over.
    pub fn set_pins(&self, pins: &Pins) -> io::Result<()> {
        pins.check()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let text = std::fs::read_to_string(&self.path)?;
        self.write(&with_pins(&text, pins))
    }

    /// The retention of the metrics and the logs, as the compose file
    /// resolves it.
    pub fn retention(&self) -> io::Result<(Kept, Kept)> {
        let text = self.text()?;
        let kept = |key, ui_key, default: &str| match value(&text, key) {
            Some(value) => Kept {
                value,
                locked: true,
            },
            None => Kept {
                value: value_or(&text, ui_key, default),
                locked: false,
            },
        };
        Ok((
            kept(METRICS_RETENTION, UI_METRICS_RETENTION, DEFAULT_METRICS),
            kept(LOGS_RETENTION, UI_LOGS_RETENTION, DEFAULT_LOGS),
        ))
    }

    /// Adds `key` with a new random password when it has no value, and says
    /// whether it did. 24 random bytes in hex: safe in a connection URL.
    pub fn ensure_secret(&self, key: &str) -> io::Result<bool> {
        let text = self.text()?;
        if value(&text, key).is_some() {
            return Ok(false);
        }
        self.write(&with_line(&text, key, &new_secret()?))?;
        Ok(true)
    }

    /// The whole file, to put back with [`Self::write`].
    pub fn text(&self) -> io::Result<String> {
        std::fs::read_to_string(&self.path)
    }

    /// Replaces the file, keeping its mode and owner.
    pub fn write(&self, text: &str) -> io::Result<()> {
        let metadata = std::fs::metadata(&self.path)?;
        let (dir, name) = self.dir_and_name()?;
        write_atomic(dir, name, text.as_bytes(), metadata.mode() & 0o7777)?;
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

fn new_secret() -> io::Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 24];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("no random numbers"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `KEY=value` of a line, `export` and spaces around the key allowed.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// The last non-empty value of `key` in `text`, as compose reads it.
fn value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(assignment)
        .filter(|(k, _)| *k == key)
        .map(|(_, value)| unquote(value).to_owned())
        .next_back()
        .filter(|value| !value.is_empty())
}

fn value_or(text: &str, key: &str, default: &str) -> String {
    value(text, key).unwrap_or_else(|| default.to_owned())
}

/// `text` with the line of `key` set to `value`, appended if missing.
pub fn with_line(text: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut written = false;
    for line in text.lines() {
        if assignment(line).is_some_and(|(k, _)| k == key) {
            if !written {
                out.push_str(&format!("{key}={value}\n"));
            }
            written = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !written {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
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
    fn retention_follows_the_compose_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".env");
        let env = EnvFile::new(path.clone());
        let kept = |value: &str, locked| Kept {
            value: value.into(),
            locked,
        };
        std::fs::write(&path, "DOMAIN=mf.example\nMETRICS_RETENTION=\n").unwrap();
        assert_eq!(
            env.retention().unwrap(),
            (kept("90d", false), kept("30d", false))
        );
        let text = with_line(&env.text().unwrap(), UI_METRICS_RETENTION, "60d");
        let text = with_line(&text, UI_LOGS_RETENTION, "7d");
        env.write(&text).unwrap();
        assert_eq!(
            env.retention().unwrap(),
            (kept("60d", false), kept("7d", false))
        );
        // Replaced in place, and by hand wins.
        let text = with_line(&env.text().unwrap(), UI_METRICS_RETENTION, "45d");
        assert_eq!(text.matches("UI_METRICS_RETENTION").count(), 1);
        env.write(&format!("{text}LOGS_RETENTION=\"1y\"\n"))
            .unwrap();
        assert_eq!(
            env.retention().unwrap(),
            (kept("45d", false), kept("1y", true))
        );
    }

    #[test]
    fn only_image_references_are_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".env");
        let original = "DOMAIN=mf.example\nSERVER_IMAGE=s:1\n";
        std::fs::write(&path, original).unwrap();
        let env = EnvFile::new(path.clone());
        let digest = format!(
            "ghcr.io/kidoly/monitorflow-server@sha256:{}",
            "a".repeat(64)
        );
        for good in [
            digest.as_str(),
            "monitorflow-server:e2e-base",
            "localhost:5000/x_y.z:1",
        ] {
            assert_eq!(pins(Some(good), None, None).check(), Ok(()), "{good}");
        }
        for bad in [
            "",
            "s:1\nUPDATER_IMAGE=evil",
            "s:1\rDOCKER_SOCKET=/x",
            "s:1 # x",
            "${DOCKER_SOCKET}",
            "\"s:1\"",
            "-s:1",
            "s:1\0",
            &"a".repeat(513),
        ] {
            let forged = Pins {
                mirror: Some(bad.into()),
                ..pins(Some("s:2"), None, None)
            };
            assert_eq!(
                forged.check().unwrap_err(),
                "MIRROR_IMAGE is not an image reference",
                "{bad:?}"
            );
            let error = env.set_pins(&forged).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    #[test]
    fn a_secret_is_added_once_and_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "POSTGRES_PASSWORD=s3cret\nPOSTGRES_APP_PASSWORD=\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let env = EnvFile::new(path.clone());
        assert!(env.ensure_secret(POSTGRES_APP_PASSWORD).unwrap());
        let text = env.text().unwrap();
        let secret = value(&text, POSTGRES_APP_PASSWORD).unwrap();
        assert_eq!(secret.len(), 48);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()), "{secret}");
        assert_eq!(text.matches(POSTGRES_APP_PASSWORD).count(), 1, "{text}");
        assert!(text.starts_with("POSTGRES_PASSWORD=s3cret\n"), "{text}");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Set, it stays: the role's password follows this line.
        assert!(!env.ensure_secret(POSTGRES_APP_PASSWORD).unwrap());
        assert_eq!(env.text().unwrap(), text);
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
