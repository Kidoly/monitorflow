use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, Ed25519KeyPair, KeyPair};
use serde_json::json;

use super::*;

/// The modulus of a throwaway RSA key whose private half never left the
/// machine that signed the tokens below with OpenSSL; the exponent is
/// 65537. No private key is kept in the repository.
const RSA_N: &str = "uY5Ip8JepTeL5qPzDaKPMt4eIYjCsXLglyEY5FIfEniVV6meYf7mPjY6-WKMf3eIrWaAhOL9ewHKa0ck5_pUNvYNIJ6d8pbksD2Y0ONpGhr7DRIc3rHXLzMdTlNpAXoLUs71ZDpkrwTaYeQH3QUbK2QmH31sSpBSkYKQmKzhjwM7NXsxIfoRn7m_bG7USdtptsWsGeCnNyGXxJrVJv5z1XZzW3RLWK0m69DqSXaa8_HO4fNXCw-sm8lzk9wqql2KuKBaOXNfwV-Fj99ZCbZHuB_CAZnizJEnR3Bou09TVaDU_8IF0yAGw2neClQ7aJivycBpUGdBtXEXG94cxGGXHw";

/// `{"iss":"https://id.example.com","sub":"alice"}` signed with that key.
const RS256_K1: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImsxIn0.eyJpc3MiOiJodHRwczovL2lkLmV4YW1wbGUuY29tIiwic3ViIjoiYWxpY2UifQ.t_jDY2FleF6cC7U6co9-fY5zC8iKdPHDCPX9o6iTTT8sKI5O3SH6sN50VCrYcDOTNmnYkzBcHQyXD6X43DHj-zwiNpl7d__LRHG33rsTDY3v9FQuxiYZFEGjNRVfI1Kn4RSw5Jh4rgYntEfrWrL2SQkg2LbsqA3QUpm6htUeeMUN1Qr_XvWMqMkbn8ljXfVhR4Nc6ZRdBwu4j9PJhD4nvu7NIWifci_IVfzn-knFRxSbtf_14xc2xxDwP62_8qi4Yex54vTaWrT4bphnE332D5nedEW9_vbDByRJ6HH3ajpUhjAXAGcmFj651tVZeQvbPYRbZRHheqecQV-ZEk6ozg";
const RS512_K1: &str = "eyJhbGciOiJSUzUxMiIsInR5cCI6IkpXVCIsImtpZCI6ImsxIn0.eyJpc3MiOiJodHRwczovL2lkLmV4YW1wbGUuY29tIiwic3ViIjoiYWxpY2UifQ.LrwFAgfIIJh37BC85xjau5AYE463Z6Qy9QdUqv02Eixz8j3iAOw_xPeMmnO23IYTGkRNI0iTt6s5wKKYpNPDFYvGgrB88dKbKyP4Z8LyzTLoSmeV_gHDUQdOfS819jyZm6dr9IhsZ5PAK4WMQC96tSCcQ6MGUKndIhCC_DuDzO2c61WADSmdV3l1CQuu3elX3XKbA2vmYE3vJr5I7QxSW5nkIL-F1g60CWsCdCi8p0L2bN8_H_Yqlc8GoKCiQw3vJySbs-u1HMVLAE-PwxZlTkkj_-Kjt5t3v987169G8D72_k2gNZ3k1gpL_khb32x-WngZt4MY8At3fWAoCmR63A";
const PS256_K1: &str = "eyJhbGciOiJQUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6ImsxIn0.eyJpc3MiOiJodHRwczovL2lkLmV4YW1wbGUuY29tIiwic3ViIjoiYWxpY2UifQ.ASNWxVGjJLSoAxqaa_7IMWMHwlDsFzIxU7xaGzcv2Rg0LQerXhwbjKnadoBpuXgFomwzMHUSDfSTQWkbOv3YgHCky6GIZdVFldvlE_sRo9V8fckPAfPK73EaHozoccXVeDPS2Vd0Xg8cWkQQQbg81hRIsUYmEP_l8DpMR8Q9OARYjimpP-m5xP6QseS6QTXJzbd89Sqqdn2hu8K_wYAiDTEU352poZVAb9jqWcQB-Qw0rqqmk_8Vb8S0sZvz0ZcMCC8Jy_6YltpIeQeON83pvtKuI6_h8HnGV6nEPootorkP6NNqF7S4NmA6VosPzjVsT-iAeAv6Yzco4ON216N0BA";
/// The same, without a `kid`.
const RS256: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2lkLmV4YW1wbGUuY29tIiwic3ViIjoiYWxpY2UifQ.B92T8TWCxzFSMD28cYtUWoByE8qRSPDfMg_cd-ntZnYc9KxzHHXfIWJf7s-5i2cYfns529MXlCYB6zf64ltNsAlv-D-setu5dpQkPvdbgA8a4nm5Nc2_PMMw5ejtn72ojha8NXpa9VGjqhOja8Gx_jogkCWUAGcn0Qr6EtSRRbvURjDBhdDZfDFQUcAyashIHhhouKxQnnCBHLAFrAHIRxBU_V7W25rpRSCblSUnN31XNV2K1IgpnkrWORgDrL1vYEWiTnIytiK1P92sHx-YcTSKnK9fSZ03AVtoZ0YXFp1235VUqBjb0PKflNDoW08Ig0dgJPaN84IN9N9BAO3LXA";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn signing_input(header: &Value, claims: &Value) -> String {
    format!(
        "{}.{}",
        b64(header.to_string().as_bytes()),
        b64(claims.to_string().as_bytes())
    )
}

fn rsa_jwk(kid: &str) -> Jwk {
    Jwk {
        kty: "RSA".into(),
        kid: Some(kid.into()),
        n: Some(RSA_N.into()),
        e: Some("AQAB".into()),
        ..Jwk::default()
    }
}

/// One of the tokens above.
fn rsa_token(alg: &str, kid: Option<&str>) -> String {
    match (alg, kid) {
        ("RS256", Some("k1")) => RS256_K1,
        ("RS512", Some("k1")) => RS512_K1,
        ("PS256", Some("k1")) => PS256_K1,
        ("RS256", None) => RS256,
        _ => unreachable!(),
    }
    .to_owned()
}

fn claims() -> Value {
    json!({ "iss": "https://id.example.com", "sub": "alice" })
}

fn set(keys: Vec<Jwk>) -> JwkSet {
    JwkSet { keys }
}

#[test]
fn rsa_signatures_verify() {
    for alg in ["RS256", "RS512", "PS256"] {
        let token = rsa_token(alg, Some("k1"));
        let verified = verify(&token, &set(vec![rsa_jwk("k1")])).unwrap();
        assert_eq!(verified.alg.name(), alg);
        assert_eq!(verified.claims["sub"], "alice");
    }
}

#[test]
fn a_key_without_kid_is_tried() {
    let token = rsa_token("RS256", None);
    let mut key = rsa_jwk("k1");
    key.kid = None;
    assert!(verify(&token, &set(vec![key])).is_ok());
}

#[test]
fn an_unknown_kid_asks_for_the_set_again() {
    // Signed with k1, while the provider still publishes only k0.
    let token = rsa_token("RS256", Some("k1"));
    assert_eq!(
        verify(&token, &set(vec![rsa_jwk("k0")])).unwrap_err(),
        JwsError::UnknownKey
    );
    assert_eq!(peek_kid(&token).unwrap().as_deref(), Some("k1"));
}

#[test]
fn a_changed_payload_fails() {
    let token = rsa_token("RS256", Some("k1"));
    let mut parts: Vec<&str> = token.split('.').collect();
    let forged = b64(json!({ "iss": "https://id.example.com", "sub": "root" })
        .to_string()
        .as_bytes());
    parts[1] = &forged;
    assert_eq!(
        verify(&parts.join("."), &set(vec![rsa_jwk("k1")])).unwrap_err(),
        JwsError::BadSignature
    );
}

#[test]
fn none_and_hmac_are_refused() {
    let none = format!("{}.", signing_input(&json!({ "alg": "none" }), &claims()));
    assert!(matches!(
        verify(&none, &set(vec![rsa_jwk("k1")])),
        Err(JwsError::Algorithm(_))
    ));
    let hs = format!(
        "{}.c2ln",
        signing_input(&json!({ "alg": "HS256" }), &claims())
    );
    assert!(
        matches!(verify(&hs, &set(vec![rsa_jwk("k1")])), Err(JwsError::Algorithm(a)) if a == "HS256")
    );
}

#[test]
fn a_key_of_another_algorithm_is_not_used() {
    let token = rsa_token("RS256", Some("k1"));
    let mut key = rsa_jwk("k1");
    key.alg = Some("PS256".into());
    assert_eq!(
        verify(&token, &set(vec![key])).unwrap_err(),
        JwsError::UnknownKey
    );
    let mut key = rsa_jwk("k1");
    key.use_ = Some("enc".into());
    assert_eq!(
        verify(&token, &set(vec![key])).unwrap_err(),
        JwsError::UnknownKey
    );
}

#[test]
fn critical_headers_are_refused() {
    let header = json!({ "alg": "RS256", "kid": "k1", "crit": ["exp"] });
    let input = signing_input(&header, &claims());
    let token = format!("{input}.AAAA");
    assert_eq!(
        verify(&token, &set(vec![rsa_jwk("k1")])).unwrap_err(),
        JwsError::Critical
    );
}

#[test]
fn malformed_tokens_are_refused() {
    let keys = set(vec![rsa_jwk("k1")]);
    for token in [
        "",
        "a.b",
        "a.b.c.d",
        "!!.e30.AA",
        &"a".repeat(MAX_TOKEN + 1),
    ] {
        assert!(verify(token, &keys).is_err(), "{token:.20}");
    }
}

#[test]
fn ecdsa_signatures_verify() {
    let rng = SystemRandom::new();
    for (alg, sign, crv) in [
        (
            "ES256",
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            "P-256",
        ),
        (
            "ES384",
            &signature::ECDSA_P384_SHA384_FIXED_SIGNING,
            "P-384",
        ),
    ] {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(sign, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(sign, pkcs8.as_ref(), &rng).unwrap();
        let point = key.public_key().as_ref();
        let half = (point.len() - 1) / 2;
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("ec".into()),
            crv: Some(crv.into()),
            x: Some(b64(&point[1..=half])),
            y: Some(b64(&point[1 + half..])),
            ..Jwk::default()
        };
        let input = signing_input(&json!({ "alg": alg, "kid": "ec" }), &claims());
        let sig = key.sign(&rng, input.as_bytes()).unwrap();
        let token = format!("{input}.{}", b64(sig.as_ref()));
        assert!(verify(&token, &set(vec![jwk.clone()])).is_ok(), "{alg}");
        let mut wrong = jwk;
        wrong.crv = Some("P-521".into());
        assert_eq!(
            verify(&token, &set(vec![wrong])).unwrap_err(),
            JwsError::UnknownKey
        );
    }
}

#[test]
fn ed25519_signatures_verify() {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let jwk = Jwk {
        kty: "OKP".into(),
        crv: Some("Ed25519".into()),
        x: Some(b64(key.public_key().as_ref())),
        ..Jwk::default()
    };
    let input = signing_input(&json!({ "alg": "EdDSA" }), &claims());
    let token = format!("{input}.{}", b64(key.sign(input.as_bytes()).as_ref()));
    assert!(verify(&token, &set(vec![jwk])).is_ok());
}

#[test]
fn key_sets_skip_what_they_cannot_read() {
    let body = json!({ "keys": [
        { "kty": "RSA", "kid": "a", "n": "AQAB", "e": "AQAB" },
        { "kty": "oct", "k": "c2VjcmV0" },
        { "kid": 5 },
        { "kty": "RSA", "use": "enc", "n": "AQAB", "e": "AQAB" },
    ]});
    let set = JwkSet::parse(body.to_string().as_bytes()).unwrap();
    assert_eq!(set.keys.len(), 3);
    assert_eq!(set.usable(), 1);
    assert!(set.has("a"));
    assert!(JwkSet::parse(b"[]").is_err());
}
