//! Version comparison for the fleet search (ADR-0187): dpkg's rules
//! (epoch, then runs of digits compared as numbers and runs of other
//! characters compared with `~` first and letters before symbols), which
//! also order RPM versions and the dotted versions of Windows programs
//! the way people expect.

use std::cmp::Ordering;

/// Compares two versions.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (epoch_a, a) = epoch(a.trim());
    let (epoch_b, b) = epoch(b.trim());
    epoch_a
        .cmp(&epoch_b)
        .then_with(|| runs(a.as_bytes(), b.as_bytes()))
}

/// `1:2.3` as `(1, "2.3")`; no epoch is 0.
fn epoch(version: &str) -> (u64, &str) {
    match version.split_once(':') {
        Some((epoch, rest)) if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) => {
            (epoch.parse().unwrap_or(u64::MAX), rest)
        }
        _ => (0, version),
    }
}

/// dpkg's weight of a character in a non-digit run.
fn weight(c: Option<&u8>) -> i32 {
    match c {
        None => 0,
        Some(b'~') => -1,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(*c),
        Some(c) => i32::from(*c) + 256,
    }
}

fn runs(mut a: &[u8], mut b: &[u8]) -> Ordering {
    while !a.is_empty() || !b.is_empty() {
        // The non-digit runs.
        while a.first().is_some_and(|c| !c.is_ascii_digit())
            || b.first().is_some_and(|c| !c.is_ascii_digit())
        {
            let (wa, wb) = (weight(a.first()), weight(b.first()));
            if wa != wb {
                return wa.cmp(&wb);
            }
            a = a.get(1..).unwrap_or_default();
            b = b.get(1..).unwrap_or_default();
        }
        // The digit runs, as numbers of any length.
        let zeros = |s: &[u8]| s.iter().take_while(|c| **c == b'0').count();
        a = &a[zeros(a)..];
        b = &b[zeros(b)..];
        let len = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
        let (la, lb) = (len(a), len(b));
        let order = la.cmp(&lb).then_with(|| a[..la].cmp(&b[..lb]));
        if order != Ordering::Equal {
            return order;
        }
        a = &a[la..];
        b = &b[lb..];
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_like_dpkg() {
        let ascending = [
            "1.0~rc1",
            "1.0",
            "1.0-1",
            "1.0.1",
            "1.2",
            "1.10",
            "3.0.14-1~deb12u2",
            "3.0.14-1~deb12u2+b1",
            "3.0.15-1~deb12u1",
            "1:0.1",
        ];
        for pair in ascending.windows(2) {
            assert_eq!(compare(pair[0], pair[1]), Ordering::Less, "{pair:?}");
            assert_eq!(compare(pair[1], pair[0]), Ordering::Greater, "{pair:?}");
        }
        assert_eq!(compare("1.01", "1.1"), Ordering::Equal);
        assert_eq!(compare(" 24.08 ", "24.08"), Ordering::Equal);
        assert_eq!(
            compare("10.0.20348.2340", "10.0.20348.887"),
            Ordering::Greater
        );
        assert_eq!(
            compare("99999999999999999999999.1", "99999999999999999999998.1"),
            Ordering::Greater
        );
    }
}
