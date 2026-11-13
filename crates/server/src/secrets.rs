//! Secrets the server keeps to call out, sealed at rest (ADR-0125): SMTP
//! passwords, webhook signing secrets, chat URLs and the AI key are stored
//! encrypted, with a key kept in the server's data directory, outside the
//! database.
//!
//! A sealed value is `mfs1:<key id>:<base64url(nonce | ciphertext | tag)>`:
//! format 1 is AES-256-GCM with a random 96-bit nonce, and the column it
//! belongs to as associated data, so a value moved to another column does
//! not open. The key file holds one key per line; the first one seals and
//! the others only open, which leaves room for a rotation.
//!
//! The keyring is loaded once at startup with [`install`]; [`seal`] and
//! [`open`] use it from anywhere, like the loaders that read channels.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use monitorflow_common::redact::Secret;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

/// The directory of the key file, under the server's data directory.
pub const DIR: &str = "secrets";
/// The key file in [`DIR`].
pub const KEYS_FILE: &str = "keys";
/// The prefix of a value sealed with format 1, AES-256-GCM.
const PREFIX: &str = "mfs1";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const HEADER: &str =
    "# MonitorFlow secrets key (ADR-0125). It opens the passwords, signing secrets\n\
# and keys stored in the database: keep it with the server's data and its\n\
# backups. One key per line; the first one seals, the others only open.\n";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error("{} can be read or written by others (mode {mode:o}): run chmod 600 on it", path.display())]
    Permissions { path: PathBuf, mode: u32 },
    #[error(
        "there is no secrets key at {}: an added central server uses the first one's. On the first central server, run install-server.sh --add-server, copy the file it writes here, then run install-server.sh --join with it",
        path.display()
    )]
    Missing { path: PathBuf },
    #[error("the secrets key is not loaded")]
    NotLoaded,
    #[error("the secrets key is already loaded")]
    AlreadyLoaded,
    #[error("a stored secret is not sealed")]
    NotSealed,
    #[error("a stored secret was sealed with key {0}, which this server does not have: restore its secrets key (ADR-0125), or enter the secret again")]
    UnknownKey(String),
    #[error("a stored secret does not open: it was changed, or it belongs to another column")]
    Corrupted,
    #[error("cannot draw random bytes")]
    Random,
}

/// The columns that hold sealed secrets. Each one is the associated data
/// of its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    NotificationChannels,
    UserContacts,
    IntegrationEndpoints,
    AiApiKey,
}

impl Column {
    /// `table.column`, as stored in `sealed_columns` and bound to values.
    pub fn name(self) -> &'static str {
        match self {
            Self::NotificationChannels => "notification_channels.secret",
            Self::UserContacts => "user_contacts.secret",
            Self::IntegrationEndpoints => "integration_endpoints.secret",
            Self::AiApiKey => "server_settings.ai_api_key",
        }
    }

    /// The table and the column of [`Column::name`].
    fn parts(self) -> (&'static str, &'static str) {
        self.name().split_once('.').unwrap_or_default()
    }
}

struct Key {
    id: String,
    key: LessSafeKey,
}

/// The keys of the key file; the first one seals.
pub struct Keyring {
    keys: Vec<Key>,
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyring").field("ids", &self.ids()).finish()
    }
}

/// The ID of `key`: the first four bytes of a SHA-256 of it, in hex. It
/// names a key in sealed values and logs without revealing it.
fn key_id(key: &[u8]) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(b"monitorflow secrets key id\0");
    context.update(key);
    let digest = context.finish();
    digest.as_ref()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Keyring {
    /// Reads the keys of a key file's text.
    fn parse(text: &str) -> Result<Self, String> {
        let mut keys: Vec<Key> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bytes = STANDARD
                .decode(line)
                .map_err(|_| format!("line {} is not base64", number + 1))?;
            if bytes.len() != KEY_LEN {
                return Err(format!("line {} is not a {KEY_LEN}-byte key", number + 1));
            }
            let id = key_id(&bytes);
            if keys.iter().any(|k| k.id == id) {
                return Err(format!("key {id} appears twice"));
            }
            let unbound = UnboundKey::new(&AES_256_GCM, &bytes)
                .map_err(|_| format!("line {} is not a key", number + 1))?;
            keys.push(Key {
                id,
                key: LessSafeKey::new(unbound),
            });
        }
        if keys.is_empty() {
            return Err("no key".into());
        }
        Ok(Self { keys })
    }

    /// The text of a new key file with one random key.
    fn generate_text() -> Result<String, SecretsError> {
        let mut bytes = [0u8; KEY_LEN];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| SecretsError::Random)?;
        Ok(format!("{HEADER}{}\n", STANDARD.encode(bytes)))
    }

    /// A keyring of one new random key, kept only in memory.
    pub fn generate() -> Result<Self, SecretsError> {
        Self::parse(&Self::generate_text()?).map_err(|reason| SecretsError::Corrupt {
            path: PathBuf::new(),
            reason,
        })
    }

    /// Reads `<data_dir>/secrets/keys`. When it does not exist, creates
    /// it with one new key if `create`, else fails: an added central
    /// server must use the first one's key, never a key of its own.
    pub fn open_or_create(data_dir: &Path, create: bool) -> Result<Self, SecretsError> {
        let dir = data_dir.join(DIR);
        let path = dir.join(KEYS_FILE);
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| SecretsError::Io { path, source }
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => {
                check_permissions(&path)?;
                text
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Err(SecretsError::Missing { path });
                }
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&dir)
                    .map_err(io(&dir))?;
                let text = Self::generate_text()?;
                write_new(&path, text.as_bytes()).map_err(io(&path))?;
                text
            }
            Err(error) => return Err(io(&path)(error)),
        };
        Self::parse(&text).map_err(|reason| SecretsError::Corrupt { path, reason })
    }

    /// The IDs of the keys, the sealing one first.
    pub fn ids(&self) -> Vec<&str> {
        self.keys.iter().map(|k| k.id.as_str()).collect()
    }

    /// `plaintext` sealed for `column` with the first key.
    pub fn seal(&self, column: Column, plaintext: &str) -> Result<String, SecretsError> {
        let key = &self.keys[0];
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| SecretsError::Random)?;
        let mut data = plaintext.as_bytes().to_vec();
        key.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(column.name()),
                &mut data,
            )
            .map_err(|_| SecretsError::Corrupted)?;
        let mut out = nonce.to_vec();
        out.append(&mut data);
        Ok(format!(
            "{PREFIX}:{}:{}",
            key.id,
            URL_SAFE_NO_PAD.encode(out)
        ))
    }

    /// The plaintext of `stored`, a value of `column`.
    pub fn open(&self, column: Column, stored: &str) -> Result<Secret, SecretsError> {
        let mut parts = stored.splitn(3, ':');
        let (Some(PREFIX), Some(id), Some(body)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(SecretsError::NotSealed);
        };
        let key = self
            .keys
            .iter()
            .find(|k| k.id == id)
            .ok_or_else(|| SecretsError::UnknownKey(id.to_owned()))?;
        let data = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| SecretsError::Corrupted)?;
        if data.len() < NONCE_LEN + TAG_LEN {
            return Err(SecretsError::Corrupted);
        }
        let (nonce, sealed) = data.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SecretsError::Corrupted)?;
        let mut sealed = sealed.to_vec();
        let plain = key
            .key
            .open_in_place(nonce, Aad::from(column.name()), &mut sealed)
            .map_err(|_| SecretsError::Corrupted)?;
        String::from_utf8(plain.to_vec())
            .map(Secret::new)
            .map_err(|_| SecretsError::Corrupted)
    }
}

/// Refuses a key file that group or others can read or write.
fn check_permissions(path: &Path) -> Result<(), SecretsError> {
    let meta = fs::metadata(path).map_err(|source| SecretsError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mode = meta.permissions().mode() & 0o777;
    if !meta.is_file() || mode & 0o077 != 0 {
        return Err(SecretsError::Permissions {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}

/// Writes `contents` to a temporary file with mode 0600, syncs it and
/// links it to `path`, which must not exist: a crash leaves no file or the
/// whole one, and an existing key is never replaced.
fn write_new(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    match fs::remove_file(&tmp) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    let linked = fs::hard_link(&tmp, path);
    let removed = fs::remove_file(&tmp);
    linked?;
    removed?;
    File::open(dir)?.sync_all()
}

static KEYRING: OnceLock<Keyring> = OnceLock::new();

/// Makes `keyring` the one [`seal`] and [`open`] use, once per process.
pub fn install(keyring: Keyring) -> Result<(), SecretsError> {
    KEYRING
        .set(keyring)
        .map_err(|_| SecretsError::AlreadyLoaded)
}

/// Loads a random key kept only in memory, unless one is loaded. For tests
/// and tools that never read what another process sealed.
pub fn install_for_tests() {
    KEYRING.get_or_init(|| Keyring::generate().expect("a random key"));
}

/// The loaded keyring.
pub fn keyring() -> Result<&'static Keyring, SecretsError> {
    KEYRING.get().ok_or(SecretsError::NotLoaded)
}

/// `plaintext` sealed for `column` with the loaded keyring.
pub fn seal(column: Column, plaintext: &str) -> Result<String, SecretsError> {
    keyring()?.seal(column, plaintext)
}

/// The plaintext of `stored`, a value of `column`, with the loaded
/// keyring.
pub fn open(column: Column, stored: &str) -> Result<Secret, SecretsError> {
    keyring()?.open(column, stored)
}

/// A stored secret's plaintext, or why it does not open, in words for the
/// last error of what uses it.
pub type Opened = Result<Secret, String>;

/// A value of `column` as read from the database: `None` when there is
/// none.
pub fn open_stored(column: Column, stored: Option<String>) -> Option<Opened> {
    stored.map(|value| open(column, &value).map_err(|error| error.to_string()))
}

mod store;
pub use store::{seal_stored, SEALED};

#[cfg(test)]
mod tests;
