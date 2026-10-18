use tempfile::TempDir;

use super::test_support::{Ca, Leaf};
use super::*;

/// 2048-bit RSA key in PKCS#1 form, generated once with `openssl genrsa
/// -traditional 2048` for this test only. It protects nothing; embedding it
/// avoids slow RSA key generation at test time (ring cannot generate RSA).
const TEST_ONLY_RSA_PKCS1: &str = "\
-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAry93qMDXjPKiOKy3BfAmVCr7C4ZYRRLhBJENeQDmv/oeYLht
1NHwhq/CLETW5NSWyuwAZm7kIIz4xIv8NsfAwe1HglHUiqgYWWriXL9Prmdx9LNG
srVpchvYUhH7yWa/2r7sMq+C9Kv6y0dJvlRX/0q6XQesLpt1qHxkfvKf7lDsBiWc
KD6NUuluOCklVGwtzyDxl7+Kw1hbiufolxK98RQIlhFNYAH/ONUkX2ot5COtOONN
+BpZ2S9AA8tCSNNSdXm1q6cJgjz6NXEOgFZ/1nVGAwDR59aDfXdeStk7vNtuXQ9W
EJNeUN8v01a3sAAaOsUYtvwoP20ZUJ+rfRBm3QIDAQABAoIBAAlYdBQ4ZryvVvBv
ZPt38mYsEs9sanoTheswnRzhlkMTN0VRTBP94ncfmjFdYhoRWJKuYLPA4legMvHd
RpEE5VITHpejq5MoC4B9qgy+QfnGV/cbYeqiWzk0phIewSVfNLND2yWLD7vjZCeC
/+VGP5mFZYgVTYirpMJeZx1kmRvlS5mv5PJLxuFSs+GqUUrp9GWdW+zCS0NEUsyv
4JiroOqBM7QrMnxgVKFkAo9/FNZKNBP8JsGp/6M9IyZ+d10xJgoX5aeQd+zfdtX5
Tsz0S5bobS/PgmMaOJ7Br9hxcOR5I/huMgcQ81aWvJK/oAKeGX0RLf2ui/rElGDX
+6Dg6FECgYEA4rAd38f+b+dBVn1okkLEUxrE4ewqcR0WXH08kXVaoSKCqTH/gs1H
NBvGu25pPLH6a0kYKugI0NUh8alPdeKDT/6uuD0qd5XEXePiABXRh2sN/VYjWIUs
TwUhz/96RBZ3HmgwFMTDNKYW1kv1YyQBtkHZMX6+yV8Aj5fz9x92WQUCgYEAxdaA
HfGns1AhgYWmpPeedYFULLTtPzh6WUmv4kOkQCO0lwBGEs1lD5jznSYkGfdZDrcv
/wnQS7kTaoXDF2rTuJBzNse/quFGrn6hGRAkl+vMhSDAK6OFNgWFuqh3Hl3pKuBF
pfusDv+0I/ed2sV3PMYLJypvNiQ/pDvV6FC5XfkCgYAhJ2LnJMfos+jRbnwJ0CwI
f4TGChVW/8sScdqW55P+tx5nAoH9Lmq4i0twhEBt+RXH9SPMuqz3ZgSV5I5TjnJo
E7aYCZIZdVUH6llWa9p+NMESCTQYMTn+wKBmXFYUeTiowNIvZ2upWpVCeE+GVXDO
qCjM3V2n8/YQCkhLH1XexQKBgQCh9dykpvT3Yx5K/rNSUjUzVVEn2GZxXzcqaZzH
y0BoEdMB24OCWQvnHPwzJspfR5Ns7KMqU0pm83jlt+PjA92YNwlbS2Op7QsERz5z
Wjfa93tTcWBhWVjwxMyZ5ti9z3vXUgsyuzo8fTFoUIq2UvoukqMGyUKKPFqthR61
NYDLYQKBgB//9yOg1CT7cf3CgZWhVeZuG3ccB7+0EAWDLgzowM0fF9Al2kgzYqNj
/wwQtdWeLbdosmKngDDPGT86G1GTtAxN1I/8kkviF82CZQleQik7UeQQvAP2LzpF
hrxhebIbxCurVXEeX2wcsu3FoZajA9T2IscbBQZTeEbu60WMm7Md
-----END RSA PRIVATE KEY-----
";

fn write(ca: &Ca, leaf: &Leaf) -> (TempDir, TlsPaths) {
    let dir = TempDir::new().unwrap();
    let (ca_cert, cert, key) = test_support::write(dir.path(), "agent", ca, leaf);
    (dir, TlsPaths { ca_cert, cert, key })
}

fn load_err(paths: &TlsPaths) -> String {
    Identity::load(paths).unwrap_err().to_string()
}

fn assert_usable(paths: &TlsPaths) {
    let identity = Identity::load(paths).unwrap();
    identity.server_config(Vec::new()).unwrap();
    identity.client_config().unwrap();
}

fn pem(label: &str, der: &[u8]) -> String {
    ::pem::encode(&::pem::Pem::new(label, der))
}

/// Contents and remainder of the DER element at the start of `der`.
fn der_element(der: &[u8]) -> (&[u8], &[u8]) {
    let (len, start) = match der[1] {
        n if n < 0x80 => (n as usize, 2),
        n => {
            let octets = (n & 0x7f) as usize;
            let len = der[2..2 + octets]
                .iter()
                .fold(0, |acc, &b| acc << 8 | b as usize);
            (len, 2 + octets)
        }
    };
    (&der[start..start + len], &der[start + len..])
}

/// DER element with `tag` around `contents`.
fn der_wrap(tag: u8, contents: &[u8]) -> Vec<u8> {
    let len = contents.len().to_be_bytes();
    let len = &len[len.iter().position(|&b| b != 0).unwrap_or(len.len() - 1)..];
    let mut out = vec![tag];
    if contents.len() < 0x80 {
        out.push(contents.len() as u8);
    } else {
        out.push(0x80 | len.len() as u8);
        out.extend_from_slice(len);
    }
    out.extend_from_slice(contents);
    out
}

#[test]
fn loads_pkcs8_key() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    assert!(std::fs::read_to_string(&paths.key)
        .unwrap()
        .starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(matches!(
        load_key(&paths.key).unwrap(),
        PrivateKeyDer::Pkcs8(_)
    ));
    assert_usable(&paths);
}

#[test]
fn loads_sec1_key() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    // A PKCS#8 EC key wraps the SEC1 `ECPrivateKey` structure.
    let pkcs8 = load_key(&paths.key).unwrap();
    // PrivateKeyInfo ::= SEQUENCE { version, algorithm, privateKey OCTET STRING }
    let (info, _) = der_element(pkcs8.secret_der());
    let (_, rest) = der_element(info);
    let (_, rest) = der_element(rest);
    let (sec1, _) = der_element(rest);
    std::fs::write(&paths.key, pem("EC PRIVATE KEY", sec1)).unwrap();
    assert!(matches!(
        load_key(&paths.key).unwrap(),
        PrivateKeyDer::Sec1(_)
    ));
    assert_usable(&paths);
}

#[test]
fn loads_pkcs1_key() {
    let PrivateKeyDer::Pkcs1(pkcs1) =
        rustls_pemfile::private_key(&mut TEST_ONLY_RSA_PKCS1.as_bytes())
            .unwrap()
            .unwrap()
    else {
        panic!("not PKCS#1");
    };
    // rcgen on ring imports only PKCS#8: wrap it as PrivateKeyInfo
    // { version 0, rsaEncryption with NULL parameters, key }.
    const RSA_ALGORITHM: &[u8] = &[
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let mut info = vec![0x02, 0x01, 0x00];
    info.extend_from_slice(RSA_ALGORITHM);
    info.extend(der_wrap(0x04, pkcs1.secret_pkcs1_der()));
    let pkcs8 = der_wrap(0x30, &info);
    let key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&pkcs8.into(), &rcgen::PKCS_RSA_SHA256)
        .unwrap();
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client_with_key("agent-1", 10, key));
    std::fs::write(&paths.key, TEST_ONLY_RSA_PKCS1).unwrap();
    assert!(matches!(
        load_key(&paths.key).unwrap(),
        PrivateKeyDer::Pkcs1(_)
    ));
    assert_usable(&paths);
}

#[test]
fn missing_file_names_it() {
    let ca = Ca::new("ca");
    let (dir, mut paths) = write(&ca, &ca.client("agent-1", 10));
    paths.ca_cert = dir.path().join("nope.pem");
    let err = load_err(&paths);
    assert!(err.contains("nope.pem: file not found"), "{err}");
}

#[test]
fn unreadable_file_names_it() {
    let ca = Ca::new("ca");
    let (dir, mut paths) = write(&ca, &ca.client("agent-1", 10));
    paths.cert = dir.path().to_owned();
    let err = load_err(&paths);
    assert!(
        err.starts_with(&format!("cannot read {}", dir.path().display())),
        "{err}"
    );
}

#[test]
fn file_without_certificate() {
    let ca = Ca::new("ca");
    let (_dir, mut paths) = write(&ca, &ca.client("agent-1", 10));
    paths.cert = paths.key.clone();
    let err = load_err(&paths);
    assert!(err.contains("agent-key.pem: no certificate found"), "{err}");
}

#[test]
fn file_without_key() {
    let ca = Ca::new("ca");
    let (_dir, mut paths) = write(&ca, &ca.client("agent-1", 10));
    paths.key = paths.cert.clone();
    let err = load_err(&paths);
    assert!(err.contains("agent.pem: no private key found"), "{err}");
}

#[test]
fn malformed_pem() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    std::fs::write(&paths.cert, "-----BEGIN CERTIFICATE-----\n!!!\n").unwrap();
    let err = load_err(&paths);
    assert!(err.contains("agent.pem: malformed PEM"), "{err}");
}

#[test]
fn malformed_key_error_hides_content() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    std::fs::write(&paths.key, "-----BEGIN PRIVATE KEY-----\nc2VjcmV0!\n").unwrap();
    let err = load_err(&paths);
    assert!(err.contains("agent-key.pem: malformed PEM"), "{err}");
    assert!(!err.contains("c2VjcmV0"), "{err}");
}

#[test]
fn invalid_key() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    let pem = pem("PRIVATE KEY", b"not a key");
    std::fs::write(&paths.key, pem).unwrap();
    let err = load_err(&paths);
    assert!(
        err.contains("agent-key.pem: unsupported or invalid private key"),
        "{err}"
    );
}

#[test]
fn invalid_ca_certificate() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    let pem = pem("CERTIFICATE", b"not a cert");
    std::fs::write(&paths.ca_cert, pem).unwrap();
    let err = load_err(&paths);
    assert!(err.contains("ca.pem: invalid certificate"), "{err}");
}

#[test]
fn key_that_does_not_match_certificate() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    let other = ca.client("agent-2", 11);
    std::fs::write(&paths.key, other.key.serialize_pem()).unwrap();
    let err = load_err(&paths);
    assert!(
        err.contains("agent-key.pem does not match certificate"),
        "{err}"
    );
}

#[test]
fn invalid_crl_is_a_config_error() {
    let ca = Ca::new("ca");
    let (_dir, paths) = write(&ca, &ca.client("agent-1", 10));
    let identity = Identity::load(&paths).unwrap();
    let err = identity
        .server_config(vec![CertificateRevocationListDer::from(b"junk".to_vec())])
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("cannot build TLS configuration"),
        "{err}"
    );
}

#[test]
fn debug_hides_key() {
    let ca = Ca::new("ca");
    let leaf = ca.client("agent-1", 10);
    let (_dir, paths) = write(&ca, &leaf);
    let debug = format!("{:?}", Identity::load(&paths).unwrap());
    assert_eq!(debug, "Identity { ca_certs: 1, chain_len: 1, .. }");
}
