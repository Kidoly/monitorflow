use serde_json::json;

use super::*;

fn settings() -> OidcSettings {
    OidcSettings {
        issuer: "https://id.example.com/realms/mf".into(),
        client_id: "monitorflow".into(),
        scopes: "openid profile email groups".into(),
        groups_claim: "groups".into(),
        ca_pem: None,
    }
}

fn discovery() -> Discovery {
    serde_json::from_value(json!({
        "issuer": "https://id.example.com/realms/mf",
        "authorization_endpoint": "https://id.example.com/auth",
        "token_endpoint": "https://id.example.com/token",
        "jwks_uri": "https://id.example.com/certs",
        "scopes_supported": ["openid", "profile", "email"],
    }))
    .unwrap()
}

fn claims(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn valid() -> Map<String, Value> {
    claims(json!({
        "iss": "https://id.example.com/realms/mf", "aud": "monitorflow", "sub": "u-1",
        "exp": 2_000, "iat": 1_000, "nonce": "n0",
    }))
}

const EXPECTED: Expected<'static> = Expected {
    issuer: "https://id.example.com/realms/mf",
    client_id: "monitorflow",
    nonce: "n0",
    now: 1_500,
};

fn refused(change: impl FnOnce(&mut Map<String, Value>)) -> String {
    let mut c = valid();
    change(&mut c);
    check_claims(&c, &EXPECTED).unwrap_err().to_string()
}

#[test]
fn a_valid_token_passes() {
    assert!(check_claims(&valid(), &EXPECTED).is_ok());
    let mut c = valid();
    c.insert("aud".into(), json!(["monitorflow", "other"]));
    c.insert("azp".into(), json!("monitorflow"));
    assert!(check_claims(&c, &EXPECTED).is_ok());
}

#[test]
fn each_claim_is_checked() {
    assert!(refused(|c| {
        c.insert("iss".into(), json!("https://evil.example.com"));
    })
    .contains("another issuer"));
    assert!(refused(|c| {
        c.remove("iss");
    })
    .contains("another issuer"));
    assert!(refused(|c| {
        c.insert("aud".into(), json!("other"));
    })
    .contains("another client"));
    assert!(refused(|c| {
        c.insert("aud".into(), json!(["monitorflow", "other"]));
    })
    .contains("issued to another"));
    assert!(refused(|c| {
        c.insert("azp".into(), json!("other"));
    })
    .contains("issued to another"));
    assert!(refused(|c| {
        c.insert("exp".into(), json!(1_400));
    })
    .contains("expired"));
    assert!(refused(|c| {
        c.remove("exp");
    })
    .contains("no expiry"));
    assert!(refused(|c| {
        c.insert("iat".into(), json!(1_600));
    })
    .contains("future"));
    assert!(refused(|c| {
        c.insert("nonce".into(), json!("n1"));
    })
    .contains("nonce"));
    assert!(refused(|c| {
        c.remove("nonce");
    })
    .contains("nonce"));
    assert!(refused(|c| {
        c.insert("sub".into(), json!(""));
    })
    .contains("names nobody"));
}

#[test]
fn a_minute_of_leeway() {
    let mut c = valid();
    c.insert("exp".into(), json!(1_500 - LEEWAY_S));
    c.insert("iat".into(), json!(1_500 + LEEWAY_S));
    assert!(check_claims(&c, &EXPECTED).is_ok());
}

#[test]
fn the_authorize_url_carries_pkce_and_known_scopes() {
    let url = authorize_url(
        &discovery(),
        &settings(),
        "https://mf.example.com/api/auth/oidc/callback",
        "st",
        "no",
        "verifier",
    );
    let url = Url::parse(&url).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "monitorflow");
    assert_eq!(
        query["redirect_uri"],
        "https://mf.example.com/api/auth/oidc/callback"
    );
    assert_eq!(query["scope"], "openid profile email");
    assert_eq!(query["state"], "st");
    assert_eq!(query["nonce"], "no");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["code_challenge"], challenge("verifier"));
}

#[test]
fn pkce_matches_rfc_7636() {
    // RFC 7636, appendix B.
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert_ne!(random(), random());
    assert_eq!(random().len(), 43);
}

#[test]
fn discovery_is_checked() {
    let d = discovery();
    assert!(d.check("https://id.example.com/realms/mf/").is_ok());
    assert!(matches!(
        d.check("https://id.example.com/realms/other"),
        Err(OidcError::Issuer { .. })
    ));
    let mut plain = discovery();
    plain.token_endpoint = "http://id.example.com/token".into();
    assert_eq!(
        plain.check(&plain.issuer.clone()),
        Err(OidcError::NotHttps("token endpoint"))
    );
    for odd in [
        "https://id.example.com/auth\n",
        "https://id.example.com/a\u{1}",
        "https://id.example.com/é",
    ] {
        let mut odd_endpoint = discovery();
        odd_endpoint.authorization_endpoint = odd.into();
        assert_eq!(
            odd_endpoint.check(&odd_endpoint.issuer.clone()),
            Err(OidcError::NotHttps("authorization endpoint")),
            "{odd:?}"
        );
    }
    let mut no_pkce = discovery();
    no_pkce.code_challenge_methods_supported = Some(vec!["plain".into()]);
    assert_eq!(
        no_pkce.check(&no_pkce.issuer.clone()),
        Err(OidcError::NoPkce)
    );
    let mut hs = discovery();
    hs.id_token_signing_alg_values_supported = Some(vec!["HS256".into()]);
    assert!(matches!(
        hs.check(&hs.issuer.clone()),
        Err(OidcError::SigningAlgorithm(_))
    ));
}

#[test]
fn the_secret_goes_where_the_provider_wants_it() {
    let mut d = discovery();
    assert!(!d.secret_in_body());
    d.token_endpoint_auth_methods_supported = Some(vec!["client_secret_post".into()]);
    assert!(d.secret_in_body());
    d.token_endpoint_auth_methods_supported = Some(vec![
        "client_secret_post".into(),
        "client_secret_basic".into(),
    ]);
    assert!(!d.secret_in_body());
}

#[test]
fn groups_and_names_from_claims() {
    let c =
        claims(json!({ "groups": ["a", 3, "b"], "role": "x", "sub": "1234", "email": "al ice@x" }));
    assert_eq!(
        groups_of(&c, "groups"),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(groups_of(&c, "role"), Some(vec!["x".to_owned()]));
    assert_eq!(groups_of(&c, "none"), None);
    assert_eq!(username_of(&c).as_deref(), Some("1234"));
    let c = claims(json!({ "preferred_username": "alice", "sub": "1234" }));
    assert_eq!(username_of(&c).as_deref(), Some("alice"));
}

#[test]
fn form_encoding() {
    assert_eq!(form(&[("a b", "c&d=é")]), "a%20b=c%26d%3D%C3%A9");
}
