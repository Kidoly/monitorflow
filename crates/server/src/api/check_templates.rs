//! Ready-made application checks (ADR-0183): their templates, and the
//! alerts a check gets when it is added with them.
//!
//! The checks themselves are ordinary rows of `sensor_configs` of kinds
//! `postgresql`, `mysql`, `mssql`, `redis` and `docker`, made with the routes of
//! [`super::sensors`]; a template only says what such a check measures and
//! which thresholds come with it.

use axum::Json;
use monitorflow_apps::templates::{self, Template};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::ApiError;

/// Body of `GET /api/check-templates`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CheckTemplates {
    pub templates: Vec<TemplateView>,
}

/// What one kind of application check measures, and its default alerts.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct TemplateView {
    /// The check's `kind`, and its name by default.
    pub kind: &'static str,
    pub title: &'static str,
    /// `agent_or_probe`: the server's agent, the central server or a
    /// relay; `agent`: the server's agent only.
    pub runs: &'static str,
    pub default_port: Option<u16>,
    pub params: Vec<ParamView>,
    pub metrics: Vec<MetricView>,
    pub alerts: Vec<AlertView>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ParamView {
    pub name: &'static str,
    pub default: Option<&'static str>,
    pub description: &'static str,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MetricView {
    /// The full name for a check of the default name, like
    /// `monitorflow_postgresql_connections_ratio`; another check name
    /// replaces `postgresql`.
    pub metric: String,
    pub suffix: &'static str,
    /// `""`, `seconds`, `bytes`, `ratio` (0 to 1), `per_second` or `count`.
    pub unit: &'static str,
    /// What each instance is, when there is one per database, replica or
    /// container.
    pub per: Option<&'static str>,
    pub description: &'static str,
}

/// A threshold added with `default_alerts`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AlertView {
    pub metric: String,
    pub comparison: &'static str,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// Seconds the condition must hold; 0 alerts at once.
    pub duration_s: i32,
}

fn view(t: &Template) -> TemplateView {
    TemplateView {
        kind: t.kind,
        title: t.title,
        runs: t.runs,
        default_port: t.default_port,
        params: t
            .params
            .iter()
            .map(|p| ParamView {
                name: p.name,
                default: p.default,
                description: p.description,
            })
            .collect(),
        metrics: t
            .metrics
            .iter()
            .map(|m| MetricView {
                metric: templates::metric_name(t.kind, m.suffix),
                suffix: m.suffix,
                unit: m.unit,
                per: m.per,
                description: m.description,
            })
            .collect(),
        alerts: t
            .alerts
            .iter()
            .map(|a| AlertView {
                metric: templates::metric_name(t.kind, a.suffix),
                comparison: a.comparison,
                warning: a.warning,
                critical: a.critical,
                duration_s: a.minutes * 60,
            })
            .collect(),
    }
}

/// `GET /api/check-templates`: every template. Any signed-in role.
pub async fn list(_user: CurrentUser) -> Result<Json<CheckTemplates>, ApiError> {
    Ok(Json(CheckTemplates {
        templates: templates::TEMPLATES.iter().map(view).collect(),
    }))
}

/// Adds the alerts of `kind`'s template for the check named `check` to a
/// site or a server (exactly one of them), where it has no threshold on
/// that metric for every instance yet. Returns the metrics added.
pub async fn add_alerts(
    db: &mut PgConnection,
    site: Option<Uuid>,
    server: Option<Uuid>,
    kind: &str,
    check: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let Some(template) = templates::of(kind) else {
        return Ok(Vec::new());
    };
    let mut added = Vec::new();
    for alert in template.alerts {
        let metric = templates::metric_name(check, alert.suffix);
        let inserted = sqlx::query(
            "INSERT INTO thresholds (site_id, server_id, metric, comparison, warning, critical, duration_s)
             SELECT $1, $2, $3, $4, $5, $6, $7
             WHERE NOT EXISTS (SELECT 1 FROM thresholds
                               WHERE site_id IS NOT DISTINCT FROM $1
                                 AND server_id IS NOT DISTINCT FROM $2
                                 AND metric = $3 AND instance IS NULL)",
        )
        .bind(site)
        .bind(server)
        .bind(&metric)
        .bind(alert.comparison)
        .bind(alert.warning)
        .bind(alert.critical)
        .bind(alert.minutes * 60)
        .execute(&mut *db)
        .await?
        .rows_affected();
        if inserted > 0 {
            added.push(metric);
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_name_full_metrics() {
        let views: Vec<_> = templates::TEMPLATES.iter().map(view).collect();
        assert_eq!(views.len(), 5);
        let pg = &views[0];
        assert_eq!(pg.kind, "postgresql");
        assert!(pg
            .metrics
            .iter()
            .any(|m| m.metric == "monitorflow_postgresql_up"));
        let up = pg
            .alerts
            .iter()
            .find(|a| a.metric == "monitorflow_postgresql_up")
            .unwrap();
        assert_eq!((up.critical, up.duration_s), (Some(1.0), 120));
        assert!(pg
            .params
            .iter()
            .any(|p| p.name == "password" && p.default.is_none()));
        for v in &views {
            for a in &v.alerts {
                // What the thresholds table accepts.
                assert!(a.metric.len() <= "monitorflow_".len() + 64, "{}", a.metric);
                assert!((0..=86_400).contains(&a.duration_s));
            }
        }
        assert_eq!(views[2].kind, "mssql");
        assert_eq!(views[4].runs, "agent");
    }
}
