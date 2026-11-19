//! Probe checks in PostgreSQL: the enabled rows of `sensor_configs` with
//! a probe, of servers and devices still in the fleet.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use monitorflow_probe::{Check, CENTRAL};
use monitorflow_sensors::SensorConfig;
use sqlx::PgPool;
use uuid::Uuid;

use super::ProbeSet;
use crate::sensor_config::{params, with_secret};

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
                    c.interval_ms, c.timeout_ms, c.params, c.probe, r.peer_id, c.secret
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
        let mut sets = group(lines);
        for kind in CREDENTIALED {
            let wanted: Vec<&str> = sets
                .values()
                .flat_map(|set| &set.checks)
                .filter(|c| c.kind == kind)
                .map(|c| c.device.as_str())
                .collect();
            if !wanted.is_empty() {
                let credentials = self.credentials(kind, &wanted).await?;
                add_credentials(&mut sets, kind, &credentials);
            }
        }
        let wanted: Vec<Uuid> = sets
            .values()
            .flat_map(|set| &set.checks)
            .filter(|c| c.kind == monitorflow_snmp::CUSTOM_KIND)
            .flat_map(|c| crate::snmp_custom::own_ids(&c.config.params))
            .collect();
        let mut conn = self.pool.acquire().await?;
        let stored = crate::snmp_custom::load_lookups(&mut conn, &wanted).await?;
        add_lookups(&mut sets, &stored);
        Ok(sets)
    }

    /// The opened SNMP (ADR-0149) or vCenter (ADR-0156) credentials of
    /// `devices`, as check parameters, by device ID. Those that do not
    /// open are left out, logged.
    async fn credentials(
        &self,
        kind: &str,
        devices: &[&str],
    ) -> Result<HashMap<String, BTreeMap<String, String>>, sqlx::Error> {
        let ids: Vec<String> = devices.iter().map(|d| (*d).to_owned()).collect();
        let rows: Vec<(String, Uuid)> =
            sqlx::query_as("SELECT peer_id, id FROM servers WHERE peer_id = ANY($1)")
                .bind(&ids)
                .fetch_all(&self.pool)
                .await?;
        let mut out = HashMap::new();
        let mut conn = self.pool.acquire().await?;
        for (peer, id) in rows {
            let opened = if kind == monitorflow_vmware::KIND {
                crate::vmware::load(&mut conn, id)
                    .await?
                    .map(|c| c.map(|c| c.params()))
            } else {
                crate::snmp::load(&mut conn, id)
                    .await?
                    .map(|c| c.map(|c| c.params()))
            };
            match opened {
                Some(Ok(params)) => {
                    out.insert(peer, params);
                }
                Some(Err(error)) => {
                    tracing::warn!(device = %peer, kind, %error, "cannot open the credentials of a device; its checks of that kind do not run")
                }
                None => {}
            }
        }
        Ok(out)
    }
}

/// Kinds whose checks run with their device's credentials (ADR-0149,
/// ADR-0156, ADR-0157).
pub const CREDENTIALED: [&str; 3] = [
    monitorflow_snmp::KIND,
    monitorflow_snmp::CUSTOM_KIND,
    monitorflow_vmware::KIND,
];

/// Adds to each check of `kind` its device's credentials; checks of a
/// device without any are left out, as they could not run.
fn add_credentials(
    sets: &mut HashMap<String, ProbeSet>,
    kind: &str,
    credentials: &HashMap<String, BTreeMap<String, String>>,
) {
    for set in sets.values_mut() {
        set.checks.retain_mut(|check| {
            if check.kind != kind {
                return true;
            }
            let Some(found) = credentials.get(&check.device) else {
                tracing::debug!(device = %check.device, kind, "a check of a device without credentials; not sent");
                return false;
            };
            for (key, value) in found {
                // A row's own port wins; secrets only come from here.
                if key == "port" && check.config.params.contains_key("port") {
                    continue;
                }
                check.config.params.insert(key.clone(), value.clone());
            }
            true
        });
    }
}

/// Adds to each custom SNMP check the definitions of its lookups
/// (ADR-0157); a check naming a lookup that is gone is left out.
fn add_lookups(
    sets: &mut HashMap<String, ProbeSet>,
    stored: &HashMap<Uuid, monitorflow_snmp::custom::Lookup>,
) {
    for set in sets.values_mut() {
        set.checks.retain_mut(|check| {
            if check.kind != monitorflow_snmp::CUSTOM_KIND {
                return true;
            }
            match crate::snmp_custom::expand(&mut check.config.params, stored) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(device = %check.device, check = %check.config.name, %error, "a custom SNMP check is not sent");
                    false
                }
            }
        });
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
        let (
            device,
            site,
            address,
            kind,
            name,
            instance,
            interval,
            timeout,
            raw,
            probe,
            relay,
            secret,
        ) = line;
        let probe = match (probe.as_str(), relay) {
            ("central", _) => CENTRAL.to_owned(),
            (_, Some(relay)) => relay,
            _ => continue,
        };
        let mut params: BTreeMap<String, String> = params(raw);
        // The password of a website check, opened for the probe that runs
        // it, which gets it over its mTLS stream like an agent.
        with_secret(&kind, &mut params, secret);
        // The device's credentials go to the device's address alone: a
        // `host` of the row is dropped, whenever it was stored.
        if CREDENTIALED.contains(&kind.as_str()) {
            params.remove("host");
        }
        // A website check names its URL itself (ADR-0150).
        if let Some(address) = address.filter(|_| kind != monitorflow_webcheck::KIND) {
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
            None,
        )
    }

    #[test]
    fn snmp_checks_get_their_device_credentials() {
        let mut sets = group(vec![
            (
                "switch".into(),
                "paris".into(),
                Some("192.0.2.2".into()),
                "snmp".into(),
                "snmp_if".into(),
                "central".into(),
                60_000,
                None,
                serde_json::json!({"what": "interfaces"}),
                "central".into(),
                None,
                None,
            ),
            (
                "printer".into(),
                "paris".into(),
                Some("192.0.2.3".into()),
                "snmp".into(),
                "snmp".into(),
                "central".into(),
                60_000,
                None,
                serde_json::json!({}),
                "central".into(),
                None,
                None,
            ),
            line(
                "nas",
                Some("192.0.2.5"),
                "ping",
                "central",
                None,
                serde_json::json!({}),
            ),
        ]);
        let credentials = HashMap::from([(
            "switch".to_string(),
            BTreeMap::from([
                ("community".to_string(), "c".to_string()),
                ("port".to_string(), "1161".to_string()),
            ]),
        )]);
        add_credentials(&mut sets, "snmp", &credentials);
        let checks = &sets[CENTRAL].checks;
        assert_eq!(checks.len(), 2, "the printer has no credentials");
        let switch = checks.iter().find(|c| c.device == "switch").unwrap();
        assert_eq!(switch.config.params["community"], "c");
        assert_eq!(switch.config.params["port"], "1161");
        assert_eq!(switch.config.params["host"], "192.0.2.2");
        assert!(monitorflow_snmp::parse(&switch.config.params).is_ok());
    }

    #[test]
    fn custom_checks_get_credentials_and_their_lookups() {
        let mine = Uuid::new_v4();
        let gone = Uuid::new_v4();
        let custom = |name: &str, lookup: String| {
            let mut row = line(
                "nas",
                Some("192.0.2.5"),
                name,
                "central",
                None,
                serde_json::json!({"oid1": "1.3.6.1.4.1.6574.1.1.0", "lookup1": lookup}),
            );
            row.3 = monitorflow_snmp::CUSTOM_KIND.into();
            row
        };
        let mut sets = group(vec![
            custom("snmp_state", mine.to_string()),
            custom("snmp_gone", gone.to_string()),
            custom("snmp_builtin", "builtin:truth_value".into()),
        ]);
        let credentials = HashMap::from([(
            "nas".to_string(),
            BTreeMap::from([("community".to_string(), "c".to_string())]),
        )]);
        add_credentials(&mut sets, monitorflow_snmp::CUSTOM_KIND, &credentials);
        let stored = HashMap::from([(
            mine,
            monitorflow_snmp::library::lookup_from(&[(1, "normal".into())]),
        )]);
        add_lookups(&mut sets, &stored);
        let checks = &sets[CENTRAL].checks;
        let names: Vec<&str> = checks.iter().map(|c| c.config.name.as_str()).collect();
        assert_eq!(
            names,
            ["snmp_state", "snmp_builtin"],
            "the gone lookup's check is not sent"
        );
        for check in checks {
            assert_eq!(check.config.params["community"], "c");
            monitorflow_snmp::custom::build(&check.config.params).unwrap();
        }
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
            {
                let mut web = line(
                    "nas",
                    Some("192.0.2.5"),
                    "http",
                    "central",
                    None,
                    serde_json::json!({"url": "https://nas.test/"}),
                );
                web.3 = "http".into();
                web
            },
        ]);
        assert_eq!(sets.len(), 2);
        let central = &sets[CENTRAL];
        assert_eq!(central.checks.len(), 2);
        assert_eq!(central.checks[0].config.params["host"], "192.0.2.5");
        let web = &central.checks[1];
        assert_eq!(web.kind, "http");
        assert!(
            !web.config.params.contains_key("host"),
            "{:?}",
            web.config.params
        );
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
