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
    // The page and the certificate of web01 are one website check, with
    // the page's response time and the certificate's days limits.
    let url = "https://web01.example.net/health";
    let pages: Vec<&Watch> = inv
        .checks
        .iter()
        .map(|c| &c.watch)
        .filter(|w| matches!(w, Watch::Web(_)))
        .collect();
    assert_eq!(pages, [&Watch::web(crate::model::Website::new(url))]);
    let response = find(Measure::WebResponseTime { url: url.into() });
    assert_eq!(
        (response.direction, response.warning, response.critical),
        (Direction::Above, Some(0.5), None)
    );
    let days = find(Measure::CertificateDays { url: url.into() });
    assert_eq!(
        (days.direction, days.warning, days.critical),
        (Direction::Below, Some(30.0), Some(7.0))
    );
    assert_eq!(days.origin.name, "Certificate");
    let lost: Vec<&str> = inv
        .unmovable
        .iter()
        .filter(|u| u.why.code == "prtg_limit")
        .map(|u| u.why.args["channel"].as_str())
        .collect();
    assert_eq!(lost, ["CPU 1", "C:\\ Free Bytes"]);
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
        // A proxy user without its password.
        &[
            ("url", "https://prtg.example"),
            ("apitoken", "t"),
            ("proxy_user", "u"),
        ],
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
    // A channel without a lookup, as PRTG 24.3 answers: once read as a
    // lookup named "not found", missing from every file.
    assert_eq!(
        api::property("<prtg><version>24.3.98.1210</version><result>Not found</result></prtg>"),
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

/// Custom SNMP sensors of each kind, with lookup files beside the
/// configuration file (ADR-0157).
#[tokio::test]
async fn custom_snmp_sensors_with_their_lookup_files() {
    use crate::model::{Num, SnmpState, SnmpValue};
    let file = |name: &str, text: &str| File {
        name: name.into(),
        content: text.as_bytes().to_vec(),
    };
    let input = Input::Files(vec![
        file(
            "PRTG Configuration.dat",
            include_str!("../../tests/fixtures/prtg/configuration-snmp.dat"),
        ),
        file(
            "raid.ovl",
            include_str!("../../tests/fixtures/prtg/raid.ovl"),
        ),
        file("ha.ovl", include_str!("../../tests/fixtures/prtg/ha.ovl")),
    ]);
    let inv = Prtg.read(input, &Context::new(now())).await.unwrap();
    let custom = |name: &str| {
        let check = inv
            .checks
            .iter()
            .find(|c| c.origin.name == name)
            .unwrap_or_else(|| panic!("no {name}: {:?}", inv.checks));
        match &check.watch {
            Watch::SnmpCustom(c) => ((**c).clone(), check.fidelity.clone(), check.interval_s),
            other => panic!("{other:?}"),
        }
    };
    let (sessions, fidelity, interval) = custom("Sessions");
    assert_eq!((fidelity, interval), (Fidelity::Exact, Some(60)));
    assert_eq!(sessions.channels[0].oid, "1.3.6.1.4.1.12356.101.4.1.8.0");
    assert_eq!(sessions.channels[0].unit.as_deref(), Some("sessions"));
    assert_eq!(
        sessions.channels[0].limits.as_ref().unwrap().critical,
        Some(Num(8000.0))
    );

    let (health, fidelity, _) = custom("Firewall health");
    assert_eq!(fidelity, Fidelity::Exact);
    let names: Vec<&str> = health.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["CPU", "Inbound", "HA state"]);
    assert_eq!(health.channels[0].unit.as_deref(), Some("%"));
    assert_eq!(health.channels[1].value, SnmpValue::Counter);
    assert_eq!(health.channels[1].multiply, Some(Num(8.0)));
    let ha = health.channels[2].lookup.as_ref().unwrap();
    assert_eq!((ha.entries.len(), ha.otherwise), (3, SnmpState::Critical));

    let (firmware, _, interval) = custom("Firmware");
    assert_eq!(interval, Some(300));
    assert_eq!(firmware.channels[0].expect.as_ref().unwrap().1, "v7.");

    let (raid, fidelity, _) = custom("RAID volumes");
    assert_eq!(fidelity, Fidelity::Exact);
    assert_eq!(raid.table.as_deref(), Some("1.3.6.1.4.1.6574.3.1.1"));
    assert_eq!(
        raid.name_column.as_deref(),
        Some("1.3.6.1.4.1.6574.3.1.1.2")
    );
    let status = raid.channels[0].lookup.as_ref().unwrap();
    assert_eq!(status.entries[2].max, Some(12));

    let (temp, _, _) = custom("Temperature");
    assert_eq!(temp.channels[0].divide, Some(Num(10.0)));

    // A lookup no file had: the check moves, and says to pick its names.
    let (_, fidelity, _) = custom("Power source");
    assert!(
        matches!(fidelity, Fidelity::Approximated(n) if n.code == "snmp_lookup_missing" && n.args["name"] == "oid.test.missing")
    );
    // Hosts watched by SNMP only are devices.
    assert!(inv.hosts.iter().all(|h| h.agentless), "{:?}", inv.hosts);
}

/// What a real PRTG answers without a filter: every node of the tree in
/// `probenodes`, devices and sensors under their probe.
#[test]
fn the_probe_table_keeps_children_of_the_root_only() {
    let body = r#"{"prtg-version":"24.3","treesize":4,"probenodes":[
        {"objid":0,"name":"Root","parentid":-1000,"status":3},
        {"objid":1,"name":"Local Probe","parentid":0,"status":3},
        {"objid":2001,"name":"Server","parentid":1,"status":3},
        {"objid":3001,"name":"Ping","parentid":2001,"status":3},
        {"objid":60,"name":"Older answer without parent","status":3}]}"#;
    let ids: Vec<i64> = api::objects(body, Kind::Probe)
        .unwrap()
        .iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(ids, [1, 60]);
}

#[test]
fn the_time_zone_comes_from_the_status() {
    // 18:46:21 local when it is 17:46:21 UTC: one hour ahead.
    let status = r#"{"Clock":"21/11/2026 18:46:21","jsClock":1795283181}"#;
    let far = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(
        api::status_offset(status, far),
        Some(chrono::Duration::hours(1))
    );
    // Without `jsClock`, against our clock.
    let status = r#"{"Clock":"11/17/2026 1:00:00 PM"}"#;
    assert_eq!(
        api::status_offset(status, now()),
        Some(chrono::Duration::hours(1))
    );
    assert_eq!(api::status_offset("<html>", now()), None);
}

/// Answers as [`Recorded`], but the status as `status` says.
struct NoStatus(Recorded, fn() -> ReadError);

#[async_trait::async_trait]
impl Fetch for NoStatus {
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError> {
        if path == "/api/status.json" {
            return Err((self.1)());
        }
        self.0.get(path, query).await
    }
}

#[tokio::test]
async fn a_missing_status_reads_windows_as_utc_but_a_refusal_stops() {
    let recorded = || Recorded(serde_json::from_str(API).unwrap());
    let missing = NoStatus(recorded(), || {
        ReadError::Connect("https://prtg.example.com/api/status.json answered 404.".into())
    });
    let tree = api::read(Arc::new(missing), now()).await.unwrap();
    assert!(tree.objects.iter().any(|o| o.kind == Kind::Probe));
    let refused = NoStatus(recorded(), || ReadError::Refused("refused".into()));
    let error = api::read(Arc::new(refused), now()).await.unwrap_err();
    assert!(matches!(error, ReadError::Refused(_)), "{error}");
}

#[test]
fn a_proxy_in_front_gets_basic_authentication() {
    let connection = |pairs: &[(&str, &str)]| {
        Connection(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    };
    let headers = super::proxy_auth(&connection(&[
        ("proxy_user", "Aladdin"),
        ("proxy_password", "open sesame"),
    ]))
    .unwrap();
    let value = &headers[reqwest::header::AUTHORIZATION];
    assert_eq!(value, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    // Kept out of debug output.
    assert!(value.is_sensitive());
    assert!(super::proxy_auth(&connection(&[])).unwrap().is_empty());
    assert!(super::proxy_auth(&connection(&[("proxy_password", "p")])).is_err());
}
