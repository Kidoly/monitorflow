//! The SNMP checks against a real net-snmp agent (ADR-0149), configured
//! as `deploy/probe-test/snmp/snmpd.conf` says. Skipped unless
//! `MF_SNMPD` names it as `host:port`; `deploy/probe-test/run-e2e.sh`
//! starts one.

use std::collections::BTreeMap;
use std::time::Duration;

use monitorflow_sensors::Sample;
use tokio::time::Instant;

fn target() -> Option<(String, String)> {
    let addr = std::env::var("MF_SNMPD").ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_owned(), port.to_owned()))
}

async fn collect(pairs: &[(&str, &str)]) -> Result<Vec<Sample>, String> {
    let (host, port) = target().unwrap();
    let mut params: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    params.insert("host".into(), host);
    params.insert("port".into(), port);
    let sensor = monitorflow_snmp::build(&params)?;
    sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())
}

fn metric<'a>(samples: &'a [Sample], name: &str) -> Vec<&'a Sample> {
    samples.iter().filter(|s| s.sensor == name).collect()
}

const V2C: (&str, &str) = ("community", "e2e-community");

fn v3(user: &str, auth: &str, privacy: &str, password: &str) -> Vec<(&'static str, String)> {
    vec![
        ("version", "v3".into()),
        ("user", user.into()),
        ("auth_protocol", auth.into()),
        ("auth_password", password.into()),
        ("priv_protocol", privacy.into()),
        ("priv_password", "e2e-priv-password".into()),
    ]
}

#[tokio::test]
async fn net_snmp_answers_every_group_it_has() {
    if target().is_none() {
        eprintln!("MF_SNMPD is not set; skipped");
        return;
    }
    let system = collect(&[V2C]).await.unwrap();
    assert_eq!(metric(&system, "up")[0].value, 1.0);
    assert!(metric(&system, "uptime_seconds")[0].value >= 0.0);

    // Interfaces: eth0 is up; rates come with the second collection.
    let (host, port) = target().unwrap();
    let params: BTreeMap<String, String> = [
        ("host", host.as_str()),
        ("port", port.as_str()),
        V2C,
        ("what", "interfaces"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let sensor = monitorflow_snmp::build(&params).unwrap();
    let first = sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    let up: Vec<_> = metric(&first, "oper_up")
        .iter()
        .map(|s| (s.instance.clone(), s.value))
        .collect();
    assert!(
        up.iter()
            .any(|(name, v)| name.starts_with("eth") && *v == 1.0),
        "{up:?}"
    );
    assert!(metric(&first, "in_bytes_per_second").is_empty());
    assert!(
        !up.iter().any(|(name, _)| name == "lo"),
        "loopback left out"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let second = sensor
        .collect(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    assert!(
        !metric(&second, "in_bytes_per_second").is_empty(),
        "{second:?}"
    );

    let host_resources = collect(&[V2C, ("what", "host")]).await.unwrap();
    // A fresh agent may have no processor load yet.
    assert!(metric(&host_resources, "cpu_usage_ratio")
        .iter()
        .all(|s| (0.0..=1.0).contains(&s.value)));
    assert!((0.0..=1.0).contains(&metric(&host_resources, "memory_used_ratio")[0].value));
    let disks = metric(&host_resources, "storage_size_bytes");
    assert!(disks.iter().any(|s| s.value > 1e9), "{host_resources:?}");
    assert!(!disks
        .iter()
        .any(|s| s.instance.starts_with("/proc") || s.instance.starts_with("/sys")));
    let sizes: std::collections::BTreeSet<_> = disks.iter().map(|s| s.value.to_bits()).collect();
    assert_eq!(sizes.len(), disks.len(), "one volume once: {disks:?}");

    let detected = collect(&[V2C, ("what", "detect")]).await.unwrap();
    let answers = metric(&detected, "answers")[0];
    assert!(answers.labels["descr"].starts_with("Linux"), "{answers:?}");
    assert_eq!(answers.labels["name"], "nas-e2e");
    let has: BTreeMap<_, _> = metric(&detected, "has")
        .iter()
        .map(|s| (s.instance.as_str(), s.value))
        .collect();
    assert_eq!(
        (has["interfaces"], has["host"], has["ups"], has["printer"]),
        (1.0, 1.0, 0.0, 0.0),
        "{has:?}"
    );
}

#[tokio::test]
async fn v3_with_authentication_and_privacy() {
    if target().is_none() {
        return;
    }
    for (user, auth, privacy) in [
        ("e2e-sha256", "sha256", "aes128"),
        ("e2e-sha512", "sha512", "aes256"),
    ] {
        let pairs = v3(user, auth, privacy, "e2e-auth-password");
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let samples = collect(&pairs).await.unwrap();
        assert_eq!(metric(&samples, "up")[0].value, 1.0, "{user}");
        let mut detect = pairs.clone();
        detect.push(("what", "detect"));
        let detected = collect(&detect).await.unwrap();
        assert_eq!(
            metric(&detected, "answers")[0].labels["name"],
            "nas-e2e",
            "{user}"
        );
    }
    // Every group reads over v3 as over v2c.
    let pairs = v3("e2e-sha512", "sha512", "aes256", "e2e-auth-password");
    let mut host: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    host.push(("what", "host"));
    let samples = collect(&host).await.unwrap();
    assert!(
        !metric(&samples, "memory_used_ratio").is_empty(),
        "{samples:?}"
    );
    // A wrong password gets no answer: the system check says down.
    let wrong = v3("e2e-sha256", "sha256", "aes128", "not-the-password");
    let wrong: Vec<(&str, &str)> = wrong.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let samples = collect(&wrong).await.unwrap();
    assert_eq!(metric(&samples, "up")[0].value, 0.0);
    // And a wrong community.
    let samples = collect(&[("community", "nope")]).await.unwrap();
    assert_eq!(metric(&samples, "up")[0].value, 0.0);
}
