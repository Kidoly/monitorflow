//! The VictoriaMetrics writer against an in-process fake endpoint, and
//! optionally against a real VictoriaMetrics (`VICTORIAMETRICS_URL`).

mod fake_http;

use std::time::Duration;

use fake_http::FakeHttp;
use monitorflow_common::proto::v1::{Sample, SampleBatch};
use monitorflow_server::ingest::metrics::{decode, remote_write};
use monitorflow_server::ingest::{
    check_batch, Identity, RemoteWrite, RetrySettings, Series, Writer, WriterSettings,
};

fn settings(batch_size: usize, queue_capacity: usize) -> WriterSettings {
    WriterSettings {
        batch_size,
        flush_interval: Duration::from_millis(20),
        queue_capacity,
        retry: RetrySettings {
            attempts: 4,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(20),
        },
        report_interval: Duration::from_secs(3600),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sample(sensor: &str, instance: &str, ts: i64, value: f64) -> Sample {
    Sample {
        sensor: sensor.into(),
        instance: instance.into(),
        timestamp_unix_ms: ts,
        value,
        labels: [("device".to_string(), "sda".to_string())].into(),
    }
}

fn series(n: usize) -> Vec<Series> {
    let identity = Identity {
        peer: "web-01".into(),
        site: "paris".into(),
    };
    let ts = now_ms();
    let samples = (0..n)
        .map(|i| sample("cpu", &i.to_string(), ts, i as f64))
        .collect();
    check_batch(&identity, SampleBatch { samples }, ts).series
}

fn labels(ts: &remote_write::TimeSeries) -> Vec<(&str, &str)> {
    ts.labels
        .iter()
        .map(|l| (l.name.as_str(), l.value.as_str()))
        .collect()
}

fn decoded(fake: &FakeHttp) -> Vec<remote_write::TimeSeries> {
    fake.accepted()
        .iter()
        .flat_map(|r| decode(&r.body).unwrap().timeseries)
        .collect()
}

#[tokio::test]
async fn samples_arrive_as_remote_write_with_identity_labels() {
    let fake = FakeHttp::start().await;
    let writer = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), settings(100, 100));
    let identity = Identity {
        peer: "web-01".into(),
        site: "paris".into(),
    };
    let ts = now_ms() - 60_000;
    let mut forged = sample("disk_free_bytes", "/", ts, 1.0);
    forged.labels.insert("site".into(), "lyon".into());
    let batch = SampleBatch {
        samples: vec![
            sample("disk_free_bytes", "/var", ts, 42.5),
            forged,
            sample("cpu", "", ts + 1, f64::NAN),
        ],
    };
    let checked = check_batch(&identity, batch, now_ms());
    assert_eq!(checked.invalid, 2);
    let queue = writer.queue();
    queue.reject(checked.invalid);
    for s in checked.series {
        assert!(queue.push(s));
    }
    fake.wait_for("one write", |r| !r.is_empty()).await;
    let stats = writer.shutdown(Duration::from_secs(5)).await;
    assert_eq!((stats.written, stats.rejected), (1, 2));

    let request = &fake.received()[0];
    assert_eq!(request.path_and_query, "/api/v1/write");
    assert_eq!(request.headers["content-encoding"], "snappy");
    assert_eq!(request.headers["content-type"], "application/x-protobuf");
    let series = decoded(&fake);
    assert_eq!(series.len(), 1);
    assert_eq!(
        labels(&series[0]),
        [
            ("__name__", "monitorflow_disk_free_bytes"),
            ("device", "sda"),
            ("instance", "/var"),
            ("peer", "web-01"),
            ("site", "paris"),
        ]
    );
    assert_eq!(series[0].samples.len(), 1);
    assert_eq!(series[0].samples[0].timestamp, ts);
    assert_eq!(series[0].samples[0].value, 42.5);
}

#[tokio::test]
async fn server_errors_are_retried_until_accepted() {
    let fake = FakeHttp::start().await;
    fake.script(&[503, 500]);
    let writer = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), settings(3, 100));
    for s in series(3) {
        writer.queue().push(s);
    }
    fake.wait_for("an accepted write", |r| {
        r.iter().any(|r| r.status.is_success())
    })
    .await;
    let stats = writer.shutdown(Duration::from_secs(5)).await;
    let statuses: Vec<_> = fake.received().iter().map(|r| r.status.as_u16()).collect();
    assert_eq!(statuses, [503, 500, 204]);
    assert_eq!((stats.written, stats.failed), (3, 0));
    // Each attempt carried the same batch.
    let bodies: Vec<_> = fake.received().iter().map(|r| r.body.clone()).collect();
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(decoded(&fake).len(), 3);
}

#[tokio::test]
async fn client_errors_drop_the_batch_without_retrying() {
    let fake = FakeHttp::start().await;
    fake.script(&[400]);
    let writer = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), settings(2, 100));
    for s in series(2) {
        writer.queue().push(s);
    }
    let stats = writer.shutdown(Duration::from_secs(5)).await;
    assert_eq!(fake.received().len(), 1);
    assert_eq!((stats.written, stats.failed), (0, 2));
}

#[tokio::test]
async fn unreachable_backend_counts_failures() {
    // A port nothing listens on: every attempt is a connection error.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let writer = Writer::spawn(RemoteWrite::new(&url).unwrap(), settings(1, 10));
    writer.queue().push(series(1).remove(0));
    let stats = writer.shutdown(Duration::from_secs(5)).await;
    assert_eq!((stats.written, stats.failed), (0, 1));
}

#[tokio::test]
async fn a_full_queue_drops_new_samples_while_the_backend_is_slow() {
    let fake = FakeHttp::start().await;
    let gate = fake.hold();
    let writer = Writer::spawn(RemoteWrite::new(&fake.url).unwrap(), settings(1, 4));
    let queue = writer.queue();
    let mut all = series(20).into_iter();
    // The first sample goes out and waits in the fake.
    queue.push(all.next().unwrap());
    tokio::time::sleep(Duration::from_millis(50)).await;
    let pushed = all.map(|s| queue.push(s)).filter(|&ok| ok).count();
    assert_eq!(pushed, 4, "only the queue capacity is accepted");
    assert_eq!(queue.stats().dropped, 15);

    gate.add_permits(100);
    let stats = writer.shutdown(Duration::from_secs(5)).await;
    assert_eq!((stats.accepted, stats.written, stats.dropped), (5, 5, 15));
    assert_eq!(decoded(&fake).len(), 5);
}

/// Writes one series to the VictoriaMetrics at `VICTORIAMETRICS_URL` and
/// reads it back.
#[tokio::test]
#[cfg_attr(
    not(vm_tests),
    ignore = "VICTORIAMETRICS_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d victoriametrics` and export it from .env.example"
)]
async fn real_victoriametrics_stores_the_series() {
    let base = std::env::var("VICTORIAMETRICS_URL").unwrap();
    let base = base.trim_end_matches('/');
    let peer = format!("test-{}", uuid::Uuid::new_v4().simple());
    let identity = Identity {
        peer: peer.clone(),
        site: "test".into(),
    };
    let ts = now_ms();
    let batch = SampleBatch {
        samples: vec![sample("ingest_test_ratio", "x", ts, 0.75)],
    };
    let writer = Writer::spawn(RemoteWrite::new(base).unwrap(), settings(10, 10));
    for s in check_batch(&identity, batch, ts).series {
        writer.queue().push(s);
    }
    let stats = writer.shutdown(Duration::from_secs(10)).await;
    assert_eq!(stats.written, 1, "{stats:?}");

    let client = reqwest::Client::new();
    let query = format!("monitorflow_ingest_test_ratio{{peer=\"{peer}\"}}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        client
            .get(format!("{base}/internal/force_flush"))
            .send()
            .await
            .unwrap();
        let body = client
            .get(format!("{base}/api/v1/export"))
            .query(&[("match[]", query.as_str())])
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if !body.is_empty() {
            let line: serde_json::Value =
                serde_json::from_str(body.lines().next().unwrap()).unwrap();
            assert_eq!(line["metric"]["site"], "test");
            assert_eq!(line["metric"]["instance"], "x");
            assert_eq!(line["values"][0], 0.75);
            assert_eq!(line["timestamps"][0], ts);
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "series not found");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
