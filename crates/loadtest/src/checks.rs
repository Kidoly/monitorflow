//! The checks a run adds through the API, as an operator adds them in the
//! interface: each one watches one thing (a service, a port), comes with
//! its alert, and counts as one sensor. Every agent runs the five built-in
//! sensors plus its site's checks and its own; each device has a ping and
//! some port checks, run by the central server or its site's relay.

use std::net::{Ipv4Addr, SocketAddr};

use serde_json::{json, Value};
use tokio::net::TcpListener;

/// Sensors every agent runs without a row: cpu, memory, disk, diskio, net.
pub const BUILTIN: usize = 5;
/// Interval of every added check, the interface's default.
pub const INTERVAL_MS: u64 = 60_000;
/// Ports the devices' checks connect to, on each device's address.
pub const DEVICE_PORTS: [u16; 8] = [20022, 20080, 20443, 23306, 25432, 26379, 28080, 29100];
/// Most devices: their addresses are 127.1.0.1 to 127.1.249.250.
pub const MAX_DEVICES: usize = 250 * 250;

/// How many checks of each kind a run adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Sensors each agent runs, the built-in ones included.
    pub per_agent: usize,
    /// Of the added ones, how many are the agent's own rather than its
    /// site's.
    pub own: usize,
    pub devices: usize,
    /// Port checks of each device, next to its ping.
    pub device_ports: usize,
}

/// One check: its sensor row and the alert that comes with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub kind: &'static str,
    pub name: String,
    pub params: Value,
}

impl Check {
    /// A service `units` (`app-01`), named as the interface names it.
    fn service(unit: &str) -> Self {
        Self {
            kind: "systemd",
            name: format!("service_{}", unit.replace('-', "_")),
            params: json!({"units": unit}),
        }
    }

    fn port(port: u16) -> Self {
        Self {
            kind: "port",
            name: format!("port_{port}"),
            params: json!({"port": port}),
        }
    }

    /// Body of `POST .../sensor_configs`.
    pub fn sensor(&self) -> Value {
        json!({
            "kind": self.kind,
            "name": self.name,
            "interval_ms": INTERVAL_MS,
            "params": self.params,
        })
    }

    /// Body of `POST .../thresholds`: critical when the service is not
    /// active or the port not open for a minute, as the interface adds it.
    pub fn threshold(&self) -> Value {
        let suffix = if self.kind == "systemd" {
            "active"
        } else {
            "up"
        };
        json!({
            "metric": format!("monitorflow_{}_{suffix}", self.name),
            "comparison": "<",
            "critical": 1,
            "duration_s": 60,
            "hysteresis": 0,
        })
    }
}

/// Two services for one port: a server runs more services than it opens
/// ports.
fn nth(n: usize, unit: impl Fn(usize) -> String, port: impl Fn(usize) -> u16) -> Check {
    if n % 3 == 2 {
        Check::port(port(n))
    } else {
        Check::service(&unit(n))
    }
}

impl Plan {
    pub fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.per_agent >= BUILTIN,
            "--sensors-per-agent is at least {BUILTIN}, the built-in sensors"
        );
        anyhow::ensure!(
            self.per_agent - BUILTIN <= 5_000,
            "--sensors-per-agent is at most {}",
            BUILTIN + 5_000
        );
        anyhow::ensure!(
            self.devices <= MAX_DEVICES,
            "--devices is at most {MAX_DEVICES}"
        );
        anyhow::ensure!(
            self.device_ports <= DEVICE_PORTS.len(),
            "--device-ports is at most {}",
            DEVICE_PORTS.len()
        );
        Ok(())
    }

    fn added(&self) -> usize {
        self.per_agent.saturating_sub(BUILTIN)
    }

    fn own_count(&self) -> usize {
        self.own.min(self.added())
    }

    /// The checks of every site, the same on each.
    pub fn site_checks(&self) -> Vec<Check> {
        (0..self.added() - self.own_count())
            .map(|n| nth(n, |n| format!("app-{n:02}"), |n| 10_000 + n as u16))
            .collect()
    }

    /// Checks of the agent itself, the same names on every agent.
    pub fn own_checks(&self) -> Vec<Check> {
        (0..self.own_count())
            .map(|n| nth(n, |n| format!("own-{n:02}"), |n| 15_000 + n as u16))
            .collect()
    }

    /// Port checks of each device; its ping comes with it.
    pub fn device_checks(&self) -> Vec<Check> {
        DEVICE_PORTS[..self.device_ports]
            .iter()
            .map(|&p| Check::port(p))
            .collect()
    }

    /// Sensors of the whole run: agents' and devices'.
    pub fn total(&self, agents: usize) -> usize {
        agents * self.per_agent + self.devices * (1 + self.device_ports)
    }
}

/// Address of device `n`.
pub fn device_address(n: usize) -> Ipv4Addr {
    Ipv4Addr::new(127, 1, (n / 250) as u8, (n % 250 + 1) as u8)
}

/// Listens on every port the devices' checks connect to, on each
/// device's loopback address, accepting and closing connections. The
/// listeners stop when the returned set is dropped.
pub async fn device_listeners(
    devices: usize,
    ports: usize,
) -> anyhow::Result<tokio::task::JoinSet<()>> {
    let mut set = tokio::task::JoinSet::new();
    for n in 0..devices {
        for &port in &DEVICE_PORTS[..ports] {
            let addr = SocketAddr::from((device_address(n), port));
            let listener = TcpListener::bind(addr)
                .await
                .map_err(|e| anyhow::anyhow!("listening on {addr}: {e}"))?;
            set.spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    drop(stream);
                }
            });
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(per_agent: usize, own: usize) -> Plan {
        Plan {
            per_agent,
            own,
            devices: 800,
            device_ports: 4,
        }
    }

    #[test]
    fn fifty_sensors_are_the_builtins_forty_site_checks_and_five_own() {
        let plan = plan(50, 5);
        plan.check().unwrap();
        let site = plan.site_checks();
        let own = plan.own_checks();
        assert_eq!((site.len(), own.len()), (40, 5));
        assert_eq!(BUILTIN + site.len() + own.len(), 50);
        // Two services for one port.
        assert_eq!(site.iter().filter(|c| c.kind == "port").count(), 13);
        assert_eq!(plan.total(2_000), 104_000);
        let names: std::collections::BTreeSet<_> =
            site.iter().chain(&own).map(|c| c.name.clone()).collect();
        assert_eq!(names.len(), 45, "names are unique on an agent");
    }

    #[test]
    fn the_builtins_alone_add_nothing() {
        let plan = plan(5, 5);
        assert!(plan.site_checks().is_empty());
        assert!(plan.own_checks().is_empty());
        assert!(plan.check().is_ok());
        assert!(super::Plan {
            per_agent: 4,
            ..plan
        }
        .check()
        .is_err());
        assert!(super::Plan {
            device_ports: 9,
            ..plan
        }
        .check()
        .is_err());
    }

    #[test]
    fn checks_look_like_the_interface_adds_them() {
        let service = &plan(10, 0).site_checks()[0];
        assert_eq!(service.name, "service_app_00");
        assert_eq!(service.sensor()["params"]["units"], "app-00");
        assert_eq!(service.sensor()["interval_ms"], 60_000);
        assert_eq!(
            service.threshold()["metric"],
            "monitorflow_service_app_00_active"
        );
        let port = &plan(10, 0).site_checks()[2];
        assert_eq!(port.name, "port_10002");
        assert_eq!(port.threshold()["metric"], "monitorflow_port_10002_up");
        assert_eq!(port.threshold()["comparison"], "<");
        assert_eq!(plan(10, 0).device_checks()[0].params["port"], 20022);
    }

    #[test]
    fn device_addresses_are_distinct_loopback_addresses() {
        assert_eq!(device_address(0), Ipv4Addr::new(127, 1, 0, 1));
        assert_eq!(device_address(249), Ipv4Addr::new(127, 1, 0, 250));
        assert_eq!(device_address(250), Ipv4Addr::new(127, 1, 1, 1));
        assert!(device_address(MAX_DEVICES - 1).is_loopback());
    }

    #[tokio::test]
    async fn device_listeners_accept_connections() {
        let _set = device_listeners(2, 1).await.unwrap();
        let addr = SocketAddr::from((device_address(1), DEVICE_PORTS[0]));
        tokio::net::TcpStream::connect(addr).await.unwrap();
    }
}
