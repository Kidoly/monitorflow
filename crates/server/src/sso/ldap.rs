//! LDAP directories over TLS only (ADR-0145): LDAPS, or StartTLS before
//! anything is sent. A service account searches the person, the person's
//! own bind checks the password, and the service account reads the groups,
//! nested ones too when asked.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use ldap3::{
    ldap_escape, Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchOptions,
};
use monitorflow_common::redact::Secret;
use serde::Serialize;

use super::config::{LdapSecurity, LdapSettings};

/// How long connecting, and each operation, may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How deep nested groups are followed.
const MAX_DEPTH: usize = 10;
/// The most groups read for one person.
const MAX_GROUPS: usize = 500;
/// Longest username accepted.
const MAX_USERNAME: usize = 256;
/// Active Directory's capability OID in the root DSE.
const AD_CAPABILITY: &str = "1.2.840.113556.1.4.800";
/// `LDAP_MATCHING_RULE_IN_CHAIN`: Active Directory's nested membership.
const IN_CHAIN: &str = "1.2.840.113556.1.4.1941";
/// Result codes.
const SIZE_LIMIT_EXCEEDED: u32 = 4;
const NO_SUCH_OBJECT: u32 = 32;
const INVALID_CREDENTIALS: u32 = 49;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LdapError {
    #[error("Plain LDAP is refused: use ldaps://, or ldap:// with StartTLS")]
    Plain,
    #[error("Could not connect to the directory over TLS: {0}")]
    Connect(String),
    #[error("The directory refused the service account: {0}")]
    ServiceAccount(String),
    #[error("The base {0} does not exist in the directory")]
    NoBase(String),
    #[error("The directory search failed: {0}")]
    Search(String),
    #[error("No one with this username was found in the people base")]
    NotFound,
    #[error("Several entries match this username; make the people filter stricter")]
    Ambiguous,
    #[error("The directory refused this password")]
    BadPassword,
}

impl LdapError {
    /// Whether the person, not the directory, is at fault: an unknown name
    /// or a wrong password.
    pub fn is_credentials(&self) -> bool {
        matches!(self, Self::NotFound | Self::Ambiguous | Self::BadPassword)
    }
}

fn describe(error: &ldap3::LdapError) -> String {
    match error {
        ldap3::LdapError::LdapResult { result } => {
            let text = if result.text.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    result
                        .text
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(200)
                        .collect::<String>()
                )
            };
            format!("result code {}{text}", result.rc)
        }
        other => monitorflow_common::tls::error_chain(other),
    }
}

/// A person the directory knows, with the names of their groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub dn: String,
    pub groups: Vec<String>,
}

/// Opens a TLS connection: LDAPS, or StartTLS required before anything
/// else. `ldap://` without StartTLS is refused here too.
async fn connect(settings: &LdapSettings) -> Result<Ldap, LdapError> {
    let tls = super::tls::client_config(settings.ca_pem.as_deref())
        .map_err(|e| LdapError::Connect(e.to_string()))?;
    let starttls = match (
        settings.url.split_once("://").map(|(s, _)| s),
        settings.security(),
    ) {
        (Some("ldaps"), LdapSecurity::Ldaps) => false,
        (Some("ldap"), LdapSecurity::StartTls) => true,
        _ => return Err(LdapError::Plain),
    };
    let conn_settings = LdapConnSettings::new()
        .set_conn_timeout(TIMEOUT)
        .set_starttls(starttls)
        .set_config(tls);
    let (conn, mut ldap) = LdapConnAsync::with_settings(conn_settings, &settings.url)
        .await
        .map_err(|e| LdapError::Connect(describe(&e)))?;
    ldap3::drive!(conn);
    ldap.with_timeout(TIMEOUT);
    Ok(ldap)
}

async fn service_bind(
    ldap: &mut Ldap,
    settings: &LdapSettings,
    password: &Secret,
) -> Result<(), LdapError> {
    if password.expose().is_empty() {
        return Err(LdapError::ServiceAccount("enter its password".into()));
    }
    ldap.with_timeout(TIMEOUT);
    ldap.simple_bind(&settings.bind_dn, password.expose())
        .await
        .and_then(|r| r.success())
        .map_err(|e| LdapError::ServiceAccount(describe(&e)))?;
    Ok(())
}

async fn search(
    ldap: &mut Ldap,
    base: &str,
    scope: Scope,
    filter: &str,
    attrs: Vec<&str>,
    limit: i32,
) -> Result<(Vec<SearchEntry>, bool), LdapError> {
    ldap.with_timeout(TIMEOUT);
    ldap.with_search_options(
        SearchOptions::new()
            .sizelimit(limit)
            .timelimit(TIMEOUT.as_secs() as i32),
    );
    let result = ldap
        .search(base, scope, filter, attrs)
        .await
        .map_err(|e| LdapError::Search(describe(&e)))?;
    let truncated = match result.1.rc {
        0 => false,
        SIZE_LIMIT_EXCEEDED => true,
        NO_SUCH_OBJECT => return Err(LdapError::NoBase(base.to_owned())),
        _ => {
            return Err(LdapError::Search(describe(&ldap3::LdapError::LdapResult {
                result: result.1,
            })))
        }
    };
    let entries = result.0.into_iter().map(SearchEntry::construct).collect();
    Ok((entries, truncated))
}

/// What a typed username becomes in a filter, after its checks.
fn typed(username: &str) -> Result<&str, LdapError> {
    let username = username.trim();
    if username.is_empty()
        || username.len() > MAX_USERNAME
        || username.chars().any(char::is_control)
    {
        return Err(LdapError::NotFound);
    }
    Ok(username)
}

fn fill(filter: &str, dn: &str, username: &str) -> String {
    filter
        .replace("{dn}", &ldap_escape(dn))
        .replace("{username}", &ldap_escape(username))
}

/// The single entry of `username` under the people base.
async fn find(
    ldap: &mut Ldap,
    settings: &LdapSettings,
    username: &str,
) -> Result<String, LdapError> {
    let filter = fill(&settings.user_filter, "", username);
    let (entries, truncated) = search(
        ldap,
        &settings.user_base_dn,
        Scope::Subtree,
        &filter,
        vec!["1.1"],
        2,
    )
    .await?;
    match (entries.as_slice(), truncated) {
        ([one], false) => Ok(one.dn.clone()),
        ([], false) => Err(LdapError::NotFound),
        _ => Err(LdapError::Ambiguous),
    }
}

/// Whether the root DSE says the directory is Active Directory.
async fn is_active_directory(ldap: &mut Ldap) -> bool {
    match search(
        ldap,
        "",
        Scope::Base,
        "(objectClass=*)",
        vec!["supportedCapabilities"],
        1,
    )
    .await
    {
        Ok((entries, _)) => entries.iter().any(|e| {
            attribute(e, "supportedCapabilities")
                .iter()
                .any(|v| v == AD_CAPABILITY)
        }),
        Err(_) => false,
    }
}

/// The values of `name` in `entry`, whatever the case the server used.
fn attribute<'a>(entry: &'a SearchEntry, name: &str) -> &'a [String] {
    entry
        .attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map_or(&[], |(_, v)| v.as_slice())
}

/// The names of the groups of the entry `dn`.
async fn groups(
    ldap: &mut Ldap,
    settings: &LdapSettings,
    dn: &str,
    username: &str,
) -> Result<Vec<String>, LdapError> {
    let base = settings.group_base();
    let name_attr = settings.group_name_attribute.as_str();
    let limit = MAX_GROUPS as i32;
    let mut names: Vec<String> = Vec::new();
    let add = |entry: &SearchEntry, names: &mut Vec<String>| {
        if let Some(name) = attribute(entry, name_attr).first() {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                names.push(name.clone());
            }
        }
    };
    if settings.nested_groups && is_active_directory(ldap).await {
        let filter = format!("(member:{IN_CHAIN}:={})", ldap_escape(dn));
        let (entries, _) =
            search(ldap, base, Scope::Subtree, &filter, vec![name_attr], limit).await?;
        entries.iter().for_each(|e| add(e, &mut names));
        return Ok(names);
    }
    let filter = fill(&settings.group_filter, dn, username);
    let (direct, _) = search(ldap, base, Scope::Subtree, &filter, vec![name_attr], limit).await?;
    direct.iter().for_each(|e| add(e, &mut names));
    if !settings.nested_groups {
        return Ok(names);
    }
    // Groups of groups, breadth first, each group once.
    let mut seen: HashSet<String> = direct.iter().map(|e| e.dn.to_lowercase()).collect();
    let mut queue: VecDeque<(String, usize)> = direct.iter().map(|e| (e.dn.clone(), 1)).collect();
    while let Some((group, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH || seen.len() >= MAX_GROUPS {
            break;
        }
        let filter = format!(
            "(|(member={0})(uniqueMember={0}))",
            ldap_escape(group.as_str())
        );
        let (parents, _) =
            search(ldap, base, Scope::Subtree, &filter, vec![name_attr], limit).await?;
        for parent in parents {
            if seen.insert(parent.dn.to_lowercase()) {
                add(&parent, &mut names);
                queue.push_back((parent.dn.clone(), depth + 1));
            }
        }
    }
    Ok(names)
}

/// Signs `username` in with `password`: their entry and their groups.
pub async fn authenticate(
    settings: &LdapSettings,
    service_password: &Secret,
    username: &str,
    password: &Secret,
) -> Result<Person, LdapError> {
    let username = typed(username)?;
    // An empty password is an anonymous bind to many directories.
    if password.expose().is_empty() {
        return Err(LdapError::BadPassword);
    }
    let mut ldap = connect(settings).await?;
    let result = async {
        service_bind(&mut ldap, settings, service_password).await?;
        let dn = find(&mut ldap, settings, username).await?;
        ldap.with_timeout(TIMEOUT);
        match ldap.simple_bind(&dn, password.expose()).await {
            Ok(r) if r.rc == 0 => {}
            Ok(r) if r.rc == INVALID_CREDENTIALS => return Err(LdapError::BadPassword),
            Ok(r) => {
                return Err(LdapError::Search(describe(&ldap3::LdapError::LdapResult {
                    result: r,
                })))
            }
            Err(e) => return Err(LdapError::Search(describe(&e))),
        }
        // Back to the service account, which may read the groups.
        service_bind(&mut ldap, settings, service_password).await?;
        let groups = groups(&mut ldap, settings, &dn, username).await?;
        Ok(Person { dn, groups })
    }
    .await;
    let _ = ldap.unbind().await;
    result
}

/// What a passed connection test found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Found {
    /// Whether the root DSE says Active Directory.
    pub active_directory: bool,
    /// The entry of the person tried, when a username was given.
    pub dn: Option<String>,
    /// Their groups.
    pub groups: Vec<String>,
}

/// Checks a directory: TLS, the service account and the people base, and
/// with a username and password the whole sign-in.
pub async fn test(
    settings: &LdapSettings,
    service_password: &Secret,
    person: Option<(&str, &Secret)>,
) -> Result<Found, LdapError> {
    let mut ldap = connect(settings).await?;
    let result = async {
        service_bind(&mut ldap, settings, service_password).await?;
        let active_directory = is_active_directory(&mut ldap).await;
        search(
            &mut ldap,
            &settings.user_base_dn,
            Scope::Base,
            "(objectClass=*)",
            vec!["1.1"],
            1,
        )
        .await?;
        if let Some(base) = &settings.group_base_dn {
            search(
                &mut ldap,
                base,
                Scope::Base,
                "(objectClass=*)",
                vec!["1.1"],
                1,
            )
            .await?;
        }
        Ok(active_directory)
    }
    .await;
    let _ = ldap.unbind().await;
    let active_directory = result?;
    let Some((username, password)) = person else {
        return Ok(Found {
            active_directory,
            dn: None,
            groups: Vec::new(),
        });
    };
    let person = authenticate(settings, service_password, username, password).await?;
    Ok(Found {
        active_directory,
        dn: Some(person.dn),
        groups: person.groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_names_are_escaped_in_filters() {
        let filter = fill(
            "(&(uid={username})(member={dn}))",
            "cn=a,dc=x",
            "*)(uid=admin",
        );
        assert_eq!(filter, "(&(uid=\\2a\\29\\28uid=admin)(member=cn=a,dc=x))");
        assert_eq!(typed("  alice "), Ok("alice"));
        assert_eq!(typed(""), Err(LdapError::NotFound));
        assert_eq!(typed("a\0b"), Err(LdapError::NotFound));
    }

    #[tokio::test]
    async fn plain_ldap_is_never_opened() {
        let settings = LdapSettings {
            url: "ldap://127.0.0.1:1".into(),
            starttls: false,
            ca_pem: None,
            bind_dn: "cn=svc".into(),
            user_base_dn: "dc=x".into(),
            user_filter: "(uid={username})".into(),
            group_base_dn: None,
            group_filter: "(member={dn})".into(),
            group_name_attribute: "cn".into(),
            nested_groups: false,
        };
        let error = test(&settings, &Secret::new("p"), None).await.unwrap_err();
        assert_eq!(error, LdapError::Plain);
        let error = authenticate(&settings, &Secret::new("p"), "alice", &Secret::new("pw"))
            .await
            .unwrap_err();
        assert_eq!(error, LdapError::Plain);
    }

    #[tokio::test]
    async fn an_empty_password_never_binds() {
        let settings = LdapSettings {
            url: "ldaps://127.0.0.1:1".into(),
            starttls: false,
            ca_pem: None,
            bind_dn: "cn=svc".into(),
            user_base_dn: "dc=x".into(),
            user_filter: "(uid={username})".into(),
            group_base_dn: None,
            group_filter: "(member={dn})".into(),
            group_name_attribute: "cn".into(),
            nested_groups: false,
        };
        let error = authenticate(&settings, &Secret::new("p"), "alice", &Secret::new(""))
            .await
            .unwrap_err();
        assert_eq!(error, LdapError::BadPassword);
    }
}
