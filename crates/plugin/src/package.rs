//! The `.mfplugin` file (ADR-0103): a JSON object holding the manifest's
//! exact text, the module in base64, the signer's Ed25519 key and the
//! signature. The signature is checked before anything else is read.

use base64::Engine as _;
use ring::signature::Ed25519KeyPair;
use serde::{Deserialize, Serialize};

use crate::keys::{self, PublicKey};
use crate::manifest::{Manifest, ManifestError};
use crate::{limits, runtime};

/// The value of `format`.
pub const FORMAT: &str = "monitorflow-plugin-1";
/// Starts every signed message, so no other signature of the same key
/// can pass for a plugin's.
const DOMAIN: &[u8] = b"monitorflow-plugin-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    #[error("not a MonitorFlow plugin file: {0}")]
    Format(String),
    #[error("the plugin file is over {} MiB", limits::PACKAGE_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("signed by a key you do not trust (fingerprint {})", .0.fingerprint())]
    UntrustedKey(PublicKey),
    #[error("the signature does not match the plugin: the file was changed after it was signed")]
    BadSignature,
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the module: {0}")]
    Module(String),
}

/// A package as read from its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub format: String,
    pub manifest: String,
    pub module: String,
    pub public_key: String,
    pub signature: String,
}

/// A package whose signature verified against a trusted key, with a valid
/// manifest and a module the runtime accepts.
#[derive(Debug, Clone)]
pub struct Verified {
    pub manifest: Manifest,
    pub module: Vec<u8>,
    pub key: PublicKey,
    /// Signed by a catalogue key.
    pub official: bool,
    /// SHA-256 of the whole file: what an administrator confirms and what
    /// agents are granted.
    pub sha256: [u8; 32],
}

/// What the signature covers: the domain, the manifest's length and bytes,
/// and the module's SHA-256.
pub fn signed_bytes(manifest: &str, module: &[u8]) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    // Manifests are at most 16 KiB.
    bytes.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
    bytes.extend_from_slice(manifest.as_bytes());
    bytes.extend_from_slice(&sha256(module));
    bytes
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().try_into().expect("SHA-256 is 32 bytes")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl Package {
    /// Checks `manifest` and `module` as an install would, then signs them.
    pub fn sign(manifest: &str, module: &[u8], key: &Ed25519KeyPair) -> Result<Self, PackageError> {
        use ring::signature::KeyPair as _;
        let parsed = Manifest::parse(manifest)?;
        runtime::Plugin::load(module).map_err(|e| PackageError::Module(e.to_string()))?;
        let package = Self {
            format: FORMAT.to_owned(),
            manifest: manifest.to_owned(),
            module: b64().encode(module),
            public_key: b64().encode(key.public_key().as_ref()),
            signature: b64().encode(key.sign(&signed_bytes(manifest, module)).as_ref()),
        };
        if package.to_bytes().len() > limits::PACKAGE_BYTES {
            return Err(PackageError::TooLarge);
        }
        tracing::debug!(name = %parsed.name, "plugin signed");
        Ok(package)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > limits::PACKAGE_BYTES {
            return Err(PackageError::TooLarge);
        }
        let package: Self =
            serde_json::from_slice(bytes).map_err(|e| PackageError::Format(e.to_string()))?;
        if package.format != FORMAT {
            return Err(PackageError::Format(format!(
                "format {:?}, this version reads {FORMAT}",
                package.format
            )));
        }
        Ok(package)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("a package serializes");
        bytes.push(b'\n');
        bytes
    }

    /// The signer's key, before anything is checked.
    pub fn key(&self) -> Result<PublicKey, PackageError> {
        PublicKey::parse(&self.public_key).map_err(|e| PackageError::Format(e.to_string()))
    }

    /// Verifies the signature against the catalogue keys and `trusted`,
    /// then the manifest and the module. `file` is the package's bytes,
    /// for its SHA-256.
    pub fn verify(&self, file: &[u8], trusted: &[PublicKey]) -> Result<Verified, PackageError> {
        let key = self.key()?;
        let official = keys::official().contains(&key);
        if !official && !trusted.contains(&key) {
            return Err(PackageError::UntrustedKey(key));
        }
        let module = b64()
            .decode(&self.module)
            .map_err(|e| PackageError::Format(format!("module: {e}")))?;
        let signature = b64()
            .decode(&self.signature)
            .map_err(|e| PackageError::Format(format!("signature: {e}")))?;
        if self.manifest.len() > limits::MANIFEST_BYTES
            || !key.verifies(&signed_bytes(&self.manifest, &module), &signature)
        {
            return Err(PackageError::BadSignature);
        }
        let manifest = Manifest::parse(&self.manifest)?;
        runtime::Plugin::load(&module).map_err(|e| PackageError::Module(e.to_string()))?;
        Ok(Verified {
            manifest,
            module,
            key,
            official,
            sha256: sha256(file),
        })
    }
}

/// Reads and verifies a package file in one step.
pub fn verify_file(file: &[u8], trusted: &[PublicKey]) -> Result<Verified, PackageError> {
    Package::parse(file)?.verify(file, trusted)
}

#[cfg(test)]
pub(crate) mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::KeyPair as _;

    use super::*;

    /// A throwaway key, never written anywhere.
    pub(crate) fn throwaway_key() -> (Ed25519KeyPair, PublicKey) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key = PublicKey::from_bytes(pair.public_key().as_ref()).unwrap();
        (pair, key)
    }

    pub(crate) const MANIFEST: &str =
        r#"{"name": "noop", "version": "1.0.0", "kind": "check", "title": "No-op", "abi": 1}"#;

    pub(crate) fn module() -> Vec<u8> {
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "mf_run") (result i32) i32.const 0))"#).unwrap()
    }

    #[test]
    fn a_signed_package_verifies_with_its_key_only() {
        let (pair, key) = throwaway_key();
        let file = Package::sign(MANIFEST, &module(), &pair)
            .unwrap()
            .to_bytes();
        let verified = verify_file(&file, &[key]).unwrap();
        assert_eq!(verified.manifest.name, "noop");
        assert_eq!(verified.module, module());
        assert_eq!(verified.key, key);
        assert!(!verified.official);
        assert_eq!(verified.sha256, sha256(&file));
        let (_, other) = throwaway_key();
        assert_eq!(
            verify_file(&file, &[other]).unwrap_err(),
            PackageError::UntrustedKey(key)
        );
    }

    #[test]
    fn any_change_breaks_the_signature() {
        let (pair, key) = throwaway_key();
        let package = Package::sign(MANIFEST, &module(), &pair).unwrap();
        let mut changed = package.clone();
        changed.manifest = changed.manifest.replace("No-op", "No-op!");
        assert_eq!(
            changed.verify(b"", &[key]).unwrap_err(),
            PackageError::BadSignature
        );
        let mut changed = package.clone();
        let mut other = module();
        other.extend_from_slice(&[0, 4, 1, b'x', 0]);
        changed.module = b64().encode(other);
        assert_eq!(
            changed.verify(b"", &[key]).unwrap_err(),
            PackageError::BadSignature
        );
        // Another signer's key with this signature.
        let (_, other_key) = throwaway_key();
        let mut changed = package;
        changed.public_key = other_key.to_base64();
        assert_eq!(
            changed.verify(b"", &[other_key]).unwrap_err(),
            PackageError::BadSignature
        );
    }

    #[test]
    fn bad_files_are_refused_before_trust() {
        assert!(matches!(
            Package::parse(b"{}"),
            Err(PackageError::Format(_))
        ));
        assert!(matches!(
            Package::parse(b"\x00\x01"),
            Err(PackageError::Format(_))
        ));
        let big = vec![b' '; limits::PACKAGE_BYTES + 1];
        assert_eq!(Package::parse(&big).unwrap_err(), PackageError::TooLarge);
        let (pair, _) = throwaway_key();
        let mut package = Package::sign(MANIFEST, &module(), &pair).unwrap();
        package.format = "monitorflow-plugin-2".into();
        assert!(matches!(
            Package::parse(&package.to_bytes()),
            Err(PackageError::Format(_))
        ));
    }

    #[test]
    fn signing_checks_the_manifest_and_the_module() {
        let (pair, _) = throwaway_key();
        let bad_manifest = MANIFEST.replace("noop", "No op");
        assert!(matches!(
            Package::sign(&bad_manifest, &module(), &pair),
            Err(PackageError::Manifest(_))
        ));
        let wasi = wat::parse_str(
            r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1) (func (export "mf_run") (result i32) i32.const 0))"#,
        )
        .unwrap();
        assert!(matches!(
            Package::sign(MANIFEST, &wasi, &pair),
            Err(PackageError::Module(_))
        ));
        assert!(matches!(
            Package::sign(MANIFEST, b"not wasm", &pair),
            Err(PackageError::Module(_))
        ));
    }
}
