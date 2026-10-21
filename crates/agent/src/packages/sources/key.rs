//! Just enough OpenPGP (RFC 9580) to check a signing key before trusting
//! it: decode one ASCII-armored public key block, walk its packets, and
//! compute the primary key's fingerprint. Signatures are not checked here;
//! apt and rpm do that with the installed key.

use base64::Engine;
use ring::digest;

const BEGIN: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----";
const END: &str = "-----END PGP PUBLIC KEY BLOCK-----";

/// Packet tags a public key block may hold: signature, public key, user
/// ID, public subkey, user attribute. Anything else, a secret key in
/// particular, is refused.
const ALLOWED_TAGS: [u8; 5] = [2, 6, 13, 14, 17];
const TAG_PUBLIC_KEY: u8 = 6;

/// A checked public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// Uppercase hex.
    pub fingerprint: String,
    /// The packets, re-armored canonically: nothing outside the block, so
    /// the installed file holds exactly what was checked.
    pub armored: String,
}

/// Decodes `armored` and checks its primary key has `expected` as
/// fingerprint (hex, any case, spaces ignored).
pub fn verify(armored: &str, expected: &str) -> Result<PublicKey, String> {
    let expected = normalize_fingerprint(expected)?;
    let packets = dearmor(armored)?;
    let fingerprint = primary_fingerprint(&packets)?;
    if fingerprint != expected {
        return Err(format!("fingerprint is {fingerprint}, expected {expected}"));
    }
    Ok(PublicKey {
        fingerprint,
        armored: armor(&packets),
    })
}

fn normalize_fingerprint(value: &str) -> Result<String, String> {
    let hex: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if !matches!(hex.len(), 40 | 64) || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("expected fingerprint is not 40 or 64 hex digits".into());
    }
    Ok(hex.to_ascii_uppercase())
}

/// The binary packets of the single armored block in `text`.
fn dearmor(text: &str) -> Result<Vec<u8>, String> {
    if text.matches(BEGIN).count() != 1 {
        return Err("expected exactly one public key block".into());
    }
    let mut lines = text.lines().map(str::trim_end);
    lines
        .find(|line| *line == BEGIN)
        .ok_or("no public key block")?;
    // Armor headers, like `Comment:`, end at the first blank line.
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if !line.contains(": ") {
            return Err("malformed armor header".into());
        }
    }
    let mut body = String::new();
    let mut checksum = None;
    let mut ended = false;
    for line in lines.by_ref() {
        if line == END {
            ended = true;
            break;
        }
        match line.strip_prefix('=') {
            Some(crc) if checksum.is_none() => checksum = Some(crc.to_owned()),
            Some(_) => return Err("more than one armor checksum".into()),
            None if checksum.is_some() => return Err("data after the armor checksum".into()),
            None => body.push_str(line.trim()),
        }
    }
    if !ended {
        return Err("public key block is not terminated".into());
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let packets = engine.decode(&body).map_err(|_| "invalid base64 in key")?;
    if let Some(crc) = checksum {
        let crc = engine.decode(crc).map_err(|_| "invalid armor checksum")?;
        if crc != crc24(&packets).to_be_bytes()[1..] {
            return Err("armor checksum mismatch".into());
        }
    }
    Ok(packets)
}

/// Fingerprint of the only primary key in `packets`, which must all be of
/// an allowed tag.
fn primary_fingerprint(mut packets: &[u8]) -> Result<String, String> {
    let mut fingerprint = None;
    while !packets.is_empty() {
        let (tag, body, rest) = packet(packets)?;
        if !ALLOWED_TAGS.contains(&tag) {
            return Err(format!("packet type {tag} is not allowed in a public key"));
        }
        if tag == TAG_PUBLIC_KEY {
            if fingerprint.is_some() {
                return Err("more than one primary key".into());
            }
            fingerprint = Some(key_fingerprint(body)?);
        }
        packets = rest;
    }
    fingerprint.ok_or_else(|| "no primary key".into())
}

/// One packet: tag, body and the bytes after it.
fn packet(data: &[u8]) -> Result<(u8, &[u8], &[u8]), String> {
    let truncated = || "truncated packet".to_string();
    let header = *data.first().ok_or_else(truncated)?;
    if header & 0x80 == 0 {
        return Err("invalid packet header".into());
    }
    let be = |bytes: &[u8]| bytes.iter().fold(0usize, |n, b| (n << 8) | *b as usize);
    let (tag, offset, len) = if header & 0x40 != 0 {
        let first = *data.get(1).ok_or_else(truncated)? as usize;
        let tag = header & 0x3f;
        match first {
            0..=191 => (tag, 2, first),
            192..=223 => {
                let second = *data.get(2).ok_or_else(truncated)? as usize;
                (tag, 3, ((first - 192) << 8) + second + 192)
            }
            255 => (tag, 6, be(data.get(2..6).ok_or_else(truncated)?)),
            _ => return Err("partial packet lengths are not allowed in keys".into()),
        }
    } else {
        let tag = (header >> 2) & 0x0f;
        let size = match header & 0x03 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err("indeterminate packet length".into()),
        };
        let len = be(data.get(1..1 + size).ok_or_else(truncated)?);
        (tag, 1 + size, len)
    };
    let end = offset.checked_add(len).ok_or_else(truncated)?;
    let body = data.get(offset..end).ok_or_else(truncated)?;
    Ok((tag, body, &data[end..]))
}

/// RFC 9580 §5.5.4: SHA-1 for version 4 keys, SHA-256 for 5 and 6.
fn key_fingerprint(body: &[u8]) -> Result<String, String> {
    let mut input = Vec::with_capacity(body.len() + 5);
    let algorithm = match body.first() {
        Some(4) => {
            let len = u16::try_from(body.len()).map_err(|_| "key packet too long")?;
            input.push(0x99);
            input.extend_from_slice(&len.to_be_bytes());
            &digest::SHA1_FOR_LEGACY_USE_ONLY
        }
        Some(version @ (5 | 6)) => {
            let len = u32::try_from(body.len()).map_err(|_| "key packet too long")?;
            input.push(if *version == 5 { 0x9a } else { 0x9b });
            input.extend_from_slice(&len.to_be_bytes());
            &digest::SHA256
        }
        _ => return Err("unsupported key version".into()),
    };
    input.extend_from_slice(body);
    Ok(hex_upper(digest::digest(algorithm, &input).as_ref()))
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Canonical armor: 64-column base64 and the CRC-24 checksum, which old
/// gpg versions still expect.
fn armor(packets: &[u8]) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let body = engine.encode(packets);
    let mut out = format!("{BEGIN}\n\n");
    for chunk in body.as_bytes().chunks(64) {
        // Base64 output is ASCII.
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    let crc = engine.encode(&crc24(packets).to_be_bytes()[1..]);
    out.push_str(&format!("={crc}\n{END}\n"));
    out
}

/// RFC 4880 §6.1.
fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0x00B7_04CE;
    for byte in data {
        crc ^= (*byte as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x0100_0000 != 0 {
                crc ^= 0x0186_4CFB;
            }
        }
    }
    crc & 0x00FF_FFFF
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A version 4 key packet (Ed25519 public key, fixed bytes) and a user
    /// ID, armored, with its fingerprint computed independently.
    pub(crate) fn test_key(seed: u8) -> (String, String) {
        let mut body = vec![4, 0x65, 0x00, 0x00, 0x00, 22, 9];
        body.extend_from_slice(&[0x2b, 0x06, 0x01, 0x04, 0x01, 0xda, 0x47, 0x0f, 0x01]);
        body.extend_from_slice(&[0x01, 0x07, 0x40]);
        body.extend((0..32).map(|i| seed.wrapping_add(i)));
        let mut packets = vec![0xc6, body.len() as u8];
        packets.extend_from_slice(&body);
        let uid = b"MonitorFlow test <test@example.net>";
        packets.extend_from_slice(&[0xcd, uid.len() as u8]);
        packets.extend_from_slice(uid);
        let mut hashed = vec![0x99, 0, body.len() as u8];
        hashed.extend_from_slice(&body);
        let fingerprint =
            hex_upper(digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, &hashed).as_ref());
        (armor(&packets), fingerprint)
    }

    #[test]
    fn accepts_the_expected_fingerprint_in_any_format() {
        let (armored, fingerprint) = test_key(1);
        let spaced: String = fingerprint
            .to_lowercase()
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!("junk before\n{armored}junk after\n");
        let key = verify(&text, &spaced).unwrap();
        assert_eq!(key.fingerprint, fingerprint);
        assert_eq!(key.armored, armored);
    }

    #[test]
    fn checks_a_key_exported_by_gpg() {
        // `gpg --quick-gen-key ... ed25519 sign never; gpg --armor --export`.
        let armored = include_str!("testdata/gpg-ed25519.asc");
        let key = verify(armored, "B83E4A90D7E15769D955A46A7414A7809EA73D78").unwrap();
        // gpg's armor is already canonical.
        assert_eq!(key.armored, armored);
    }

    #[test]
    fn refuses_another_fingerprint() {
        let (armored, _) = test_key(1);
        let (_, other) = test_key(2);
        let error = verify(&armored, &other).unwrap_err();
        assert!(error.contains("expected"), "{error}");
    }

    #[test]
    fn refuses_bad_blocks() {
        let (armored, fingerprint) = test_key(1);
        let check = |text: &str| verify(text, &fingerprint).unwrap_err();
        assert!(check(&format!("{armored}{armored}")).contains("exactly one"));
        assert!(check(&armored.replace(&format!("{END}\n"), "")).contains("not terminated"));
        // A flipped data character breaks the checksum.
        let mut lines: Vec<String> = armored.lines().map(str::to_owned).collect();
        let flipped = if lines[2].starts_with('x') { "y" } else { "x" };
        lines[2].replace_range(0..1, flipped);
        assert!(check(&lines.join("\n")).contains("checksum"));
        assert!(verify(&armored, "1234").unwrap_err().contains("hex digits"));
    }

    #[test]
    fn refuses_secret_keys_and_two_primaries() {
        let (_, fingerprint) = test_key(1);
        let secret = armor(&[0xc5, 1, 4]);
        assert!(verify(&secret, &fingerprint)
            .unwrap_err()
            .contains("not allowed"));
        let one = super::dearmor(&test_key(1).0).unwrap();
        let two = [one.clone(), one].concat();
        assert!(verify(&armor(&two), &fingerprint)
            .unwrap_err()
            .contains("more than one"));
    }

    #[test]
    fn reads_old_format_headers() {
        // Old format, tag 6, one-byte length: the same key body.
        let packets = super::dearmor(&test_key(3).0).unwrap();
        let mut old = vec![0x98, packets[1]];
        old.extend_from_slice(&packets[2..]);
        let (_, fingerprint) = test_key(3);
        assert_eq!(primary_fingerprint(&old).unwrap(), fingerprint);
    }

    #[test]
    fn crc24_matches_the_rfc_initial_value_for_empty_input() {
        assert_eq!(crc24(b""), 0x00B7_04CE);
    }
}
