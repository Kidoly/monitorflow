use serde_json::json;

use super::*;

fn oidc(issuer: &str) -> Settings {
    serde_json::from_value(json!({ "kind": "oidc", "issuer": issuer, "client_id": " mf " }))
        .unwrap()
}

fn ldap(url: &str, starttls: bool) -> Settings {
    serde_json::from_value(json!({
        "kind": "ldap", "url": url, "starttls": starttls,
        "bind_dn": "cn=svc,dc=example,dc=org", "user_base_dn": "ou=people,dc=example,dc=org",
    }))
    .unwrap()
}

fn refused(settings: Settings) -> String {
    settings.checked().unwrap_err().0
}

#[test]
fn oidc_defaults_fill_in() {
    let Settings::Oidc(s) = oidc("https://id.example.com/realms/mf").checked().unwrap() else {
        panic!("kind changed")
    };
    assert_eq!(s.client_id, "mf");
    assert_eq!(s.scopes, DEFAULT_SCOPES);
    assert_eq!(s.groups_claim, "groups");
    assert_eq!(s.ca_pem, None);
}

#[test]
fn oidc_needs_https() {
    assert!(refused(oidc("http://id.example.com")).contains("https://"));
    assert!(refused(oidc("id.example.com")).contains("not a URL"));
    assert!(refused(oidc("https://id.example.com/?a=b")).contains("without ?"));
    assert!(refused(oidc("")).contains("Enter the issuer"));
}

#[test]
fn oidc_scopes_need_openid() {
    let mut settings = oidc("https://id.example.com");
    if let Settings::Oidc(s) = &mut settings {
        s.scopes = "profile email".into();
    }
    assert!(refused(settings).contains("openid"));
    let mut settings = oidc("https://id.example.com");
    if let Settings::Oidc(s) = &mut settings {
        s.scopes = "openid  a\"b".into();
    }
    assert!(refused(settings).contains("scope"));
}

#[test]
fn unknown_fields_are_refused() {
    let bad = json!({ "kind": "oidc", "issuer": "https://a", "client_id": "x", "secret": "s" });
    assert!(serde_json::from_value::<Settings>(bad).is_err());
    let bad = json!({ "kind": "saml", "issuer": "https://a" });
    assert!(serde_json::from_value::<Settings>(bad).is_err());
}

#[test]
fn ldap_is_tls_only() {
    assert!(ldap("ldaps://ldap.example.com", false).checked().is_ok());
    assert!(ldap("ldaps://ldap.example.com:1636/", false)
        .checked()
        .is_ok());
    assert!(ldap("ldap://ldap.example.com", true).checked().is_ok());
    assert!(refused(ldap("ldap://ldap.example.com", false)).contains("Plain LDAP is refused"));
    assert!(refused(ldap("ldaps://ldap.example.com", true)).contains("StartTLS is for ldap://"));
    assert!(refused(ldap("http://ldap.example.com", false)).contains("ldaps://"));
    assert!(refused(ldap("ldaps://ldap.example.com/dc=x", false)).contains("nothing more"));
    assert!(refused(ldap("ldaps://u@ldap.example.com", false)).contains("nothing more"));
}

#[test]
fn ldap_defaults_and_filters() {
    let Settings::Ldap(s) = ldap("ldaps://ldap.example.com/", false).checked().unwrap() else {
        panic!("kind changed")
    };
    assert_eq!(s.url, "ldaps://ldap.example.com");
    assert_eq!(s.user_filter, DEFAULT_USER_FILTER);
    assert_eq!(s.group_filter, DEFAULT_GROUP_FILTER);
    assert_eq!(s.group_base(), "ou=people,dc=example,dc=org");
    assert_eq!(s.security(), LdapSecurity::Ldaps);

    let with = |user: &str, group: &str| {
        let mut settings = ldap("ldaps://ldap.example.com", false);
        if let Settings::Ldap(s) = &mut settings {
            s.user_filter = user.into();
            s.group_filter = group.into();
        }
        settings
    };
    assert!(refused(with("(uid=alice)", "")).contains("{username}"));
    assert!(refused(with("uid={username}", "")).contains("parentheses"));
    assert!(refused(with("(uid={username}))(", "")).contains("parentheses"));
    assert!(refused(with("", "(member=x)")).contains("{dn} or {username}"));
    assert!(with("(cn={username})", "(memberUid={username})")
        .checked()
        .is_ok());
}

#[test]
fn names_and_secrets() {
    assert_eq!(name("  Keycloak ").unwrap(), "Keycloak");
    assert!(name("").is_err());
    assert!(name(&"x".repeat(65)).is_err());
    assert!(name("a\nb").is_err());
    assert_eq!(secret(None).unwrap(), None);
    assert_eq!(secret(Some("")).unwrap(), None);
    assert_eq!(secret(Some(" s ")).unwrap().as_deref(), Some(" s "));
    assert!(secret(Some(&"x".repeat(MAX_SECRET + 1))).is_err());
}

#[test]
fn the_address_keeps_a_secret_where_it_was_entered() {
    assert_eq!(
        oidc("https://id.example.com/").address(),
        "https://id.example.com"
    );
    assert_eq!(ldap("ldaps://a", false).address(), "ldaps://a");
    assert!(ldap("ldaps://a", false).needs_secret());
    assert!(!oidc("https://a").needs_secret());
}

#[test]
fn kinds_come_from_the_column() {
    assert_eq!(Kind::of_column("ldap"), Kind::Ldap);
    assert_eq!(Kind::of_column("oidc"), Kind::Oidc);
    for kind in [Kind::Oidc, Kind::Ldap] {
        assert_eq!(Kind::of_column(kind.as_str()), kind);
    }
}
