use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use tokio::net::{TcpListener, TcpSocket};

use super::*;
use crate::registry;

fn local(port: u16) -> PortSensor {
    PortSensor::new(Host::Ip(IpAddr::from([127, 0, 0, 1])), port)
}

fn in_ms(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}

fn values(samples: &[Sample]) -> BTreeMap<&str, f64> {
    samples
        .iter()
        .map(|s| (s.sensor.as_str(), s.value))
        .collect()
}

/// A port nothing listens on: bound, then released.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn hosts_and_targets() {
    let v4 = parse_host("10.0.0.1").unwrap();
    assert_eq!(target(&v4, 22), "10.0.0.1:22");
    for v6 in ["::1", "[::1]"] {
        assert_eq!(target(&parse_host(v6).unwrap(), 443), "[::1]:443");
    }
    let name = parse_host("db-1.example.com").unwrap();
    assert_eq!(name, Host::Name("db-1.example.com".into()));
    assert_eq!(target(&name, 5432), "db-1.example.com:5432");
    let long = format!("{}.com", "a".repeat(HOST_MAX));
    for bad in [
        "",
        "a..b",
        "-a.com",
        "a b",
        "a/b",
        "http://a",
        "a:22",
        "é.com",
        "a\n",
        long.as_str(),
    ] {
        assert!(parse_host(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn params_are_checked() {
    let build = |pairs: &[(&str, &str)]| {
        let params = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        registry::build("port", &params).map(|_| ())
    };
    assert_eq!(build(&[("port", "22")]), Ok(()));
    assert_eq!(
        build(&[("port", " 443 "), ("host", "[::1]"), ("protocol", "tcp")]),
        Ok(())
    );
    for (pairs, reason) in [
        (&[][..], "`port` is required"),
        (&[("port", "0")][..], "between 1 and 65535"),
        (&[("port", "65536")][..], "between 1 and 65535"),
        (&[("port", "ssh")][..], "between 1 and 65535"),
        (&[("port", "53"), ("protocol", "udp")][..], "not supported"),
        (&[("port", "53"), ("protocol", "sctp")][..], "must be `tcp`"),
        (&[("port", "22"), ("host", "a b")][..], "neither"),
        (&[("port", "22"), ("timeout", "1")][..], "unknown parameter"),
    ] {
        let error = build(pairs).unwrap_err().to_string();
        assert!(error.contains(reason), "{pairs:?}: {error}");
    }
}

#[tokio::test]
async fn an_open_port_is_up_with_its_connect_time() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let samples = local(port).collect(in_ms(2000)).await.unwrap();
    let v = values(&samples);
    assert_eq!(v["up"], 1.0);
    assert!((0.0..2.0).contains(&v["connect_seconds"]), "{v:?}");
    for sample in &samples {
        assert_eq!(sample.labels["target"], format!("127.0.0.1:{port}"));
        assert!(sample.instance.is_empty());
        crate::validate::sample(sample).unwrap();
        crate::validate::sensor_name(&crate::metric_name("port", &sample.sensor)).unwrap();
    }
}

#[tokio::test]
async fn a_closed_port_is_down() {
    let samples = local(closed_port().await)
        .collect(in_ms(2000))
        .await
        .unwrap();
    assert_eq!(values(&samples), BTreeMap::from([("up", 0.0)]));
}

#[tokio::test]
async fn ipv6_when_the_host_has_it() {
    let Ok(listener) = TcpListener::bind("[::1]:0").await else {
        eprintln!("no IPv6 loopback, skipped");
        return;
    };
    let port = listener.local_addr().unwrap().port();
    let sensor = PortSensor::new(Host::Ip(Ipv6Addr::LOCALHOST.into()), port);
    let samples = sensor.collect(in_ms(2000)).await.unwrap();
    assert_eq!(values(&samples)["up"], 1.0);
    assert_eq!(samples[0].labels["target"], format!("[::1]:{port}"));
}

#[tokio::test]
async fn localhost_is_resolved() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sensor = PortSensor::new(Host::Name("localhost".into()), port);
    // localhost may resolve to ::1 first, where nothing listens: the next
    // address is tried.
    let samples = sensor.collect(in_ms(2000)).await.unwrap();
    assert_eq!(values(&samples)["up"], 1.0);
    assert!(!sensor.resolving.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_lookup_still_running_reports_down_without_another() {
    let sensor = PortSensor::new(Host::Name("localhost".into()), 1);
    sensor.resolving.store(true, Ordering::SeqCst);
    let samples = sensor.collect(in_ms(2000)).await.unwrap();
    assert_eq!(values(&samples), BTreeMap::from([("up", 0.0)]));
    assert!(sensor.resolving.load(Ordering::SeqCst));
}

/// A listener whose accept queue is full: Linux drops further SYNs, so
/// the next connect hangs until it times out.
async fn hanging_port() -> Option<(TcpListener, Vec<TcpStream>, u16)> {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(0).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut held = Vec::new();
    for _ in 0..16 {
        match timeout_at(in_ms(100), TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => held.push(stream),
            Ok(Err(_)) => return None,
            Err(_) => return Some((listener, held, addr.port())),
        }
    }
    None
}

#[tokio::test]
async fn a_hanging_connect_is_down_before_the_deadline() {
    let Some((_listener, _held, port)) = hanging_port().await else {
        eprintln!("could not fill the accept queue, skipped");
        return;
    };
    let started = Instant::now();
    let deadline = in_ms(600);
    let samples = local(port).collect(deadline).await.unwrap();
    assert_eq!(values(&samples), BTreeMap::from([("up", 0.0)]));
    // Answered with the margin to spare, not cancelled at the deadline.
    assert!(Instant::now() < deadline, "{:?}", started.elapsed());
}

#[tokio::test]
async fn a_spent_deadline_still_answers() {
    let samples = local(closed_port().await)
        .collect(Instant::now())
        .await
        .unwrap();
    assert_eq!(values(&samples), BTreeMap::from([("up", 0.0)]));
}
