//! The PRTG reader on recorded, anonymised answers and files.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use super::api::{self, Fetch};
use super::mapping::inventory;
use super::tree::{Kind, Paused};
use super::{export, Prtg};
use crate::model::{Direction, Fidelity, Inventory, Measure, SilenceScope, Target, Watch};
use crate::source::{Connection, Context, File, Input, ReadError, Source};

const API: &str = include_str!("../../tests/fixtures/prtg/api.json");
const FILE: &str = include_str!("../../tests/fixtures/prtg/configuration.dat");

/// Answers recorded from a PRTG installation, by path and query.
struct Recorded(BTreeMap<String, Value>);

#[async_trait::async_trait]
impl Fetch for Recorded {
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError> {
        let query: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let key = if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{}", query.join("&"))
        };
        match self.0.get(&key) {
            Some(Value::String(body)) => Ok(body.clone()),
            Some(value) => Ok(value.to_string()),
            // What PRTG answers for a property an object does not have.
            None if path == "/api/getobjectproperty.htm" => {
                Ok("<prtg><result>(Property not found)</result></prtg>".into())
            }
            None => Err(ReadError::Connect(format!("nothing recorded for {key}"))),
        }
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
}

async fn from_api() -> Inventory {
    let recorded = Recorded(serde_json::from_str(API).unwrap());
    let tree = api::read(Arc::new(recorded), now()).await.unwrap();
    inventory(&tree, now())
}

/// Compares `inventory` with the golden file `name`; writes `name.new`
/// beside it when they differ, to review and rename.
fn golden(inventory: &Inventory, name: &str, expected: &str) {
    let got = serde_json::to_string_pretty(inventory).unwrap() + "\n";
    if got != expected {
        let path = format!(
            "{}/tests/fixtures/prtg/{name}.new",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::write(&path, &got).unwrap();
        panic!("the inventory changed, see {path}");
    }
}

#[tokio::test]
async fn a_recorded_install_through_the_api() {
    golden(
        &from_api().await,
        "api-inventory.json",
        include_str!("../../tests/fixtures/prtg/api-inventory.json"),
    );
}

#[tokio::test]
async fn hosts_and_their_groups() {
    let inv = from_api().await;
    let names: Vec<&str> = inv.hosts.iter().map(|h| h.name.as_str()).collect();
    // The switch is watched over SNMP and the branch file server by ping
    // only: both become devices (ADR-0148).
    assert_eq!(names, ["dc01", "web01", "switch01", "branch-fs"]);
    assert!(inv.host("device:2003").unwrap().agentless);
    assert!(!inv.host("device:2001").unwrap().agentless);
    let web = inv.host("device:2002").unwrap();
    assert_eq!(web.addresses, ["web01.example.net"]);
    assert_eq!(web.groups, ["Local Probe", "Servers", "Web"]);
    assert_eq!(
        inv.host("device:2001").unwrap().os,
        crate::model::Os::Windows
    );
}

#[tokio::test]
async fn checks_and_thresholds() {
    let inv = from_api().await;
    let watches: Vec<&Watch> = inv
        .checks
        .iter()
        .filter(|c| c.host == "device:2001")
        .map(|c| &c.watch)
        .collect();
    assert_eq!(watches[0], &Watch::Ping { address: None });
    assert!(watches.contains(&&Watch::Service {
        name: "W32Time".into()
    }));
    assert!(watches.contains(&&Watch::EventLog {
        channel: "System".into(),
        levels: vec!["error".into()],
        providers: vec![],
        event_ids: vec![],
    }));
    let find = |measure: Measure| {
        inv.thresholds
            .iter()
            .find(|t| t.measure == measure)
            .unwrap()
    };
    let cpu = find(Measure::CpuUsage);
    assert_eq!(
        (cpu.direction, cpu.warning, cpu.critical),
        (Direction::Above, Some(0.8), Some(0.95))
    );
    let memory = find(Measure::MemoryAvailable);
    assert_eq!(
        (memory.direction, memory.warning, memory.critical),
        (Direction::Below, Some(0.2), Some(0.1))
    );
    let c = find(Measure::DiskUsed {
        mount: Some("C:\\".into()),
    });
    assert_eq!((c.warning, c.critical), (Some(0.85), Some(0.95)));
    let var = find(Measure::DiskUsed {
        mount: Some("/var".into()),
    });
    assert_eq!(var.warning, Some(0.9));
    // The page and the certificate are both port 443 of web01: one check.
    let ports = inv
        .checks
        .iter()
        .filter(|c| matches!(c.watch, Watch::Port { port: 443, .. } | Watch::Web { .. }))
        .count();
    assert_eq!(ports, 1);
    assert!(inv
        .unmovable
        .iter()
        .any(|u| u.origin.name == "Certificate" && u.why.code == "prtg_same_port"));
    let lost: Vec<&str> = inv
        .unmovable
        .iter()
        .filter(|u| u.why.code == "prtg_limit")
        .map(|u| u.why.args["channel"].as_str())
        .collect();
    assert_eq!(lost, ["CPU 1", "C:\\ Free Bytes", "Response Time",]);
}

#[tokio::test]
async fn silences_follow_pauses_and_maintenance() {
    let inv = from_api().await;
    let window = inv
        .silences
        .iter()
        .find(|s| {
            s.reason.starts_with("Maintenance")
                && s.scope == SilenceScope::Hosts(vec!["device:2002".into()])
        })
        .unwrap();
    // 22:00 in PRTG's time, an hour ahead of UTC.
    assert_eq!(
        window.starts_at,
        Utc.with_ymd_and_hms(2026, 11, 20, 21, 0, 0).unwrap()
    );
    assert_eq!(
        window.ends_at,
        Utc.with_ymd_and_hms(2026, 11, 21, 1, 0, 0).unwrap()
    );
    assert_eq!(window.fidelity, Fidelity::Exact);
    let paused = inv
        .silences
        .iter()
        .find(|s| s.scope == SilenceScope::Hosts(vec!["device:2004".into()]))
        .unwrap();
    assert_eq!(
        paused.ends_at - paused.starts_at,
        chrono::Duration::days(30)
    );
    // The network group holds only the switch, now a device.
    let network = inv
        .silences
        .iter()
        .find(|s| s.origin.name == "Network")
        .unwrap();
    assert_eq!(
        network.scope,
        SilenceScope::Hosts(vec!["device:2003".into()])
    );
    let codes: Vec<&str> = inv.unmovable.iter().map(|u| u.why.code.as_str()).collect();
    assert!(codes.contains(&"prtg_paused_by_rule"));
    // A paused sensor silences its check, not its whole server.
    let sensor = inv
        .silences
        .iter()
        .find(|s| s.origin.name == "Disk /var")
        .unwrap();
    assert_eq!(sensor.checks, vec![crate::model::Watch::Disk]);
}

#[tokio::test]
async fn channels_from_notifications() {
    let inv = from_api().await;
    assert_eq!(inv.channels.len(), 2);
    assert_eq!(
        inv.channels[0].target,
        Target::Email {
            to: vec!["ops@example.com".into(), "oncall@example.com".into()],
            smtp: None
        }
    );
    assert!(
        matches!(&inv.channels[1].target, Target::Slack { url } if url.expose().starts_with("https://hooks.slack.com/"))
    );
    // The URL is a secret: never in debug output nor in the preview.
    assert!(!format!("{:?}", inv.channels[1]).contains("hooks.slack.com"));
    assert!(!serde_json::to_string(&inv)
        .unwrap()
        .contains("hooks.slack.com"));
}

#[tokio::test]
async fn the_configuration_file() {
    let input = Input::Files(vec![File {
        name: "PRTG Configuration.dat".into(),
        content: FILE.as_bytes().to_vec(),
    }]);
    let inv = Prtg.read(input, &Context::new(now())).await.unwrap();
    golden(
        &inv,
        "configuration-inventory.json",
        include_str!("../../tests/fixtures/prtg/configuration-inventory.json"),
    );
    let used = inv
        .thresholds
        .iter()
        .find(|t| matches!(t.measure, Measure::DiskUsed { .. }))
        .unwrap();
    assert_eq!(
        used.measure,
        Measure::DiskUsed {
            mount: Some("E:\\".into())
        }
    );
    assert_eq!((used.warning, used.critical), (Some(0.8), Some(0.9)));
    assert!(inv.checks.iter().any(|c| c.watch
        == Watch::EventLog {
            channel: "Application".into(),
            levels: vec!["critical".into(), "error".into()],
            providers: vec!["MSSQLSERVER".into()],
            event_ids: vec![17, 18],
        }));
    assert!(matches!(inv.channels[1].target, Target::Teams { .. }));
    let window = inv
        .silences
        .iter()
        .find(|s| s.reason.starts_with("Maintenance"))
        .unwrap();
    assert!(matches!(&window.fidelity, Fidelity::Approximated(n) if n.code == "prtg_local_time"));
}

#[test]
fn a_file_that_is_not_prtg_is_refused() {
    let read = |xml: &str| export::read("x.dat", xml, chrono::Duration::zero());
    assert!(read("<html><body/></html>").is_err());
    assert!(read("<prtg><nodes>").is_err());
    assert!(read("<a></a></b>").is_err());
}

#[test]
fn the_file_keeps_only_what_is_read() {
    let tree = export::read("x.dat", FILE, chrono::Duration::hours(1)).unwrap();
    let group = tree.get(50).unwrap();
    assert_eq!(group.name, "Servers & storage");
    assert_eq!(group.parent, Some(1));
    // The sensor's password is never kept.
    assert_eq!(
        tree.get(2103).unwrap().settings.keys().collect::<Vec<_>>(),
        ["service"]
    );
    assert_eq!(tree.get(2101).unwrap().interval_s, Some(30));
}

#[tokio::test]
async fn credentials_are_a_token_or_a_user_and_passhash() {
    let connect = |pairs: &[(&str, &str)]| {
        Input::Connect(Connection(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ))
    };
    let context = Context::new(now());
    for pairs in [
        &[("url", "https://prtg.example")][..],
        &[
            ("url", "https://prtg.example"),
            ("apitoken", "t"),
            ("username", "u"),
            ("passhash", "p"),
        ],
        &[("url", "https://prtg.example"), ("username", "u")],
    ] {
        let error = Prtg.read(connect(pairs), &context).await.unwrap_err();
        assert!(matches!(error, ReadError::Input(_)), "{pairs:?}: {error}");
    }
}

#[test]
fn values_as_prtg_writes_them() {
    assert_eq!(api::number("1,5"), Some(1.5));
    assert_eq!(api::number("1,234.5"), Some(1234.5));
    assert_eq!(api::number(""), None);
    assert_eq!(api::unit("2.048 MByte"), "MByte");
    assert_eq!(api::unit("< 1 %"), "%");
    assert_eq!(
        api::property("<prtg><result>a &amp; b</result></prtg>").as_deref(),
        Some("a & b")
    );
    assert_eq!(
        api::property("<prtg><result>(Property not found)</result></prtg>"),
        None
    );
    assert_eq!(api::unescape("&#233;t&#xE9; &bogus; &"), "été &bogus; &");
    let clock = api::clock("17.11.2026 13:00:00").unwrap();
    assert_eq!(
        api::utc_offset(clock, now() + chrono::Duration::seconds(20)),
        chrono::Duration::hours(1)
    );
    assert!(api::setting_date("2026-11-20-22-00-00").is_some());
}

#[test]
fn form_fields_skip_unchecked_boxes() {
    let fields = api::form_fields(
        r#"<input type="checkbox" name="a" value="1"><input type="checkbox" name="b" value="1" checked><input name="c" value="x &amp; y"><textarea name="d">body</textarea>"#,
    );
    let pairs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(pairs, [("b", "1"), ("c", "x & y"), ("d", "body")]);
}

#[test]
fn statuses_become_pauses() {
    let body = r#"{"devices":[{"objid":1,"name":"a","status_raw":7},{"objid":2,"name":"b","status_raw":9},{"objid":3,"name":"c","status_raw":3}]}"#;
    let objects = api::objects(body, Kind::Device).unwrap();
    let pauses: Vec<Option<Paused>> = objects.into_iter().map(|o| o.paused).collect();
    assert_eq!(
        pauses,
        [Some(Paused::Indefinitely), Some(Paused::ByRule), None]
    );
    assert!(api::objects("[]", Kind::Device).is_err());
}
