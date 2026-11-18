//! The API client against a small JSON-RPC server answering canned
//! results, as Zabbix 7.0 and 6.0 do.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::model::{Fidelity, Measure, Repeat, SilenceScope, Target, Watch};
use crate::zabbix::map;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A request the server got.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    params: Value,
    bearer: Option<String>,
    auth: Option<String>,
}

type Answers = fn(&str, &Value) -> Value;

/// Serves JSON-RPC on 127.0.0.1: `answers` gives the `result` of a
/// method, or `{"error": ...}` to answer an error.
async fn serve(answers: Answers) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                // Headers, then the body by its length.
                let (head, body_start) = loop {
                    let n = stream.read(&mut buffer).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buffer[..n]);
                    if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (String::from_utf8_lossy(&data[..at]).to_string(), at + 4);
                    }
                };
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        let (key, value) = l.split_once(':')?;
                        key.trim()
                            .eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_owned())
                    })
                };
                let length: usize = header("content-length").unwrap().parse().unwrap();
                while data.len() < body_start + length {
                    let n = stream.read(&mut buffer).await.unwrap();
                    data.extend_from_slice(&buffer[..n]);
                }
                let request: Value =
                    serde_json::from_slice(&data[body_start..body_start + length]).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                log.lock().unwrap().push(Seen {
                    method: method.clone(),
                    params: request["params"].clone(),
                    bearer: header("authorization"),
                    auth: request["auth"].as_str().map(str::to_owned),
                });
                let result = answers(&method, &request["params"]);
                let answer = match result.get("error") {
                    Some(error) => json!({"jsonrpc": "2.0", "error": error, "id": request["id"]}),
                    None => json!({"jsonrpc": "2.0", "result": result, "id": request["id"]}),
                };
                let body = answer.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    (format!("http://{address}/zabbix"), seen)
}

fn connection(url: &str) -> Connection {
    Connection(
        [
            ("url".to_owned(), url.to_owned()),
            ("token".to_owned(), TOKEN.to_owned()),
        ]
        .into(),
    )
}

fn context() -> Context {
    Context::new(
        DateTime::parse_from_rfc3339("2026-11-17T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    )
}

/// A small Zabbix: web-01 linked to the Linux template, a discovered file
/// system and an undiscovered disk prototype, a web scenario, a weekly
/// maintenance of its group, an email and a Teams media type; as 7.0
/// answers.
fn zabbix_7(method: &str, params: &Value) -> Value {
    answer("7.0.5", "hostgroups", method, params)
}

/// The same as 6.0 answers: host groups are `groups`.
fn zabbix_6(method: &str, params: &Value) -> Value {
    answer("6.0.30", "groups", method, params)
}

fn answer(version: &str, groups: &str, method: &str, params: &Value) -> Value {
    let for_templates = params.get("templateids").is_some();
    match method {
        "apiinfo.version" => json!(version),
        "hostgroup.get" => json!([{"groupid": "2", "name": "Linux servers"}]),
        "host.get" => json!([{
            "hostid": "10", "host": "web-01", "name": "Web 01", "status": "0",
            "interfaces": [{"type": "1", "ip": "192.0.2.10", "dns": "web-01.example.com", "useip": "1", "main": "1"}],
            "inventory": {"os": "Debian 12", "os_full": ""},
            "parentTemplates": [{"templateid": "100", "host": "Linux by Zabbix agent"}],
            (groups): [{"groupid": "2", "name": "Linux servers"}],
        }, {
            "hostid": "11", "host": "old-01", "name": "old-01", "status": "1",
            "interfaces": [], "inventory": [], "parentTemplates": [], (groups): [],
        }]),
        "template.get" => json!([
            {"templateid": "100", "host": "Linux by Zabbix agent", "parentTemplates": []},
            {"templateid": "101", "host": "Unused", "parentTemplates": []},
        ]),
        "item.get" if for_templates => json!([{"itemid": "1000", "hostid": "100"}]),
        "item.get" => json!([
            {"itemid": "2000", "hostid": "10", "name": "CPU utilization", "key_": "system.cpu.util",
             "delay": "0", "type": "18", "status": "0", "url": "", "templateid": "1000", "itemDiscovery": []},
            {"itemid": "2001", "hostid": "10", "name": "FS [/]: Space: Used, in %",
             "key_": "vfs.fs.dependent.size[/,pused]", "delay": "0", "type": "18", "status": "0",
             "url": "", "templateid": "0", "itemDiscovery": {"parent_itemid": "2100"}},
            {"itemid": "2002", "hostid": "10", "name": "PostgreSQL port", "key_": "net.tcp.port[,5432]",
             "delay": "30s", "type": "0", "status": "0", "url": "", "templateid": "0", "itemDiscovery": []},
        ]),
        "itemprototype.get" if for_templates => json!([
            {"itemid": "1100", "hostid": "100"},
            {"itemid": "1101", "hostid": "100"},
        ]),
        "itemprototype.get" => json!([
            {"itemid": "2100", "hostid": "10", "name": "FS [{#FSNAME}]: Space: Used, in %",
             "key_": "vfs.fs.dependent.size[{#FSNAME},pused]", "delay": "0", "type": "18",
             "status": "0", "url": "", "templateid": "1100"},
            {"itemid": "2101", "hostid": "10", "name": "{#DEVNAME}: Disk read request avg waiting time",
             "key_": "vfs.dev.read.await[{#DEVNAME}]", "delay": "0", "type": "15",
             "status": "0", "url": "", "templateid": "1101"},
        ]),
        "usermacro.get" if params.get("globalmacro").is_some() => {
            json!([{"macro": "{$DB.CPU.HIGH}", "value": "85", "type": "0"}])
        }
        "usermacro.get" => json!([
            {"hostid": "10", "macro": "{$CPU.UTIL.CRIT}", "value": "80", "type": "0"},
            {"hostid": "10", "macro": "{$DB.PASSWORD}", "type": "1"},
            {"hostid": "100", "macro": "{$CPU.UTIL.CRIT}", "value": "90", "type": "0"},
        ]),
        "trigger.get" => json!([
            {"triggerid": "3000", "description": "High CPU",
             "expression": "min(/web-01/system.cpu.util,5m)>80", "recovery_mode": "1",
             "recovery_expression": "max(/web-01/system.cpu.util,5m)<70", "priority": "4",
             "status": "0", "hosts": [{"hostid": "10"}], "triggerDiscovery": []},
            {"triggerid": "3001", "description": "FS [/]: Space is low",
             "expression": "min(/web-01/vfs.fs.dependent.size[/,pused],5m)>80", "recovery_mode": "0",
             "recovery_expression": "", "priority": "2", "status": "0",
             "hosts": [{"hostid": "10"}], "triggerDiscovery": {"parent_triggerid": "3100"}},
            {"triggerid": "3002", "description": "PostgreSQL is down",
             "expression": "max(/web-01/net.tcp.port[,5432],3m)=0", "recovery_mode": "0",
             "recovery_expression": "", "priority": "5", "status": "0",
             "hosts": [{"hostid": "10"}], "triggerDiscovery": [],
             "tags": [{"tag": "service", "value": "postgresql"}]},
        ]),
        "triggerprototype.get" => json!([
            {"triggerid": "3100", "description": "FS [{#FSNAME}]: Space is low",
             "expression": "min(/web-01/vfs.fs.dependent.size[{#FSNAME},pused],5m)>80",
             "recovery_mode": "0", "recovery_expression": "", "priority": "2", "status": "0",
             "hosts": [{"hostid": "10"}]},
            {"triggerid": "3101", "description": "{#DEVNAME}: Disk read is slow",
             "expression": "min(/web-01/vfs.dev.read.await[{#DEVNAME}],15m)>20",
             "recovery_mode": "0", "recovery_expression": "", "priority": "2", "status": "0",
             "hosts": [{"hostid": "10"}]},
        ]),
        "httptest.get" => json!([{
            "httptestid": "1", "hostid": "10", "name": "Shop", "delay": "1m", "status": "0",
            "steps": [{"no": "2", "url": "https://shop.example.com/cart"}, {"no": "1", "url": "https://shop.example.com/"}],
        }]),
        "maintenance.get" => json!([{
            "maintenanceid": "1", "name": "Patching", "description": "Kernel updates",
            "maintenance_type": "1", "active_since": "1767225600", "active_till": "1798761600",
            "hosts": [], (groups): [{"groupid": "2"}], "tags": [],
            "timeperiods": [{"timeperiod_type": "3", "every": "1", "month": "0", "dayofweek": "64",
                             "day": "0", "start_time": "7200", "period": "7200", "start_date": "0"}],
        }, {
            "maintenanceid": "2", "name": "Database work", "description": "",
            "maintenance_type": "0", "active_since": "1767225600", "active_till": "1798761600",
            "hosts": [{"hostid": "10"}], (groups): [], "tags_evaltype": "0",
            "tags": [{"tag": "service", "operator": "0", "value": "postgres"}],
            // The second Tuesday and Friday of January and July, 22:00 for 4 hours.
            "timeperiods": [{"timeperiod_type": "4", "every": "2", "month": "65", "dayofweek": "18",
                             "day": "0", "start_time": "79200", "period": "14400", "start_date": "0"}],
        }, {
            "maintenanceid": "3", "name": "Unknown tags", "description": "",
            "maintenance_type": "0", "active_since": "1767225600", "active_till": "1798761600",
            "hosts": [{"hostid": "10"}], (groups): [],
            "tags": [{"tag": "team", "operator": "2", "value": "web"}],
            "timeperiods": [{"timeperiod_type": "2", "every": "3", "month": "0", "dayofweek": "0",
                             "day": "0", "start_time": "0", "period": "3600", "start_date": "0"}],
        }]),
        "mediatype.get" => json!([
            {"mediatypeid": "1", "type": "0", "name": "Email", "smtp_server": "mail.example.com",
             "smtp_port": "25", "smtp_email": "zabbix@example.com", "smtp_authentication": "0",
             "username": "", "passwd": "", "status": "0", "parameters": []},
            {"mediatypeid": "2", "type": "4", "name": "MS Teams Workflow", "status": "0",
             "parameters": [{"name": "teams_endpoint", "value": "{ALERT.SENDTO}"}]},
            {"mediatypeid": "3", "type": "4", "name": "Jira", "status": "0",
             "parameters": [{"name": "jira_url", "value": "<PLACE YOUR JIRA URL>"}]},
        ]),
        "user.get" => json!([{
            "userid": "1", "username": "Admin",
            "medias": [
                {"mediatypeid": "1", "sendto": ["ops@example.com"], "active": "0"},
                {"mediatypeid": "2", "sendto": "https://example.webhook.office.com/webhookb2/1", "active": "0"},
            ],
        }]),
        other => panic!("unexpected call {other}"),
    }
}

#[tokio::test]
async fn zabbix_7_with_a_bearer_token() {
    let (url, seen) = serve(zabbix_7).await;
    let mut api = Api::connect(&connection(&url), &context()).await.unwrap();
    let config = api.read().await.unwrap();

    let seen = seen.lock().unwrap().clone();
    // Reads only, the version without the token, the rest with it as a
    // bearer token and never in the body.
    assert!(seen
        .iter()
        .all(|s| s.method == "apiinfo.version" || s.method.ends_with(".get")));
    assert_eq!(seen[0].method, "apiinfo.version");
    assert_eq!(seen[0].bearer, None);
    for s in &seen[1..] {
        assert_eq!(
            s.bearer.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert_eq!(s.auth, None);
    }
    let host_get = seen.iter().find(|s| s.method == "host.get").unwrap();
    assert!(host_get.params.get("selectHostGroups").is_some());

    assert!(config.inherited);
    let host = &config.hosts[0];
    assert_eq!(host.host, "web-01");
    assert_eq!(host.groups, ["Linux servers"]);
    assert_eq!(host.os.as_deref(), Some("Debian 12"));
    assert_eq!(host.templates, ["Linux by Zabbix agent"]);
    assert!(host.interfaces[0].use_ip);
    assert_eq!(
        host.macros,
        [
            Macro {
                name: "{$CPU.UTIL.CRIT}".into(),
                value: Some("80".into())
            },
            Macro {
                name: "{$DB.PASSWORD}".into(),
                value: None
            }
        ]
    );
    assert!(!config.hosts[1].enabled);
    // The discovered file system replaces its prototype; the disk
    // prototype discovered nothing and stays.
    let keys: Vec<_> = host.items.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "system.cpu.util",
            "vfs.fs.dependent.size[/,pused]",
            "net.tcp.port[,5432]",
            "vfs.dev.read.await[{#DEVNAME}]"
        ]
    );
    let template = Some("Linux by Zabbix agent".to_owned());
    assert_eq!(host.items[0].template, template);
    assert_eq!(host.items[1].template, template, "through its prototype");
    assert_eq!(host.items[2].template, None);
    assert!(host.items[3].prototype);
    let triggers: Vec<_> = host.triggers.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        triggers,
        [
            "High CPU",
            "FS [/]: Space is low",
            "PostgreSQL is down",
            "{#DEVNAME}: Disk read is slow"
        ]
    );
    assert_eq!(
        host.web[0].urls,
        ["https://shop.example.com/", "https://shop.example.com/cart"]
    );
    assert_eq!(
        config.templates.len(),
        1,
        "templates no host uses are left out"
    );
    assert_eq!(
        config.templates["Linux by Zabbix agent"].macros[0]
            .value
            .as_deref(),
        Some("90")
    );
    assert_eq!(config.global_macros[0].name, "{$DB.CPU.HIGH}");
    assert_eq!(config.maintenances[0].groups, ["Linux servers"]);

    // And what it becomes.
    let inventory = map::inventory(&config, context().now);
    assert_eq!(inventory.hosts.len(), 1);
    assert_eq!(inventory.unmovable[0].origin.name, "old-01");
    let cpu = inventory
        .thresholds
        .iter()
        .find(|t| t.measure == Measure::CpuUsage)
        .unwrap();
    assert_eq!(
        (cpu.critical, cpu.for_s, cpu.recovery),
        (Some(0.8), 300, Some(0.7))
    );
    assert!(inventory.thresholds.iter().any(|t| t.measure
        == Measure::DiskUsed {
            mount: Some("/".into())
        }
        && t.warning == Some(0.8)));
    assert!(inventory
        .thresholds
        .iter()
        .any(|t| t.measure == Measure::DiskWait { device: None } && t.warning == Some(0.02)));
    let port = inventory
        .checks
        .iter()
        .find(|c| matches!(c.watch, Watch::Port { port: 5432, .. }))
        .unwrap();
    assert_eq!(port.interval_s, Some(30));
    assert_eq!(port.alert.as_ref().unwrap().for_s, 180);
    // Weekly on Sundays at 02:00: a maintenance window, not its next one.
    let window = &inventory.windows[0];
    assert_eq!(window.scope, SilenceScope::Hosts(vec!["web-01".into()]));
    assert_eq!(window.reason, "Patching: Kernel updates");
    assert_eq!(window.rule.repeat, Repeat::Weekly);
    assert_eq!(window.rule.weekdays, 64);
    assert_eq!(window.rule.start_time.to_string(), "02:00:00");
    assert_eq!(window.rule.time_zone, None);
    // Without data collection in Zabbix: MonitorFlow only holds notifications.
    assert!(matches!(&window.fidelity, Fidelity::Approximated(n) if n.code == "zabbix_no_data"));
    assert!(inventory.silences.is_empty());
    // Limited to tags: the check made from the trigger with them, on the
    // second Tuesday and the second Friday, one window each.
    let work: Vec<_> = inventory
        .windows
        .iter()
        .filter(|w| w.reason == "Database work")
        .collect();
    assert_eq!(work.len(), 2);
    for w in &work {
        assert_eq!(
            w.checks,
            vec![Watch::Port {
                address: None,
                port: 5432
            }]
        );
        assert_eq!(
            (w.rule.repeat, w.rule.month_week, w.rule.months),
            (Repeat::Monthly, Some(2), 65)
        );
        assert_eq!(w.rule.duration_minutes, 240);
        assert_eq!(w.fidelity, Fidelity::Exact);
    }
    let days: Vec<u8> = work.iter().map(|w| w.rule.weekdays).collect();
    assert_eq!(days, vec![2, 16]);
    // Tags no check comes from: the whole server, said so.
    let unknown = inventory
        .windows
        .iter()
        .find(|w| w.reason == "Unknown tags")
        .unwrap();
    assert!(unknown.checks.is_empty());
    assert_eq!(
        (unknown.rule.repeat, unknown.rule.every),
        (Repeat::Daily, 3)
    );
    assert!(
        matches!(&unknown.fidelity, Fidelity::Approximated(n) if n.code == "zabbix_tags_unmatched")
    );
    let names: Vec<_> = inventory.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Email", "MS Teams Workflow (Admin)"]);
    assert!(
        matches!(&inventory.channels[0].target, Target::Email { to, .. } if to == &["ops@example.com"])
    );
    // Jira is not used by anyone: left out.
    assert!(!inventory.unmovable.iter().any(|u| u.origin.name == "Jira"));
}

#[tokio::test]
async fn zabbix_6_0_with_the_token_in_the_request() {
    let (url, seen) = serve(zabbix_6).await;
    let mut api = Api::connect(&connection(&format!("{url}/api_jsonrpc.php")), &context())
        .await
        .unwrap();
    let config = api.read().await.unwrap();
    let seen = seen.lock().unwrap().clone();
    for s in &seen[1..] {
        assert_eq!(s.bearer, None);
        assert_eq!(s.auth.as_deref(), Some(TOKEN));
    }
    let host_get = seen.iter().find(|s| s.method == "host.get").unwrap();
    assert!(host_get.params.get("selectGroups").is_some());
    let maintenance_get = seen.iter().find(|s| s.method == "maintenance.get").unwrap();
    assert!(maintenance_get.params.get("selectGroups").is_some());
    assert_eq!(config.hosts[0].groups, ["Linux servers"]);
    assert_eq!(config.maintenances[0].groups, ["Linux servers"]);
}

#[tokio::test]
async fn a_refused_token_is_not_shown() {
    let (url, _) = serve(|method, _| match method {
        "apiinfo.version" => json!("7.0.5"),
        _ => json!({"error": {"code": -32602, "message": "Invalid params.", "data": "Not authorized."}}),
    })
    .await;
    let mut api = Api::connect(&connection(&url), &context()).await.unwrap();
    let error = api.read().await.unwrap_err();
    assert_eq!(
        error,
        ReadError::Refused("Zabbix refused the token: Invalid params. Not authorized.".into())
    );
    assert!(!error.to_string().contains(TOKEN));

    // A Zabbix echoing the token has it hidden.
    let (url, _) = serve(|method, _| match method {
        "apiinfo.version" => json!("6.0.30"),
        _ => json!({"error": {"code": -32500, "message": "Application error.",
                              "data": format!("No session {TOKEN}.")}}),
    })
    .await;
    let mut api = Api::connect(&connection(&url), &context()).await.unwrap();
    let error = api.read().await.unwrap_err().to_string();
    assert!(!error.contains(TOKEN), "{error}");
    assert!(error.contains("[token]"), "{error}");
}

#[tokio::test]
async fn other_servers_are_refused() {
    let (url, _) = serve(|_, _| json!("5.0.0")).await;
    // Answers a version, but too old.
    let (old, _) = serve(|_, _| json!("4.0.40")).await;
    let error = Api::connect(&connection(&old), &context())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("too old"), "{error}");
    assert!(Api::connect(&connection(&url), &context()).await.is_ok());
    // Nothing listening.
    let error = Api::connect(&connection("http://127.0.0.1:9"), &context())
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ReadError::Connect(_)), "{error}");
}

#[test]
fn endpoints() {
    let cases = [
        (
            "zabbix.example.com",
            "https://zabbix.example.com/api_jsonrpc.php",
        ),
        (
            "http://zabbix.example.com/",
            "http://zabbix.example.com/api_jsonrpc.php",
        ),
        (
            "https://x.example/zabbix",
            "https://x.example/zabbix/api_jsonrpc.php",
        ),
        (
            "https://x.example/zabbix/",
            "https://x.example/zabbix/api_jsonrpc.php",
        ),
        (
            "https://x.example/zabbix/api_jsonrpc.php",
            "https://x.example/zabbix/api_jsonrpc.php",
        ),
        (
            "https://x.example/zabbix.php?action=dashboard.view",
            "https://x.example/api_jsonrpc.php",
        ),
        (
            "https://x.example:8443/index.php",
            "https://x.example:8443/api_jsonrpc.php",
        ),
    ];
    for (typed, expected) in cases {
        assert_eq!(endpoint(typed).unwrap().as_str(), expected, "{typed}");
    }
    assert!(endpoint("ftp://x.example").is_err());
}
