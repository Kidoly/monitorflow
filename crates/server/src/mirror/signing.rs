//! The server's OpenPGP signing key in `<mirror_dir>/gnupg` (ADR-0030).

use super::runner::{Invocation, Tool};

/// `gpg --list-secret-keys` in machine-readable form.
pub fn list_secret_keys() -> Invocation {
    Invocation::new(
        Tool::Gpg,
        ["--batch", "--with-colons", "--list-secret-keys"],
    )
}

/// Fingerprints of the primary secret keys in `--with-colons` output.
pub fn fingerprints(colons: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut after_sec = false;
    for line in colons.lines() {
        let mut fields = line.split(':');
        match fields.next() {
            Some("sec") => after_sec = true,
            Some("fpr") if after_sec => {
                if let Some(fpr) = fields.nth(8).filter(|f| !f.is_empty()) {
                    found.push(fpr.to_owned());
                }
                after_sec = false;
            }
            Some("ssb" | "sub") => after_sec = false,
            _ => {}
        }
    }
    found
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("no signing key: create one with `monitorflow-server mirror key generate` or import one with `mirror key import`")]
    Missing,
    #[error("{0} secret keys in the mirror GNUPGHOME; keep exactly one")]
    Several(usize),
}

/// The one signing key's fingerprint.
pub fn the_key(colons: &str) -> Result<String, KeyError> {
    let mut keys = fingerprints(colons);
    match keys.len() {
        0 => Err(KeyError::Missing),
        1 => Ok(keys.remove(0)),
        n => Err(KeyError::Several(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "sec:u:4096:1:0123456789ABCDEF:1760000000:::u:::scESC:::+:::23::0:
fpr:::::::::AAAA0123456789ABCDEF0123456789ABCDEF0123:
grp:::::::::0011223344556677889900112233445566778899:
uid:u::::1760000000::HASH::MonitorFlow archive::::::::::0:
ssb:u:4096:1:FEDCBA9876543210:1760000000::::::e:::+:::23:
fpr:::::::::BBBB0123456789ABCDEF0123456789ABCDEF0123:
";

    #[test]
    fn the_primary_fingerprint_is_found() {
        assert_eq!(
            the_key(ONE).unwrap(),
            "AAAA0123456789ABCDEF0123456789ABCDEF0123"
        );
        assert_eq!(the_key(""), Err(KeyError::Missing));
        assert_eq!(the_key(&format!("{ONE}{ONE}")), Err(KeyError::Several(2)));
    }
}
