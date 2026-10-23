//! Administrator rules and remediation policies in PostgreSQL (ADR-0041):
//! `remediation_rules` and `remediation_policies`.
//!
//! The API writes them; the incident loop reads the [`RuleSet`] of a
//! server with [`rule_set_for_server`] and whether it may run low-impact
//! commands without approval with [`low_impact_auto`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::pattern::{Pattern, PatternError};
use super::{builtin, resolve, AllowRule, CacheDir, DenyRule, RuleSet, Tier};

/// Longest description of a rule, in characters.
pub const MAX_DESCRIPTION_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Allow,
    Deny,
    CacheDir,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::CacheDir => "cache_dir",
        }
    }
}

/// A row of `remediation_rules`.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredRule {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub site_id: Option<Uuid>,
    /// `allow`, `deny` or `cache_dir`.
    pub kind: String,
    pub pattern: String,
    /// `read_only` or `low_impact` for allow rules, else `null`.
    pub tier: Option<String>,
    pub description: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A rule to insert, checked by [`validate`].
#[derive(Debug, Clone)]
pub struct NewRule {
    pub customer_id: Uuid,
    pub site_id: Option<Uuid>,
    pub kind: Kind,
    pub pattern: String,
    pub tier: Option<Tier>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error(transparent)]
    Pattern(#[from] PatternError),
    #[error("allow rules need a tier of read_only or low_impact")]
    AllowTier,
    #[error("only allow rules have a tier")]
    UnexpectedTier,
    #[error("the first word of an allow rule must name a program, without wildcards")]
    AllowProgram,
    #[error("built-in rules decide `{0}`; allow rules only extend to unknown commands")]
    Reserved(String),
    #[error("cache_dir must be an absolute directory of two components or more, without wildcards, `.` or `..`, outside system directories")]
    CacheDir,
    #[error(
        "description is longer than {MAX_DESCRIPTION_LEN} characters or has control characters"
    )]
    Description,
}

/// Top-level directories a cache directory may not be under.
const SYSTEM_ROOTS: &[&str] = &[
    "proc", "sys", "dev", "run", "etc", "boot", "bin", "sbin", "lib", "lib32", "lib64", "usr",
];

/// Directories that are not caches, even though they pass the shape check.
const NOT_CACHES: &[&str] = &[
    "/var/lib",
    "/var/log",
    "/var/spool",
    "/var/mail",
    "/var/backups",
    "/var/db",
    "/var/opt",
    "/var/www",
    "/home/root",
];

/// Checks `rule` and returns its normalized pattern: single spaces between
/// words, no trailing slash on a directory.
pub fn validate(rule: &NewRule) -> Result<String, RuleError> {
    if rule.description.chars().count() > MAX_DESCRIPTION_LEN
        || rule.description.chars().any(char::is_control)
    {
        return Err(RuleError::Description);
    }
    match (rule.kind, rule.tier) {
        (Kind::Allow, Some(Tier::ReadOnly | Tier::LowImpact)) => {}
        (Kind::Allow, _) => return Err(RuleError::AllowTier),
        (_, Some(_)) => return Err(RuleError::UnexpectedTier),
        (_, None) => {}
    }
    match rule.kind {
        Kind::Deny => Ok(Pattern::parse(&rule.pattern)?.text()),
        Kind::Allow => {
            let pattern = Pattern::parse(&rule.pattern)?;
            let program = pattern.program();
            if program.contains(['*', '?']) {
                return Err(RuleError::AllowProgram);
            }
            let name = resolve(program);
            if builtin::is_reserved(&name) {
                return Err(RuleError::Reserved(name));
            }
            Ok(pattern.text())
        }
        Kind::CacheDir => cache_dir(&rule.pattern).ok_or(RuleError::CacheDir),
    }
}

fn cache_dir(path: &str) -> Option<String> {
    let path = path.strip_suffix('/').unwrap_or(path);
    let rest = path.strip_prefix('/')?;
    let parts: Vec<&str> = rest.split('/').collect();
    let valid = parts.len() >= 2
        && path.len() <= 512
        && path
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"*?[]\\".contains(&b))
        && parts
            .iter()
            .all(|p| !p.is_empty() && *p != "." && *p != "..")
        && !SYSTEM_ROOTS.contains(&parts[0])
        && !NOT_CACHES.contains(&path);
    valid.then(|| path.to_owned())
}

const COLUMNS: &str =
    "id, customer_id, site_id, kind, pattern, tier, description, created_by, created_at";

/// Rules of the customers in `customers` (`(all, ids)` of a scope), by
/// customer, site (customer-wide first), kind and pattern.
pub async fn list(
    db: impl PgExecutor<'_>,
    customers: &(bool, Vec<Uuid>),
    customer_id: Option<Uuid>,
    site_id: Option<Uuid>,
) -> Result<Vec<StoredRule>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM remediation_rules
         WHERE ($1 OR customer_id = ANY($2))
           AND ($3::uuid IS NULL OR customer_id = $3)
           AND ($4::uuid IS NULL OR site_id = $4)
         ORDER BY customer_id, site_id NULLS FIRST, kind, pattern"
    ))
    .bind(customers.0)
    .bind(&customers.1)
    .bind(customer_id)
    .bind(site_id)
    .fetch_all(db)
    .await
}

pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<StoredRule>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM remediation_rules WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Inserts `rule` with its normalized `pattern` from [`validate`].
pub async fn insert(
    db: impl PgExecutor<'_>,
    rule: &NewRule,
    pattern: &str,
    created_by: Option<Uuid>,
) -> Result<StoredRule, sqlx::Error> {
    sqlx::query_as(&format!(
        "INSERT INTO remediation_rules
             (customer_id, site_id, kind, pattern, tier, description, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING {COLUMNS}"
    ))
    .bind(rule.customer_id)
    .bind(rule.site_id)
    .bind(rule.kind.as_str())
    .bind(pattern)
    .bind(rule.tier.map(Tier::as_str))
    .bind(&rule.description)
    .bind(created_by)
    .fetch_one(db)
    .await
}

/// Deletes the rule `id`; whether it existed.
pub async fn delete(db: impl PgExecutor<'_>, id: Uuid) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM remediation_rules WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// The rules in force for servers of `site` (customer-wide and the site's
/// own), or of the whole customer when `site` is `None`. Rows that no
/// longer validate are skipped with a warning; a skipped deny rule is
/// logged as an error, since it no longer protects anything.
pub async fn rule_set(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    site: Option<Uuid>,
) -> Result<RuleSet, sqlx::Error> {
    let rows: Vec<StoredRule> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM remediation_rules
         WHERE customer_id = $1 AND (site_id IS NULL OR site_id = $2)
         ORDER BY created_at, id"
    ))
    .bind(customer)
    .bind(site)
    .fetch_all(db)
    .await?;
    let mut set = RuleSet::default();
    for row in rows {
        let tier = row.tier.as_deref().and_then(Tier::parse);
        match (row.kind.as_str(), Pattern::parse(&row.pattern)) {
            ("deny", Ok(pattern)) => set.deny.push(DenyRule {
                id: row.id,
                pattern,
            }),
            ("allow", Ok(pattern)) if tier.is_some_and(|t| t < Tier::HighImpact) => {
                set.allow.push(AllowRule {
                    id: row.id,
                    pattern,
                    tier: tier.unwrap_or(Tier::HighImpact),
                })
            }
            ("cache_dir", _) if cache_dir(&row.pattern).is_some() => {
                set.cache_dirs.push(CacheDir {
                    id: row.id,
                    path: row.pattern,
                })
            }
            ("deny", _) => tracing::error!(rule = %row.id, "invalid deny rule skipped"),
            _ => tracing::warn!(rule = %row.id, "invalid remediation rule skipped"),
        }
    }
    Ok(set)
}

/// The customer and site of the server `server`, if it exists.
pub async fn server_scope(
    db: impl PgExecutor<'_>,
    server: Uuid,
) -> Result<Option<(Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT st.customer_id, st.id FROM servers s JOIN sites st ON st.id = s.site_id
         WHERE s.id = $1",
    )
    .bind(server)
    .fetch_optional(db)
    .await
}

/// [`rule_set`] for the site of `server`; `None` for an unknown server.
pub async fn rule_set_for_server(
    db: &sqlx::PgPool,
    server: Uuid,
) -> Result<Option<RuleSet>, sqlx::Error> {
    match server_scope(db, server).await? {
        Some((customer, site)) => Ok(Some(rule_set(db, customer, Some(site)).await?)),
        None => Ok(None),
    }
}

/// A row of `remediation_policies`.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Policy {
    pub customer_id: Uuid,
    pub site_id: Option<Uuid>,
    pub low_impact_auto: bool,
    pub updated_by: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
}

/// Whether low-impact commands run without approval on servers of
/// `site`: the site's row, else the customer's, else `false`.
pub async fn low_impact_auto(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    site: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let auto: Option<bool> = sqlx::query_scalar(
        "SELECT low_impact_auto FROM remediation_policies
         WHERE customer_id = $1 AND (site_id IS NULL OR site_id = $2)
         ORDER BY site_id NULLS LAST LIMIT 1",
    )
    .bind(customer)
    .bind(site)
    .fetch_optional(db)
    .await?;
    Ok(auto.unwrap_or(false))
}

/// Policies of the customers in `customers`, customer rows first.
pub async fn policies(
    db: impl PgExecutor<'_>,
    customers: &(bool, Vec<Uuid>),
    customer_id: Option<Uuid>,
) -> Result<Vec<Policy>, sqlx::Error> {
    sqlx::query_as(
        "SELECT customer_id, site_id, low_impact_auto, updated_by, updated_at
         FROM remediation_policies
         WHERE ($1 OR customer_id = ANY($2)) AND ($3::uuid IS NULL OR customer_id = $3)
         ORDER BY customer_id, site_id NULLS FIRST",
    )
    .bind(customers.0)
    .bind(&customers.1)
    .bind(customer_id)
    .fetch_all(db)
    .await
}

/// Creates or replaces the policy of `customer`, or of one of its sites.
pub async fn set_policy(
    db: impl PgExecutor<'_>,
    customer: Uuid,
    site: Option<Uuid>,
    low_impact_auto: bool,
    updated_by: Option<Uuid>,
) -> Result<Policy, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO remediation_policies (customer_id, site_id, low_impact_auto, updated_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT ON CONSTRAINT remediation_policies_key DO UPDATE
             SET low_impact_auto = EXCLUDED.low_impact_auto,
                 updated_by = EXCLUDED.updated_by,
                 updated_at = now()
         RETURNING customer_id, site_id, low_impact_auto, updated_by, updated_at",
    )
    .bind(customer)
    .bind(site)
    .bind(low_impact_auto)
    .bind(updated_by)
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: Kind, pattern: &str, tier: Option<Tier>) -> NewRule {
        NewRule {
            customer_id: Uuid::nil(),
            site_id: None,
            kind,
            pattern: pattern.into(),
            tier,
            description: String::new(),
        }
    }

    #[test]
    fn validation_normalizes_and_refuses_unsafe_rules() {
        use Kind::*;
        let ro = Some(Tier::ReadOnly);
        let low = Some(Tier::LowImpact);
        for (new, expected) in [
            (rule(Deny, " rm  -rf / ", None), Ok("rm -rf /".to_owned())),
            (
                rule(Deny, "", None),
                Err(RuleError::Pattern(PatternError::Empty)),
            ),
            (rule(Deny, "x", ro), Err(RuleError::UnexpectedTier)),
            (rule(Allow, "nginx -t", ro), Ok("nginx -t".to_owned())),
            (
                rule(Allow, "/opt/app/bin/check **", low),
                Ok("/opt/app/bin/check **".to_owned()),
            ),
            (rule(Allow, "nginx -t", None), Err(RuleError::AllowTier)),
            (
                rule(Allow, "nginx -t", Some(Tier::HighImpact)),
                Err(RuleError::AllowTier),
            ),
            (
                rule(Allow, "nginx -t", Some(Tier::Forbidden)),
                Err(RuleError::AllowTier),
            ),
            (rule(Allow, "** -t", ro), Err(RuleError::AllowProgram)),
            (rule(Allow, "ngin* -t", ro), Err(RuleError::AllowProgram)),
            (
                rule(Allow, "bash -c *", ro),
                Err(RuleError::Reserved("bash".into())),
            ),
            (
                rule(Allow, "/usr/bin/sudo **", ro),
                Err(RuleError::Reserved("sudo".into())),
            ),
            (
                rule(Allow, "systemctl stop *", low),
                Err(RuleError::Reserved("systemctl".into())),
            ),
            (
                rule(Allow, "apt-get install *", low),
                Err(RuleError::Reserved("apt-get".into())),
            ),
            (
                rule(CacheDir, "/var/cache/nginx/", None),
                Ok("/var/cache/nginx".to_owned()),
            ),
            (
                rule(CacheDir, "/home/app/.cache", None),
                Ok("/home/app/.cache".to_owned()),
            ),
            (
                rule(CacheDir, "/var/cache/nginx", low),
                Err(RuleError::UnexpectedTier),
            ),
            (rule(CacheDir, "/", None), Err(RuleError::CacheDir)),
            (rule(CacheDir, "/tmp", None), Err(RuleError::CacheDir)),
            (rule(CacheDir, "/var/lib", None), Err(RuleError::CacheDir)),
            (rule(CacheDir, "/etc/nginx", None), Err(RuleError::CacheDir)),
            (
                rule(CacheDir, "/usr/share/x", None),
                Err(RuleError::CacheDir),
            ),
            (
                rule(CacheDir, "var/cache/x", None),
                Err(RuleError::CacheDir),
            ),
            (
                rule(CacheDir, "/var/cache/../lib", None),
                Err(RuleError::CacheDir),
            ),
            (
                rule(CacheDir, "/var/cache/*", None),
                Err(RuleError::CacheDir),
            ),
            (
                rule(CacheDir, "/var//cache", None),
                Err(RuleError::CacheDir),
            ),
            (
                rule(CacheDir, "/var/cache/a b", None),
                Err(RuleError::CacheDir),
            ),
        ] {
            assert_eq!(validate(&new), expected, "{new:?}");
        }
        let mut long = rule(Deny, "x", None);
        long.description = "d".repeat(257);
        assert_eq!(validate(&long), Err(RuleError::Description));
        long.description = "line\nbreak".into();
        assert_eq!(validate(&long), Err(RuleError::Description));
    }
}
