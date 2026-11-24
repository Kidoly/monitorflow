//! Searching the inventory of the whole fleet (ADR-0187): "which servers
//! have OpenSSL below 3.0.15", "which disk models do we have". Software
//! is searched across the three places it is kept: OS packages and
//! Windows updates (`server_packages`), third-party apps
//! (`server_third_party_packages`) and Windows programs (inventory items
//! of kind `software`). Text matches in SQL; versions are compared in
//! Rust with [`super::version::compare`], since no SQL order fits them.

use std::cmp::Ordering;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::version;
use crate::api::access::Scope;

/// What can be searched: `software`, or one of the item kinds.
pub const KINDS: [&str; 9] = [
    "software",
    "system",
    "processor",
    "memory",
    "disk",
    "nic",
    "service",
    "feature",
    "licence",
];

/// Rows read from the database at most, before versions are compared.
pub const MAX_SCANNED: i64 = 100_000;

/// One search.
#[derive(Debug, Clone)]
pub struct Query {
    /// One of [`KINDS`].
    pub kind: String,
    /// Matched case-insensitively against the name, and for hardware
    /// against every field; empty matches everything.
    pub text: String,
    /// The name is `text` exactly, case aside, rather than containing it.
    pub exact: bool,
    /// Only versions strictly below this one.
    pub below: Option<String>,
    /// Only versions at or above this one.
    pub at_least: Option<String>,
    pub site: Option<Uuid>,
    pub server: Option<Uuid>,
    /// The sites the caller may read.
    pub scope: Scope,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Hit {
    pub server_id: Uuid,
    pub hostname: String,
    pub site_id: Uuid,
    pub site: String,
    /// Where it comes from: `package` (Linux packages), `windows_update`,
    /// `winget`, `flatpak`, `snap`, `program` (Windows programs) for
    /// software; the item kind otherwise.
    pub source: String,
    /// The package or program name, or the item's key, like `sda`.
    pub name: String,
    /// The version compared: the software's, a system's BIOS version, a
    /// disk's firmware; `null` for the others.
    pub version: Option<String>,
    /// Every field of the item, like `{"model": "...", "serial": "..."}`.
    #[schema(value_type = Object)]
    pub fields: Value,
}

/// `%text%`, or `text`, with `\`, `%` and `_` escaped for `ILIKE`.
fn pattern(text: &str, exact: bool) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    if text.is_empty() {
        String::new()
    } else if exact {
        escaped
    } else {
        format!("%{escaped}%")
    }
}

/// The servers a search looks at: `$1`, `$2` the scope, `$3` a server,
/// `$4` a site.
const SERVERS: &str = "WITH srv AS (
    SELECT s.id, s.hostname, s.site_id, si.name AS site_name, s.os_family
    FROM servers s JOIN sites si ON si.id = s.site_id
    WHERE s.revoked_at IS NULL AND s.kind = 'agent'
      AND ($1 OR s.site_id = ANY($2))
      AND ($3::uuid IS NULL OR s.id = $3)
      AND ($4::uuid IS NULL OR s.site_id = $4))";

/// Runs `query`; also says whether more rows matched than were returned.
pub async fn run(pool: &PgPool, query: &Query) -> Result<(Vec<Hit>, bool), sqlx::Error> {
    let (all, sites) = query.scope.bind();
    let like = pattern(query.text.trim(), query.exact);
    let sql = if query.kind == "software" {
        format!(
            "{SERVERS}
            SELECT srv.id AS server_id, srv.hostname, srv.site_id, srv.site_name AS site,
                   CASE WHEN srv.os_family = 'windows' THEN 'windows_update' ELSE 'package' END AS source,
                   p.name, p.version,
                   jsonb_strip_nulls(jsonb_build_object('arch', NULLIF(p.arch, ''),
                       'upgradable_version', p.upgradable_version, 'title', p.title)) AS fields
            FROM server_packages p JOIN srv ON srv.id = p.server_id
            WHERE p.version <> '' AND ($5 = '' OR p.name ILIKE $5 OR p.title ILIKE $5)
            UNION ALL
            SELECT srv.id, srv.hostname, srv.site_id, srv.site_name, t.manager,
                   coalesce(t.name, t.package_id), t.version,
                   jsonb_strip_nulls(jsonb_build_object('id', t.package_id, 'scope', t.scope,
                       'available_version', t.available_version))
            FROM server_third_party_packages t JOIN srv ON srv.id = t.server_id
            WHERE ($5 = '' OR t.name ILIKE $5 OR t.package_id ILIKE $5)
            UNION ALL
            SELECT srv.id, srv.hostname, srv.site_id, srv.site_name, 'program', i.key,
                   i.fields->>'version', i.fields
            FROM server_inventory_items i JOIN srv ON srv.id = i.server_id
            WHERE i.kind = 'software' AND ($5 = '' OR i.key ILIKE $5)
            ORDER BY 2, 1, 6, 5
            LIMIT $6"
        )
    } else {
        format!(
            "{SERVERS}
            SELECT srv.id AS server_id, srv.hostname, srv.site_id, srv.site_name AS site,
                   i.kind AS source, i.key AS name,
                   CASE i.kind WHEN 'system' THEN i.fields->>'bios_version'
                               WHEN 'disk' THEN i.fields->>'firmware' END AS version,
                   i.fields
            FROM server_inventory_items i JOIN srv ON srv.id = i.server_id
            WHERE i.kind = $7
              AND ($5 = '' OR i.key ILIKE $5
                   OR (NOT $8 AND EXISTS (SELECT 1 FROM jsonb_each_text(i.fields) f
                                          WHERE f.value ILIKE $5)))
            ORDER BY 2, 1, 6
            LIMIT $6"
        )
    };
    // Built from constants only; every value is bound.
    let mut select = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(all)
        .bind(sites)
        .bind(query.server)
        .bind(query.site)
        .bind(&like)
        .bind(MAX_SCANNED + 1);
    if query.kind != "software" {
        select = select.bind(&query.kind).bind(query.exact);
    }
    let mut hits: Vec<Hit> = select.fetch_all(pool).await?;
    let mut truncated = hits.len() as i64 > MAX_SCANNED;
    hits.truncate(MAX_SCANNED as usize);
    hits.retain(|hit| keep(hit.version.as_deref(), query));
    if hits.len() > query.limit {
        hits.truncate(query.limit);
        truncated = true;
    }
    Ok((hits, truncated))
}

/// Whether `version` passes the version filters of `query`.
pub fn keep(version: Option<&str>, query: &Query) -> bool {
    if query.below.is_none() && query.at_least.is_none() {
        return true;
    }
    let Some(version) = version.filter(|v| !v.is_empty()) else {
        return false;
    };
    query
        .below
        .as_deref()
        .is_none_or(|below| version::compare(version, below) == Ordering::Less)
        && query
            .at_least
            .as_deref()
            .is_none_or(|floor| version::compare(version, floor) != Ordering::Less)
}

/// The hits as CSV, one row each, with the fields as `name=value` pairs.
pub fn csv(hits: &[Hit]) -> String {
    let mut out = String::from("hostname,site,source,name,version,details\n");
    for hit in hits {
        let details = match &hit.fields {
            Value::Object(map) => map
                .iter()
                .filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?)))
                .collect::<Vec<_>>()
                .join("; "),
            _ => String::new(),
        };
        let row = [
            hit.hostname.as_str(),
            &hit.site,
            &hit.source,
            &hit.name,
            hit.version.as_deref().unwrap_or(""),
            &details,
        ]
        .map(crate::availability::csv::field);
        let _ = writeln!(out, "{}", row.join(","));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn patterns_escape_wildcards() {
        assert_eq!(pattern("open_ssl%", false), "%open\\_ssl\\%%");
        assert_eq!(pattern("openssl", true), "openssl");
        assert_eq!(pattern("", false), "");
    }

    #[test]
    fn versions_filter_hits() {
        let query = |below: Option<&str>, at_least: Option<&str>| Query {
            kind: "software".into(),
            text: String::new(),
            exact: false,
            below: below.map(str::to_owned),
            at_least: at_least.map(str::to_owned),
            site: None,
            server: None,
            scope: Scope::All,
            limit: 10,
        };
        let q = query(Some("3.0.15"), None);
        assert!(keep(Some("3.0.14-1~deb12u2"), &q));
        assert!(!keep(Some("3.0.15-1~deb12u1"), &q));
        assert!(!keep(None, &q), "no version, no match");
        let q = query(Some("4"), Some("3.0.15"));
        assert!(keep(Some("3.1"), &q));
        assert!(!keep(Some("3.0.2"), &q));
        assert!(keep(None, &query(None, None)));
    }

    #[test]
    fn csv_rows_are_quoted_and_defused() {
        let hit = Hit {
            server_id: Uuid::nil(),
            hostname: "web-1".into(),
            site_id: Uuid::nil(),
            site: "Paris, main".into(),
            source: "program".into(),
            name: "=cmd".into(),
            version: Some("1.0".into()),
            fields: json!({"publisher": "Igor Pavlov", "version": "1.0"}),
        };
        assert_eq!(
            csv(&[hit]),
            "hostname,site,source,name,version,details\n\
             web-1,\"Paris, main\",program,'=cmd,1.0,publisher=Igor Pavlov; version=1.0\n"
        );
    }
}
