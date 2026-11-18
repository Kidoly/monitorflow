//! Probe checks in PostgreSQL: the enabled rows of `sensor_configs` with
//! a probe, of servers and devices still in the fleet.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use monitorflow_probe::{Check, CENTRAL};
use monitorflow_sensors::SensorConfig;
use sqlx::PgPool;

use super::ProbeSet;
use crate::sensor_config::params;

#[derive(Debug, Clone)]
pub struct ProbeStore {
    pool: PgPool,
}

/// A stored row with what a probe needs.
type Line = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    Option<i64>,
    serde_json::Value,
    String,
    Option<String>,
);

impl ProbeStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Every probe's checks, by probe: [`CENTRAL`] or a relay's ID.
    pub async fn load(&self) -> Result<HashMap<String, ProbeSet>, sqlx::Error> {
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT s.peer_id, si.name, s.address, c.kind, c.name, c.instance,
                    c.interval_ms, c.timeout_ms, c.params, c.probe, r.peer_id
             FROM sensor_configs c
             JOIN servers s ON s.id = c.server_id
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN relays r ON r.id = c.probe_relay_id
             WHERE c.probe IS NOT NULL AND c.enabled AND s.revoked_at IS NULL
               AND (c.probe = 'central' OR (r.revoked_at IS NULL AND r.site_id = s.site_id))
             ORDER BY s.peer_id, c.name, c.instance",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(group(lines))
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Rows into one [`ProbeSet`] per probe. A row without `host` gets the
/// device's address.
fn group(lines: Vec<Line>) -> HashMap<String, ProbeSet> {
    let mut sets: HashMap<String, ProbeSet> = HashMap::new();
    for line in lines {
        let (device, site, address, kind, name, instance, interval, timeout, raw, probe, relay) =
            line;
        let probe = match (probe.as_str(), relay) {
            ("central", _) => CENTRAL.to_owned(),
            (_, Some(relay)) => relay,
            _ => continue,
        };
        let mut params: BTreeMap<String, String> = params(raw);
        if let Some(address) = address {
            params.entry("host".into()).or_insert(address);
        }
        let mut config = SensorConfig::new(name, Duration::from_millis(non_negative(interval)));
        config.instance = instance;
        config.timeout = timeout.map(|t| Duration::from_millis(non_negative(t)));
        config.params = params;
        let set = sets.entry(probe).or_default();
        set.sites.insert(device.clone(), site);
        set.checks.push(Check {
            device,
            kind,
            config,
        });
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        device: &str,
        address: Option<&str>,
        name: &str,
        probe: &str,
        relay: Option<&str>,
        params: serde_json::Value,
    ) -> Line {
        (
            device.into(),
            "paris".into(),
            address.map(str::to_owned),
            "ping".into(),
            name.into(),
            String::new(),
            60_000,
            None,
            params,
            probe.into(),
            relay.map(str::to_owned),
        )
    }

    #[test]
    fn rows_are_grouped_by_probe_with_the_device_address() {
        let sets = group(vec![
            line(
                "nas",
                Some("192.0.2.5"),
                "ping",
                "central",
                None,
                serde_json::json!({}),
            ),
            line(
                "nas",
                Some("192.0.2.5"),
                "ping_lan",
                "relay",
                Some("relay-1"),
                serde_json::json!({"count": 3}),
            ),
            line(
                "web-01",
                None,
                "ping",
                "relay",
                Some("relay-1"),
                serde_json::json!({"host": "198.51.100.7"}),
            ),
            line("odd", None, "ping", "relay", None, serde_json::json!({})),
        ]);
        assert_eq!(sets.len(), 2);
        let central = &sets[CENTRAL];
        assert_eq!(central.checks.len(), 1);
        assert_eq!(central.checks[0].config.params["host"], "192.0.2.5");
        assert_eq!(central.sites["nas"], "paris");
        let relay = &sets["relay-1"];
        assert_eq!(relay.checks.len(), 2);
        assert_eq!(relay.checks[0].config.params["host"], "192.0.2.5");
        assert_eq!(relay.checks[0].config.params["count"], "3");
        assert_eq!(relay.checks[1].config.params["host"], "198.51.100.7");
        assert_eq!(relay.checks[1].config.interval, Duration::from_secs(60));
        assert_ne!(central.revision(), relay.revision());
        assert_eq!(relay.revision(), relay.clone().revision());
        assert_eq!(ProbeSet::default().to_proto().checks.len(), 0);
    }
}
