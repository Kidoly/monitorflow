use super::*;

fn b64(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(text.replace(char::is_whitespace, ""))
        .unwrap()
}

// RFC 8291, appendix A.
const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AS_PUBLIC: &str =
    "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const ECDH: &str = "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs";
const PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24";

#[test]
fn keys_match_rfc_8291() {
    let (cek, nonce) = keys(
        &b64(ECDH),
        &b64(AUTH),
        &b64(UA_PUBLIC),
        &b64(AS_PUBLIC),
        &b64(SALT),
    )
    .unwrap();
    assert_eq!(cek, b64("oIhVW04MRdy2XN9CiKLxTg"));
    assert_eq!(nonce, b64("4h_95klXJ5E_qnoN"));
}

#[test]
fn the_body_matches_rfc_8291() {
    let salt: [u8; 16] = b64(SALT).try_into().unwrap();
    let body = seal(
        &b64(ECDH),
        &b64(AUTH),
        &b64(UA_PUBLIC),
        &b64(AS_PUBLIC),
        &salt,
        &b64(PLAINTEXT),
    )
    .unwrap();
    let header = b64(
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
         mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8",
    );
    let ciphertext = b64(
        "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrdirN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs
         bI_0LpXMuGvnzQ",
    );
    assert_eq!(body[..86], header[..]);
    assert_eq!(body[86..], ciphertext[..]);
}

/// Decrypts `body` as a phone would, with its private key.
fn open_as_phone(
    private: agreement::EphemeralPrivateKey,
    ua_public: &[u8],
    auth: &[u8],
    body: &[u8],
) -> Vec<u8> {
    let salt = &body[..16];
    assert_eq!(&body[16..20], &RECORD_SIZE.to_be_bytes());
    let id_len = usize::from(body[20]);
    let as_public = &body[21..21 + id_len];
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
    let secret = agreement::agree_ephemeral(private, &peer, |s| s.to_vec()).unwrap();
    let (cek, nonce) = keys(&secret, auth, ua_public, as_public, salt).unwrap();
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
    let mut record = body[21 + id_len..].to_vec();
    let plain = key
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
            aead::Aad::empty(),
            &mut record,
        )
        .unwrap();
    assert_eq!(plain.last(), Some(&2), "the last record's delimiter");
    plain[..plain.len() - 1].to_vec()
}

#[test]
fn a_phone_reads_what_is_encrypted_to_it() {
    let rng = SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
    let public = private.compute_public_key().unwrap().as_ref().to_vec();
    let auth: [u8; 16] = ring::rand::generate(&rng).unwrap().expose();
    let subscription = Subscription {
        endpoint: "https://push.example.net/send/abc".into(),
        p256dh: public.clone(),
        auth: auth.to_vec(),
    };
    let body = encrypt(&subscription, b"{\"body\":\"hello\"}").unwrap();
    assert_eq!(
        open_as_phone(private, &public, &auth, &body),
        b"{\"body\":\"hello\"}"
    );
    // A new key and salt each time.
    assert_ne!(
        encrypt(&subscription, b"x").unwrap()[..86],
        encrypt(&subscription, b"x").unwrap()[..86]
    );
}

#[test]
fn vapid_tokens_are_es256_for_the_endpoint_s_origin() {
    let (vapid, pkcs8) = Vapid::generate().unwrap();
    assert_eq!(vapid.public().len(), 65);
    assert_eq!(Vapid::from_pkcs8(&pkcs8).unwrap().public(), vapid.public());
    let header = vapid
        .authorization(
            "https://push.example.net/send/abc?x=1",
            "https://monitor.example.com",
            1_700_000_000,
        )
        .unwrap();
    let rest = header.strip_prefix("vapid t=").unwrap();
    let (token, key) = rest.split_once(", k=").unwrap();
    assert_eq!(key, vapid.public_base64());
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let claims: Value = serde_json::from_slice(&b64(parts[1])).unwrap();
    assert_eq!(
        claims,
        json!({"aud": "https://push.example.net", "exp": 1_700_043_200, "sub": "https://monitor.example.com"})
    );
    let verifier = ring::signature::UnparsedPublicKey::new(
        &ring::signature::ECDSA_P256_SHA256_FIXED,
        vapid.public(),
    );
    verifier
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &b64(parts[2]),
        )
        .expect("a valid ES256 signature");
}

#[test]
fn subscriptions_are_checked() {
    let key = UA_PUBLIC;
    let ok = Subscription::parse("https://fcm.googleapis.com/fcm/send/x", key, AUTH).unwrap();
    assert_eq!(ok.host(), "fcm.googleapis.com");
    assert_eq!(ok.p256dh.len(), 65);
    // Padding is accepted, as some browsers add it.
    assert!(Subscription::parse("https://push.example.net/x", key, &format!("{AUTH}==")).is_ok());
    for (endpoint, p256dh, auth) in [
        ("http://push.example.net/x", key, AUTH),
        ("https://127.0.0.1/x", key, AUTH),
        ("https://[::1]/x", key, AUTH),
        ("https://10.0.0.4/x", key, AUTH),
        ("https://localhost/x", key, AUTH),
        ("https://intranet/x", key, AUTH),
        ("not a url", key, AUTH),
        ("https://push.example.net/x", "AAAA", AUTH),
        ("https://push.example.net/x", key, "AAAA"),
        ("https://push.example.net/x", "!!", AUTH),
    ] {
        assert!(
            Subscription::parse(endpoint, p256dh, auth).is_err(),
            "{endpoint} {p256dh} {auth}"
        );
    }
    let long = format!("https://push.example.net/{}", "a".repeat(MAX_ENDPOINT));
    assert!(Subscription::parse(&long, key, AUTH).is_err());
}

fn message(text: &str, payload: Value) -> Message {
    Message {
        subject: String::new(),
        text: text.into(),
        url: String::new(),
        body: String::new(),
        payload,
        lines: Vec::new(),
    }
}

#[test]
fn a_notice_names_one_incident_and_carries_no_secret() {
    let incident = Uuid::from_u128(7);
    let m = message(
        "Critical: cpu on web-1",
        json!({"id": "12", "incident": {"id": incident.to_string()},
               "acknowledge_url": "https://x/acknowledge/?token=secret"}),
    );
    let notice = Notice::from_message(&m, &[incident], &[incident]);
    assert_eq!(notice.path, format!("/mobile/incident/?id={incident}"));
    assert_eq!(notice.tag, format!("incident-{incident}"));
    assert!(notice.urgent);
    let json = notice.json();
    assert_eq!(json["actions"], json!(["acknowledge", "silence"]));
    assert_eq!(json["incident"], json!(incident));
    assert!(!json.to_string().contains("secret"));

    // Not an operator there: silence only.
    let notice = Notice::from_message(&m, &[incident], &[]);
    assert_eq!(notice.json()["actions"], json!(["silence"]));

    // A resolution: no action, not urgent, same tag so it replaces it.
    let notice = Notice::from_message(&m, &[], &[]);
    assert!(!notice.urgent);
    assert_eq!(notice.json()["actions"], json!([]));
    assert_eq!(notice.tag, format!("incident-{incident}"));
}

#[test]
fn a_group_opens_its_list_and_long_texts_are_cut() {
    let group = Uuid::from_u128(9);
    let m = message(
        &"x".repeat(500),
        json!({"id": "1", "group": {"id": group.to_string(), "rows": [1, 2]}}),
    );
    let notice = Notice::from_message(&m, &[Uuid::from_u128(1), Uuid::from_u128(2)], &[]);
    assert_eq!(notice.path, format!("/mobile/?group={group}"));
    assert_eq!(notice.incident, None);
    assert_eq!(notice.body.chars().count(), MAX_BODY);
    assert!(notice.body.ends_with('…'));
}

#[test]
fn a_burst_becomes_a_few_notifications() {
    let one = Notice::test();
    assert_eq!(limit(vec![one.clone(); 3]).len(), 3);
    let limited = limit(vec![one.clone(); 7]);
    assert_eq!(limited.len(), MAX_PER_PASS);
    assert_eq!(
        limited[2].body,
        "5 more notifications. Open MonitorFlow to see them."
    );
}

#[test]
fn the_subject_is_the_https_address_or_a_mailto() {
    assert_eq!(
        subject("https://monitor.example.com/x"),
        "https://monitor.example.com"
    );
    assert_eq!(
        subject("http://localhost:3000"),
        "mailto:push@monitorflow.invalid"
    );
}
