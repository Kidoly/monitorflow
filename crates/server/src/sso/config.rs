//! The settings of a sign-in provider and their checks (ADR-0145). What
//! is stored in `identity_providers.config`; the secret is apart.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Longest name, as the column allows.
pub const MAX_NAME: usize = 64;
/// Longest address, ID, DN or filter.
const MAX_TEXT: usize = 1024;
/// Longest pasted certificate authority.
const MAX_CA: usize = 64 * 1024;
/// Longest secret.
pub const MAX_SECRET: usize = 1024;

pub const DEFAULT_SCOPES: &str = "openid profile email groups";
pub const DEFAULT_GROUPS_CLAIM: &str = "groups";
pub const DEFAULT_USER_FILTER: &str = "(|(uid={username})(sAMAccountName={username}))";
pub const DEFAULT_GROUP_FILTER: &str = "(|(member={dn})(uniqueMember={dn})(memberUid={username}))";
pub const DEFAULT_GROUP_NAME_ATTRIBUTE: &str = "cn";

/// What a provider is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An OpenID Connect provider.
    Oidc,
    /// An LDAP directory.
    Ldap,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Oidc => "oidc",
            Self::Ldap => "ldap",
        }
    }

    /// The kind of `identity_providers.kind`: `ldap`, else OpenID Connect.
    pub fn of_column(kind: &str) -> Self {
        if kind == "ldap" {
            Self::Ldap
        } else {
            Self::Oidc
        }
    }
}

/// The settings of a provider, by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[schema(as = IdentityProviderSettings)]
pub enum Settings {
    Oidc(OidcSettings),
    Ldap(LdapSettings),
}

/// An OpenID Connect provider. The client secret is apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OidcSettings {
    /// The provider's issuer, `https`: its discovery document is at
    /// `<issuer>/.well-known/openid-configuration`.
    pub issuer: String,
    /// The client ID the provider gave MonitorFlow.
    pub client_id: String,
    /// Scopes asked for, separated by spaces; `openid` is required. A
    /// scope the discovery document does not list is left out at sign-in.
    #[serde(default = "default_scopes")]
    pub scopes: String,
    /// The claim of the ID token, or of userinfo, that lists the groups.
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    /// A certificate authority in PEM to trust besides the public ones, for
    /// a provider with a private certificate.
    #[serde(default)]
    pub ca_pem: Option<String>,
}

/// An LDAP directory, over LDAPS or StartTLS only. The service account's
/// password is apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LdapSettings {
    /// `ldaps://host[:port]`, or `ldap://host[:port]` with `starttls`.
    pub url: String,
    /// Upgrade an `ldap://` connection with StartTLS before anything is
    /// sent; required for `ldap://`.
    #[serde(default)]
    pub starttls: bool,
    /// A certificate authority in PEM to trust besides the public ones.
    #[serde(default)]
    pub ca_pem: Option<String>,
    /// The service account that searches people.
    pub bind_dn: String,
    /// Where people are searched.
    pub user_base_dn: String,
    /// Finds one person; `{username}` is what they typed, escaped.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    /// Where groups are searched; absent or empty is `user_base_dn`.
    #[serde(default)]
    pub group_base_dn: Option<String>,
    /// Finds the person's groups; `{dn}` is their entry, `{username}`
    /// what they typed, both escaped.
    #[serde(default = "default_group_filter")]
    pub group_filter: String,
    /// The attribute that names a group in "Groups and roles".
    #[serde(default = "default_group_name_attribute")]
    pub group_name_attribute: String,
    /// Count the groups of the person's groups too.
    #[serde(default)]
    pub nested_groups: bool,
}

fn default_scopes() -> String {
    DEFAULT_SCOPES.into()
}
fn default_groups_claim() -> String {
    DEFAULT_GROUPS_CLAIM.into()
}
fn default_user_filter() -> String {
    DEFAULT_USER_FILTER.into()
}
fn default_group_filter() -> String {
    DEFAULT_GROUP_FILTER.into()
}
fn default_group_name_attribute() -> String {
    DEFAULT_GROUP_NAME_ATTRIBUTE.into()
}

/// Why settings are refused, in words to show.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

fn invalid(text: impl Into<String>) -> Invalid {
    Invalid(text.into())
}

/// `text` trimmed, refused when empty, too long or with control
/// characters.
fn required(text: &str, what: &str) -> Result<String, Invalid> {
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid(format!("Enter the {what}.")));
    }
    if text.len() > MAX_TEXT || text.chars().any(char::is_control) {
        return Err(invalid(format!("The {what} is not valid.")));
    }
    Ok(text.to_owned())
}

/// A provider's name, trimmed.
pub fn name(name: &str) -> Result<String, Invalid> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("Enter a name, shown on the sign-in button."));
    }
    if name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(invalid(format!(
            "The name must be at most {MAX_NAME} characters, without control characters."
        )));
    }
    Ok(name.to_owned())
}

/// A pasted certificate authority: `None` when empty, else at least one
/// PEM certificate that parses.
fn ca(pem: Option<&str>) -> Result<Option<String>, Invalid> {
    let Some(pem) = pem.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if pem.len() > MAX_CA {
        return Err(invalid("The certificate authority is too long."));
    }
    super::tls::parse_ca(pem).map_err(|e| invalid(e.to_string()))?;
    Ok(Some(pem.to_owned()))
}

impl Settings {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Oidc(_) => Kind::Oidc,
            Self::Ldap(_) => Kind::Ldap,
        }
    }

    /// The settings checked and tidied: trimmed, defaults for empty
    /// optional fields.
    pub fn checked(self) -> Result<Self, Invalid> {
        match self {
            Self::Oidc(s) => s.checked().map(Self::Oidc),
            Self::Ldap(s) => s.checked().map(Self::Ldap),
        }
    }

    /// Where the secret is sent: a stored secret is used only while this
    /// stays the same (ADR-0145, as the AI key of ADR-0096).
    pub fn address(&self) -> &str {
        match self {
            Self::Oidc(s) => s.issuer.trim_end_matches('/'),
            Self::Ldap(s) => &s.url,
        }
    }

    /// Whether this kind needs a secret to work.
    pub fn needs_secret(&self) -> bool {
        matches!(self, Self::Ldap(_))
    }
}

impl OidcSettings {
    fn checked(self) -> Result<Self, Invalid> {
        let issuer = required(&self.issuer, "issuer address")?;
        let url = Url::parse(&issuer).map_err(|_| invalid("The issuer address is not a URL."))?;
        if url.scheme() != "https" {
            return Err(invalid(
                "The issuer address must start with https://: MonitorFlow never signs in over plain HTTP.",
            ));
        }
        if url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
            return Err(invalid(
                "The issuer address must be the provider's address, without ? or #.",
            ));
        }
        let client_id = required(&self.client_id, "client ID")?;
        let scopes = match self.scopes.split_whitespace().collect::<Vec<_>>() {
            s if s.is_empty() => DEFAULT_SCOPES.to_owned(),
            s => {
                let valid = s.iter().all(|scope| {
                    scope.len() <= 128
                        && scope
                            .bytes()
                            .all(|b| b == 0x21 || ((0x23..=0x7e).contains(&b) && b != b'\\'))
                });
                if !valid {
                    return Err(invalid("A scope is not valid."));
                }
                if !s.contains(&"openid") {
                    return Err(invalid("The scopes must include openid."));
                }
                s.join(" ")
            }
        };
        let groups_claim = match self.groups_claim.trim() {
            "" => DEFAULT_GROUPS_CLAIM.to_owned(),
            claim => required(claim, "groups claim")?,
        };
        Ok(Self {
            issuer,
            client_id,
            scopes,
            groups_claim,
            ca_pem: ca(self.ca_pem.as_deref())?,
        })
    }
}

/// How an LDAP connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LdapSecurity {
    Ldaps,
    StartTls,
}

impl LdapSettings {
    fn checked(self) -> Result<Self, Invalid> {
        let url = required(&self.url, "directory address")?;
        let parsed = Url::parse(&url).map_err(|_| {
            invalid("The directory address must look like ldaps://ldap.example.com.")
        })?;
        match (parsed.scheme(), self.starttls) {
            ("ldaps", false) | ("ldap", true) => {}
            ("ldaps", true) => {
                return Err(invalid(
                    "StartTLS is for ldap:// addresses; ldaps:// is already encrypted.",
                ))
            }
            ("ldap", false) => {
                return Err(invalid(
                    "Plain LDAP is refused: use ldaps://, or ldap:// with StartTLS.",
                ))
            }
            _ => {
                return Err(invalid(
                    "The directory address must start with ldaps:// or ldap://.",
                ))
            }
        }
        let path_empty = matches!(parsed.path(), "" | "/");
        if parsed.host_str().is_none_or(str::is_empty)
            || !path_empty
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.username().is_empty()
        {
            return Err(invalid(
                "The directory address must be ldaps://host or ldaps://host:port, nothing more.",
            ));
        }
        let url = url.trim_end_matches('/').to_owned();
        let user_filter = match self.user_filter.trim() {
            "" => DEFAULT_USER_FILTER.to_owned(),
            f => filter(f, "people filter", &["{username}"])?,
        };
        let group_filter = match self.group_filter.trim() {
            "" => DEFAULT_GROUP_FILTER.to_owned(),
            f => filter(f, "groups filter", &["{dn}", "{username}"])?,
        };
        let group_name_attribute = match self.group_name_attribute.trim() {
            "" => DEFAULT_GROUP_NAME_ATTRIBUTE.to_owned(),
            a if a.len() <= 64
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') =>
            {
                a.to_owned()
            }
            _ => return Err(invalid("The group name attribute is not valid.")),
        };
        let group_base_dn = match self.group_base_dn.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(dn) => Some(required(dn, "groups base")?),
        };
        Ok(Self {
            url,
            starttls: self.starttls,
            ca_pem: ca(self.ca_pem.as_deref())?,
            bind_dn: required(&self.bind_dn, "service account")?,
            user_base_dn: required(&self.user_base_dn, "people base")?,
            user_filter,
            group_base_dn,
            group_filter,
            group_name_attribute,
            nested_groups: self.nested_groups,
        })
    }

    pub fn security(&self) -> LdapSecurity {
        if self.starttls {
            LdapSecurity::StartTls
        } else {
            LdapSecurity::Ldaps
        }
    }

    /// Where groups are searched.
    pub fn group_base(&self) -> &str {
        self.group_base_dn.as_deref().unwrap_or(&self.user_base_dn)
    }
}

/// A search filter in parentheses with one of `placeholders`.
fn filter(text: &str, what: &str, placeholders: &[&str]) -> Result<String, Invalid> {
    let text = required(text, what)?;
    let balanced = text.starts_with('(')
        && text.ends_with(')')
        && text.chars().try_fold(0i32, |depth, c| {
            let depth = depth + i32::from(c == '(') - i32::from(c == ')');
            (depth >= 0).then_some(depth)
        }) == Some(0);
    if !balanced {
        return Err(invalid(format!(
            "The {what} must be an LDAP filter in parentheses."
        )));
    }
    if !placeholders.iter().any(|p| text.contains(p)) {
        return Err(invalid(format!(
            "The {what} must contain {}.",
            placeholders.join(" or ")
        )));
    }
    Ok(text)
}

/// A typed secret: `None` when absent or empty, which keeps the stored one.
pub fn secret(secret: Option<&str>) -> Result<Option<String>, Invalid> {
    let Some(secret) = secret.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if secret.len() > MAX_SECRET {
        return Err(invalid(format!(
            "The secret is longer than {MAX_SECRET} bytes."
        )));
    }
    Ok(Some(secret.to_owned()))
}

#[cfg(test)]
mod tests;
