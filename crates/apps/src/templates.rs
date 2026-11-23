//! What each kind measures and the alerts that come with it (ADR-0183):
//! the template the server applies when a check is added with its
//! default alerts, and lists in its API.

use crate::{DOCKER, MYSQL, POSTGRESQL, REDIS};

/// One metric of a kind, by its suffix: the check's name, `_`, then this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metric {
    pub suffix: &'static str,
    /// `""`, `seconds`, `bytes`, `ratio` (0 to 1), `per_second`, `count`.
    pub unit: &'static str,
    /// One per instance: a database, a replica, a container.
    pub per: Option<&'static str>,
    pub description: &'static str,
}

/// A default alert on a metric, an ordinary threshold once added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alert {
    pub suffix: &'static str,
    /// `>`, `>=`, `<` or `<=`.
    pub comparison: &'static str,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    /// How long it must hold, in minutes; 0 alerts at once.
    pub minutes: i32,
}

/// One kind's template.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub kind: &'static str,
    pub title: &'static str,
    /// `agent_or_probe` or `agent`.
    pub runs: &'static str,
    pub default_port: Option<u16>,
    pub metrics: &'static [Metric],
    pub alerts: &'static [Alert],
}

const fn m(
    suffix: &'static str,
    unit: &'static str,
    per: Option<&'static str>,
    description: &'static str,
) -> Metric {
    Metric {
        suffix,
        unit,
        per,
        description,
    }
}

const fn a(
    suffix: &'static str,
    comparison: &'static str,
    warning: Option<f64>,
    critical: Option<f64>,
    minutes: i32,
) -> Alert {
    Alert {
        suffix,
        comparison,
        warning,
        critical,
        minutes,
    }
}

const UP: Metric = m(
    "up",
    "",
    None,
    "1 when it answered and the login worked; 0 with labels `reason` and `error`",
);
const RESPONSE: Metric = m(
    "response_seconds",
    "seconds",
    None,
    "Time to connect, log in and read the first answer",
);
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub const POSTGRESQL_TEMPLATE: Template = Template {
    kind: POSTGRESQL,
    title: "PostgreSQL",
    runs: "agent_or_probe",
    default_port: Some(5432),
    metrics: &[
        UP,
        RESPONSE,
        m("connections", "count", None, "Client connections, the check's own included"),
        m("connections_max", "count", None, "`max_connections`"),
        m("connections_ratio", "ratio", None, "Connections against `max_connections`"),
        m("connections_active", "count", None, "Connections running a query"),
        m("connections_idle_in_transaction", "count", None, "Connections idle inside an open transaction"),
        m("slow_queries", "count", None, "Queries running for longer than `slow_after` seconds (30)"),
        m("longest_query_seconds", "seconds", None, "Age of the longest running query"),
        m("longest_transaction_seconds", "seconds", None, "Age of the oldest open transaction"),
        m("locks_waiting", "count", None, "Locks waited for"),
        m("deadlocks", "count", None, "Deadlocks since the last collection"),
        m("cache_hit_ratio", "ratio", None, "Blocks found in shared buffers since the last collection"),
        m("transactions_per_second", "per_second", None, "Commits and rollbacks per second"),
        m("database_size_bytes", "bytes", Some("database"), "Size of each database"),
        m("replicas", "count", None, "Replicas streaming from this primary"),
        m("replica_lag_bytes", "bytes", Some("replica"), "WAL a replica has not replayed yet"),
        m("replica_lag_seconds", "seconds", Some("replica"), "How late a replica replays"),
        m("wal_receiver_up", "", None, "On a replica: 1 while it streams from its primary"),
        m("replication_delay_seconds", "seconds", None, "On a replica: age of the last replayed transaction, 0 when caught up"),
    ],
    alerts: &[
        a("up", "<", None, Some(1.0), 2),
        a("connections_ratio", ">", Some(0.8), Some(0.95), 5),
        a("longest_transaction_seconds", ">", Some(3600.0), None, 5),
        a("locks_waiting", ">", Some(10.0), None, 5),
        a("deadlocks", ">", Some(0.0), None, 0),
        a("cache_hit_ratio", "<", Some(0.9), None, 30),
        a("replica_lag_bytes", ">", Some(GIB), None, 10),
        a("wal_receiver_up", "<", None, Some(1.0), 5),
        a("replication_delay_seconds", ">", Some(300.0), Some(1800.0), 5),
    ],
};

pub const MYSQL_TEMPLATE: Template = Template {
    kind: MYSQL,
    title: "MySQL and MariaDB",
    runs: "agent_or_probe",
    default_port: Some(3306),
    metrics: &[
        UP,
        RESPONSE,
        m("connections", "count", None, "`Threads_connected`"),
        m("connections_max", "count", None, "`max_connections`"),
        m("connections_ratio", "ratio", None, "Connections against `max_connections`"),
        m("threads_running", "count", None, "`Threads_running`: queries running now"),
        m("queries_per_second", "per_second", None, "Statements sent by clients per second (`Questions`)"),
        m("slow_queries", "count", None, "Queries over `long_query_time` since the last collection"),
        m("aborted_connections", "count", None, "Failed connection attempts since the last collection"),
        m("buffer_pool_hit_ratio", "ratio", None, "InnoDB reads served from the buffer pool since the last collection"),
        m("lock_waits", "count", None, "InnoDB row locks waited for now"),
        m("database_size_bytes", "bytes", Some("schema"), "Data and indexes of each schema the account sees, every 10 minutes"),
        m("replica_io_running", "", None, "On a replica: 1 while its I/O thread runs"),
        m("replica_sql_running", "", None, "On a replica: 1 while its SQL thread runs"),
        m("replication_delay_seconds", "seconds", None, "On a replica: `Seconds_Behind_Source`"),
    ],
    alerts: &[
        a("up", "<", None, Some(1.0), 2),
        a("connections_ratio", ">", Some(0.8), Some(0.95), 5),
        a("slow_queries", ">", Some(10.0), None, 5),
        a("buffer_pool_hit_ratio", "<", Some(0.95), None, 30),
        a("lock_waits", ">", Some(10.0), None, 5),
        a("replica_io_running", "<", None, Some(1.0), 2),
        a("replica_sql_running", "<", None, Some(1.0), 2),
        a("replication_delay_seconds", ">", Some(300.0), Some(1800.0), 5),
    ],
};

pub const REDIS_TEMPLATE: Template = Template {
    kind: REDIS,
    title: "Redis",
    runs: "agent_or_probe",
    default_port: Some(6379),
    metrics: &[
        UP,
        RESPONSE,
        m("clients", "count", None, "Connected clients"),
        m("blocked_clients", "count", None, "Clients waiting on a blocking command"),
        m("rejected_connections", "count", None, "Connections refused for `maxclients` since the last collection"),
        m("memory_used_bytes", "bytes", None, "`used_memory`"),
        m("memory_max_bytes", "bytes", None, "`maxmemory`, when set"),
        m("memory_usage_ratio", "ratio", None, "Used memory against `maxmemory`, when set"),
        m("memory_fragmentation_ratio", "", None, "Memory the system gave against what Redis uses"),
        m("evicted_keys", "count", None, "Keys evicted for `maxmemory` since the last collection"),
        m("keyspace_hit_ratio", "ratio", None, "Lookups that found their key since the last collection"),
        m("keys", "count", None, "Keys in every database"),
        m("last_save_ok", "", None, "1 when the last snapshot (RDB) was written"),
        m("aof_write_ok", "", None, "With AOF: 1 when the last append-only file write worked"),
        m("replicas", "count", None, "Connected replicas"),
        m("replica_lag_bytes", "bytes", Some("replica"), "Replication stream a replica has not acknowledged"),
        m("primary_link_up", "", None, "On a replica: 1 while linked to its primary"),
        m("primary_last_io_seconds", "seconds", None, "On a replica: time since its primary last spoke"),
    ],
    alerts: &[
        a("up", "<", None, Some(1.0), 2),
        a("memory_usage_ratio", ">", Some(0.8), Some(0.95), 5),
        a("evicted_keys", ">", Some(0.0), None, 15),
        a("rejected_connections", ">", Some(0.0), None, 0),
        a("last_save_ok", "<", None, Some(1.0), 0),
        a("aof_write_ok", "<", None, Some(1.0), 0),
        a("primary_link_up", "<", None, Some(1.0), 2),
    ],
};

pub const DOCKER_TEMPLATE: Template = Template {
    kind: DOCKER,
    title: "Docker containers",
    runs: "agent",
    default_port: None,
    metrics: &[
        m("up", "", None, "1 when the engine answered; 0 with labels `reason` and `error`"),
        m("response_seconds", "seconds", None, "Time the engine took to answer"),
        m("containers_running", "count", None, "Watched containers that run"),
        m("containers_stopped", "count", None, "Watched containers that do not run"),
        m("container_running", "", Some("container"), "1 when it runs; labels `image` and `state`"),
        m("container_restart_count", "count", Some("container"), "Restarts since it was created"),
        m("container_restarts", "count", Some("container"), "Restarts since the last collection"),
        m("container_health", "", Some("container"), "Its health check: 0 healthy, 1 starting, 2 unhealthy"),
        m("container_cpu_ratio", "ratio", Some("container"), "Share of the machine's processor time"),
        m("container_memory_bytes", "bytes", Some("container"), "Memory used, without the page cache"),
        m("container_memory_ratio", "ratio", Some("container"), "Memory used against its limit"),
    ],
    alerts: &[
        a("up", "<", None, Some(1.0), 2),
        a("container_running", "<", None, Some(1.0), 2),
        a("container_restarts", ">", Some(0.0), None, 0),
        a("container_health", ">=", None, Some(2.0), 2),
        a("container_memory_ratio", ">", Some(0.9), None, 5),
    ],
};

/// Every template, in the order they are offered.
pub const TEMPLATES: [Template; 4] = [
    POSTGRESQL_TEMPLATE,
    MYSQL_TEMPLATE,
    REDIS_TEMPLATE,
    DOCKER_TEMPLATE,
];

/// The template of `kind`.
pub fn of(kind: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.kind == kind)
}

/// The full name of a metric of check `name`, as stored and alerted on.
pub fn metric_name(check: &str, suffix: &str) -> String {
    format!("monitorflow_{check}_{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_name_metrics_of_their_template() {
        for t in TEMPLATES {
            assert!(crate::is_kind(t.kind));
            for alert in t.alerts {
                assert!(
                    t.metrics.iter().any(|m| m.suffix == alert.suffix),
                    "{} {}",
                    t.kind,
                    alert.suffix
                );
                assert!(alert.warning.is_some() || alert.critical.is_some());
                assert!(matches!(alert.comparison, ">" | ">=" | "<" | "<="));
            }
            let mut names: Vec<_> = t.metrics.iter().map(|m| m.suffix).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), t.metrics.len(), "{}", t.kind);
        }
        assert_eq!(metric_name("postgresql", "up"), "monitorflow_postgresql_up");
        assert!(of("redis").is_some() && of("cpu").is_none());
    }
}
