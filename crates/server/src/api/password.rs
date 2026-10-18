//! Password hashing with argon2id. Hashing is CPU-bound by design: call
//! these functions from `spawn_blocking`, never directly on the runtime.

use std::sync::LazyLock;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// Hash verified when the username is unknown, so that a failed login
/// costs the same time whether or not the account exists.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash("dummy password for unknown users").expect("hashing a constant"));

#[derive(Debug, thiserror::Error)]
#[error("password hashing failed")]
pub struct HashError(argon2::password_hash::Error);

/// Hashes `password` into a PHC string with argon2id and the crate's default
/// parameters (m=19 MiB, t=2, p=1, the OWASP baseline) and a random salt.
pub fn hash(password: &str) -> Result<String, HashError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(HashError)
}

/// Whether `password` matches the PHC string `stored`. A malformed hash
/// never matches.
pub fn verify(stored: &str, password: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// Like [`verify`], but with no stored hash it still verifies against a
/// dummy hash before returning false.
pub fn verify_or_dummy(stored: Option<&str>, password: &str) -> bool {
    match stored {
        Some(stored) => verify(stored, password),
        None => {
            let _ = verify(&DUMMY_HASH, password);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trips_and_uses_argon2id() {
        let stored = hash("correct horse").unwrap();
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(verify(&stored, "correct horse"));
        assert!(!verify(&stored, "wrong horse"));
        // Salts differ between hashes of the same password.
        assert_ne!(stored, hash("correct horse").unwrap());
    }

    #[test]
    fn malformed_or_missing_hash_never_matches() {
        assert!(!verify("not a hash", ""));
        assert!(!verify_or_dummy(None, "dummy password for unknown users"));
    }
}
