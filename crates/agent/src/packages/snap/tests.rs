//! snapd's answers, from the fixtures in `../fixtures`: the REST API's
//! shapes do not differ across the supported distributions, as snapd
//! re-executes from its own snap where it can, except that snapd before
//! `tracking-channel` only wrote `channel`.

use super::*;

const SNAPS: &str = include_str!("../fixtures/snapd-snaps.json");
const REFRESH: &str = include_str!("../fixtures/snapd-refresh.json");
const SYSTEM_INFO: &str = include_str!("../fixtures/snapd-system-info.json");

#[test]
fn installed_snaps_get_their_refresh() {
    let installed: Vec<Installed> = parse_body(SNAPS.as_bytes()).unwrap();
    let refreshes: Vec<Refresh> = parse_body(REFRESH.as_bytes()).unwrap();
    let apps = merge(installed, refreshes);
    let app =
        |id: &str, name: &str, origin: &str, version: &str, available: &str| ThirdPartyPackage {
            manager: PackageManager::Snap.into(),
            id: id.into(),
            name: name.into(),
            version: version.into(),
            available_version: available.into(),
            scope: PackageScope::Machine.into(),
            origin: origin.into(),
        };
    assert_eq!(
        apps,
        [
            // Same version, new revision: the revision tells it apart.
            app(
                "example-base",
                "example-base",
                "latest/stable",
                "20260101",
                "20260101 (revision 1380)"
            ),
            app("example-app", "example-app", "2/stable", "1.2.0", "1.3.1"),
            // An instance (`name_key`), sideloaded: no refresh.
            app(
                "example-tool_test",
                "example-tool",
                "latest/edge",
                "0.9",
                ""
            ),
        ]
    );
}

#[test]
fn older_snapd_has_only_the_channel_and_numeric_revisions_pass() {
    let body = r#"{"type":"sync","result":[{"name":"example-app","version":"1","revision":3,"channel":"stable"}]}"#;
    let installed: Vec<Installed> = parse_body(body.as_bytes()).unwrap();
    let refresh = r#"{"type":"sync","result":[{"name":"example-app","version":"1","revision":4}]}"#;
    let refreshes: Vec<Refresh> = parse_body(refresh.as_bytes()).unwrap();
    let apps = merge(installed, refreshes);
    assert_eq!(apps[0].origin, "stable");
    assert_eq!(apps[0].available_version, "1 (revision 4)");
    assert_eq!(apps[0].name, "");
}

#[test]
fn names_outside_snapds_grammar_are_dropped() {
    let body = r#"{"type":"sync","result":[{"name":"-y"},{"name":"Bad"},{"name":"ok"}]}"#;
    let installed: Vec<Installed> = parse_body(body.as_bytes()).unwrap();
    let apps = merge(installed, Vec::new());
    let names: Vec<_> = apps.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(names, ["ok"]);
}

#[test]
fn errors_and_odd_answers() {
    let not_found = r#"{"type":"error","status-code":404,"status":"Not Found","result":{"message":"no snap updates available","kind":"snap-not-found"}}"#;
    assert_eq!(
        parse_body::<Vec<Refresh>>(not_found.as_bytes()).unwrap_err(),
        SnapdError::Api {
            kind: "snap-not-found".into(),
            message: "no snap updates available".into()
        }
    );
    let async_answer = r#"{"type":"async","change":"1","result":null}"#;
    assert!(matches!(
        parse_body::<Vec<Refresh>>(async_answer.as_bytes()),
        Err(SnapdError::Malformed(_))
    ));
    assert!(matches!(
        parse_body::<Vec<Refresh>>(b"<html>"),
        Err(SnapdError::Malformed(_))
    ));
}

#[test]
fn refresh_schedule_from_system_info() {
    let info: SystemInfo = parse_body(SYSTEM_INFO.as_bytes()).unwrap();
    assert_eq!(info.version, "2.67");
    assert_eq!(info.refresh.timer, "00:00~24:00/4");
    assert_eq!(info.refresh.hold, "2026-11-20T08:00:00Z");
    assert_eq!(info.refresh.last, "2026-11-15T21:14:00Z");
    let legacy = r#"{"type":"sync","result":{"refresh":{"schedule":"00:00-04:59/5:00-10:59","next":"2026-11-16T03:00:00Z"}}}"#;
    let info: SystemInfo = parse_body(legacy.as_bytes()).unwrap();
    assert_eq!(info.refresh.schedule, "00:00-04:59/5:00-10:59");
    assert_eq!(info.refresh.hold, "");
}

#[test]
fn http_answers_split_at_the_body() {
    let answer = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}".to_vec();
    assert_eq!(split_response(answer).unwrap(), b"{\"a\":1}");
    let error = b"HTTP/1.0 404 Not Found\r\n\r\n{}".to_vec();
    assert_eq!(split_response(error).unwrap(), b"{}");
    for bad in [
        &b"HTTP/1.0 200 OK\r\n"[..],
        b"SSH-2.0\r\n\r\n{}",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n7\r\n{}",
    ] {
        assert!(matches!(
            split_response(bad.to_vec()),
            Err(SnapdError::Malformed(_))
        ));
    }
}
