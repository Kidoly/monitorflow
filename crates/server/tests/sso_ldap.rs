//! LDAP sign-in against a real directory (ADR-0145): LDAPS and StartTLS
//! with the directory's own certificate authority, nested groups, wrong
//! passwords, and every way of falling back to plain text refused.
//! `deploy/sso-test.sh` starts OpenLDAP and runs these with `--ignored`,
//! passing the directory in `MF_TEST_LDAP_*`.

use monitorflow_common::redact::Secret;
use monitorflow_server::sso::config::LdapSettings;
use monitorflow_server::sso::ldap::{self, LdapError};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: run deploy/sso-test.sh"))
}

fn settings(url: &str, starttls: bool) -> LdapSettings {
    LdapSettings {
        url: url.to_owned(),
        starttls,
        ca_pem: Some(std::fs::read_to_string(env("MF_TEST_LDAP_CA")).unwrap()),
        bind_dn: "cn=monitorflow,dc=example,dc=org".into(),
        user_base_dn: "ou=people,dc=example,dc=org".into(),
        user_filter: "(|(uid={username})(sAMAccountName={username}))".into(),
        group_base_dn: Some("ou=groups,dc=example,dc=org".into()),
        group_filter: "(|(member={dn})(uniqueMember={dn})(memberUid={username}))".into(),
        group_name_attribute: "cn".into(),
        nested_groups: true,
    }
}

fn ldaps() -> LdapSettings {
    settings(&env("MF_TEST_LDAPS_URL"), false)
}

fn starttls() -> LdapSettings {
    settings(&env("MF_TEST_LDAP_URL"), true)
}

fn service() -> Secret {
    Secret::new(env("MF_TEST_LDAP_SERVICE_PASSWORD"))
}

#[tokio::test]
#[ignore = "needs the directory of deploy/sso-test.sh"]
async fn ldaps_and_starttls_sign_in_with_nested_groups() {
    for settings in [ldaps(), starttls()] {
        let person = ldap::authenticate(
            &settings,
            &service(),
            "erin",
            &Secret::new("erin-directory-pw"),
        )
        .await
        .unwrap();
        assert_eq!(person.dn, "uid=erin,ou=people,dc=example,dc=org");
        assert_eq!(person.groups, ["mf-operators"]);
        let found = ldap::test(&settings, &service(), None).await.unwrap();
        assert!(!found.active_directory);
    }
    let mut flat = ldaps();
    flat.nested_groups = false;
    let groups = |s: LdapSettings| async move {
        ldap::authenticate(&s, &service(), "dave", &Secret::new("dave-directory-pw"))
            .await
            .map(|p| p.groups)
    };
    // dave is in helpdesk, which is in mf-viewers.
    let mut nested = groups(ldaps()).await.unwrap();
    nested.sort();
    assert_eq!(nested, ["helpdesk", "mf-viewers"]);
    assert_eq!(groups(flat).await.unwrap(), ["helpdesk"]);
}

#[tokio::test]
#[ignore = "needs the directory of deploy/sso-test.sh"]
async fn wrong_people_and_passwords_are_refused() {
    let s = ldaps();
    let wrong = ldap::authenticate(&s, &service(), "erin", &Secret::new("nope")).await;
    assert_eq!(wrong.unwrap_err(), LdapError::BadPassword);
    let empty = ldap::authenticate(&s, &service(), "erin", &Secret::new("")).await;
    assert_eq!(empty.unwrap_err(), LdapError::BadPassword);
    let unknown = ldap::authenticate(&s, &service(), "nobody", &Secret::new("x")).await;
    assert_eq!(unknown.unwrap_err(), LdapError::NotFound);
    // A filter injected through the username finds nobody.
    let injected = ldap::authenticate(&s, &service(), "*", &Secret::new("x")).await;
    assert_eq!(injected.unwrap_err(), LdapError::NotFound);
    let bad_service = ldap::test(&s, &Secret::new("wrong"), None).await;
    assert!(matches!(bad_service, Err(LdapError::ServiceAccount(_))));
}

#[tokio::test]
#[ignore = "needs the directory of deploy/sso-test.sh"]
async fn never_without_verified_tls() {
    // ldap:// without StartTLS is never opened.
    let plain = settings(&env("MF_TEST_LDAP_URL"), false);
    assert_eq!(
        ldap::test(&plain, &service(), None).await.unwrap_err(),
        LdapError::Plain
    );
    // Another certificate authority: the directory's certificate is refused.
    let mut untrusted = ldaps();
    untrusted.ca_pem = None;
    assert!(matches!(
        ldap::test(&untrusted, &service(), None).await,
        Err(LdapError::Connect(_))
    ));
    let mut untrusted = starttls();
    untrusted.ca_pem = None;
    assert!(matches!(
        ldap::test(&untrusted, &service(), None).await,
        Err(LdapError::Connect(_))
    ));
    // LDAPS to the plain port: no fallback.
    let wrong_port = settings(
        &env("MF_TEST_LDAP_URL").replace("ldap://", "ldaps://"),
        false,
    );
    assert!(matches!(
        ldap::test(&wrong_port, &service(), None).await,
        Err(LdapError::Connect(_))
    ));
}
