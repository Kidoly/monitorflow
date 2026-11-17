//! Update channels of the central server (ADR-0147). The regular channel
//! is the latest release, as ADR-0056 reads it. The tested channel is the
//! release named by `tested.json`, a file the release workflow writes when
//! the owner promotes a release, signed by the release key like
//! `SHA256SUMS` and published next to it.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::{download, fetch, release_base, Candidate, UpdateError};
use crate::release::{PublicKey, Version};

/// The channel file and its signature, next to the release files.
pub const TESTED_FILE: &str = "tested.json";
pub const TESTED_SIGNATURE: &str = "tested.json.sig";
/// Longest channel file and signature downloaded.
const MAX_CHANNEL_FILE: usize = 1024;
const MAX_SIGNATURE: usize = 1024;

/// Which releases a central server is offered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = UpdateChannel))]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The latest release, as soon as it is published.
    #[default]
    Regular,
    /// The latest release the owner promoted after it ran longer; for
    /// subscribers.
    Tested,
}

/// What `tested.json` says, once its signature verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub version: Version,
    pub promoted: NaiveDate,
}

#[derive(Deserialize)]
struct PromotionFile {
    channel: String,
    version: String,
    promoted: NaiveDate,
}

/// Checks `bytes` against `signature` with any of `keys`, then reads it.
pub fn verify_promotion(
    bytes: &[u8],
    signature: &[u8],
    keys: &[PublicKey],
) -> Result<Promotion, UpdateError> {
    let bad = |reason: String| UpdateError::BadChannel(reason);
    if !keys.iter().any(|key| key.verifies(bytes, signature)) {
        return Err(bad(
            "its signature does not verify with the release keys".into()
        ));
    }
    let file: PromotionFile =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid content: {e}")))?;
    if file.channel != "tested" {
        return Err(bad(format!(
            "it is for the {} channel",
            file.channel.escape_debug()
        )));
    }
    let version = file.version.parse().map_err(|_| {
        bad(format!(
            "`{}` is not a version",
            file.version.escape_debug()
        ))
    })?;
    Ok(Promotion {
        version,
        promoted: file.promoted,
    })
}

/// Downloads and verifies the release `channel` offers from `source`: the
/// latest one, or the one `tested.json` names, fetched by its version.
pub async fn fetch_channel(
    client: &reqwest::Client,
    source: &str,
    keys: &[PublicKey],
    channel: Channel,
) -> Result<Candidate, UpdateError> {
    let base = release_base(source, None);
    if channel == Channel::Regular {
        return fetch(client, &base, keys, None).await;
    }
    let bytes = download(client, format!("{base}{TESTED_FILE}"), MAX_CHANNEL_FILE).await?;
    let signature = download(client, format!("{base}{TESTED_SIGNATURE}"), MAX_SIGNATURE).await?;
    let promotion = verify_promotion(&bytes, &signature, keys)?;
    let base = release_base(source, Some(&promotion.version));
    fetch(client, &base, keys, Some(&promotion.version)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::PublicKey;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn key() -> (Ed25519KeyPair, PublicKey) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(pair.public_key().as_ref());
        let public = PublicKey::from_spki(&der).unwrap();
        (pair, public)
    }

    const FILE: &[u8] = br#"{"channel":"tested","version":"0.7.0","promoted":"2026-12-08"}"#;

    #[test]
    fn reads_a_signed_promotion() {
        let (pair, public) = key();
        let signature = pair.sign(FILE);
        let promotion = verify_promotion(FILE, signature.as_ref(), &[public]).unwrap();
        assert_eq!(promotion.version.to_string(), "0.7.0");
        assert_eq!(promotion.promoted.to_string(), "2026-12-08");
    }

    #[test]
    fn refuses_what_is_not_signed_or_not_a_promotion() {
        let (pair, public) = key();
        let (_, other) = key();
        let signature = pair.sign(FILE);
        let refused = |bytes: &[u8], sig: &[u8], keys: &[PublicKey]| {
            matches!(
                verify_promotion(bytes, sig, keys),
                Err(UpdateError::BadChannel(_))
            )
        };
        assert!(refused(FILE, signature.as_ref(), &[other]));
        assert!(refused(FILE, signature.as_ref(), &[]));
        let changed = String::from_utf8_lossy(FILE).replace("0.7.0", "9.9.9");
        assert!(refused(
            changed.as_bytes(),
            signature.as_ref(),
            std::slice::from_ref(&public)
        ));
        for bad in [
            r#"{"channel":"regular","version":"0.7.0","promoted":"2026-12-08"}"#,
            r#"{"channel":"tested","version":"latest","promoted":"2026-12-08"}"#,
            r#"{"channel":"tested","version":"0.7.0"}"#,
        ] {
            let sig = pair.sign(bad.as_bytes());
            assert!(
                refused(bad.as_bytes(), sig.as_ref(), std::slice::from_ref(&public)),
                "{bad}"
            );
        }
    }

    #[test]
    fn channels_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&Channel::Tested).unwrap(),
            "\"tested\""
        );
        assert_eq!(Channel::default(), Channel::Regular);
    }
}
