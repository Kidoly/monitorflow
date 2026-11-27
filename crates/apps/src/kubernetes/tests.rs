//! The Kubernetes check against a fake API server over TLS, signed by a
//! CA of its own, as a cluster's is.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use super::*;
use crate::{build, open_into, seal_form, secret_params, KUBERNETES};

const TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.fake-service-account-token.sig";

/// What the fake cluster holds; tests change it between collections.
#[derive(Clone)]
struct Cluster {
    pods: Value,
    /// Paths answered 403.
    forbidden: Vec<&'static str>,
    /// Requests seen, path and query.
    seen: Vec<String>,
}

fn node(name: &str, ready: &str, pressure: &str, role: &str) -> Value {
    json!({
        "metadata": {"name": name, "labels": {format!("node-role.kubernetes.io/{role}"): ""}},
        "spec": {},
        "status": {
            "nodeInfo": {"kubeletVersion": "v1.31.4"},
            "conditions": [
                {"type": "Ready", "status": ready},
                {"type": "MemoryPressure", "status": pressure},
                {"type": "DiskPressure", "status": "False"},
                {"type": "PIDPressure", "status": "False"},
            ]
        }
    })
}

fn pod(ns: &str, name: &str, phase: &str, ready: bool, restarts: u32) -> Value {
    json!({
        "metadata": {"namespace": ns, "name": name, "uid": format!("uid-{name}")},
        "status": {
            "phase": phase,
            "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}],
            "containerStatuses": [{"name": "main", "restartCount": restarts, "state": {}}],
        }
    })
}

fn crashing(ns: &str, name: &str, restarts: u32) -> Value {
    let mut p = pod(ns, name, "Running", false, restarts);
    p["status"]["containerStatuses"][0]["state"] =
        json!({"waiting": {"reason": "CrashLoopBackOff"}});
    p
}

fn initial() -> Cluster {
    Cluster {
        pods: json!([
            pod("shop", "web-1", "Running", true, 0),
            crashing("shop", "api-1", 4),
            pod("shop", "setup-1", "Succeeded", false, 0),
            pod("kube-system", "dns-1", "Running", true, 1),
            pod("kube-system", "pending-1", "Pending", false, 0),
        ]),
        forbidden: Vec::new(),
        seen: Vec::new(),
    }
}

fn answer_for(cluster: &mut Cluster, path: &str, query: &str) -> (u16, Value) {
    cluster.seen.push(format!("{path}?{query}"));
    if cluster.forbidden.contains(&path) {
        return (403, json!({"kind": "Status", "code": 403}));
    }
    let list = |items: Value| json!({"metadata": {}, "items": items});
    let body = match path {
        "/version" => json!({"gitVersion": "v1.31.4+k3s1"}),
        "/api/v1/nodes" => list(json!([
            node("cp-1", "True", "False", "control-plane"),
            node("worker-1", "Unknown", "True", "worker"),
        ])),
        // Two pages: the first stops after two pods.
        "/api/v1/pods" => {
            let pods = cluster.pods.as_array().unwrap().clone();
            if query.contains("continue=") {
                assert!(query.contains("continue=next%3D%3D"), "{query}");
                list(Value::Array(pods[2..].to_vec()))
            } else {
                assert!(query.contains("limit=500"), "{query}");
                json!({"metadata": {"continue": "next=="}, "items": Value::Array(pods[..2].to_vec())})
            }
        }
        "/apis/apps/v1/deployments" => list(json!([
            {"metadata": {"namespace": "shop", "name": "web"}, "spec": {"replicas": 3}, "status": {"readyReplicas": 1}},
            {"metadata": {"namespace": "shop", "name": "idle"}, "spec": {"replicas": 0}, "status": {}},
        ])),
        "/apis/apps/v1/statefulsets" => list(json!([
            {"metadata": {"namespace": "shop", "name": "db"}, "spec": {}, "status": {"readyReplicas": 1}},
        ])),
        "/apis/apps/v1/daemonsets" => list(json!([
            {"metadata": {"namespace": "kube-system", "name": "proxy"}, "status": {"desiredNumberScheduled": 2, "numberReady": 2}},
        ])),
        "/api/v1/persistentvolumeclaims" => list(json!([
            {"metadata": {"namespace": "shop", "name": "data-db-0"}, "status": {"phase": "Bound"}},
            {"metadata": {"namespace": "shop", "name": "uploads"}, "status": {"phase": "Pending"}},
            {"metadata": {"namespace": "kube-system", "name": "old"}, "status": {"phase": "Lost"}},
        ])),
        _ => return (404, json!({})),
    };
    (200, body)
}

struct Fake {
    port: u16,
    ca_pem: String,
    cluster: Arc<Mutex<Cluster>>,
}

async fn serve() -> Fake {
    let mut ca = CertificateParams::default();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &ca)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![leaf.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let cluster = Arc::new(Mutex::new(initial()));
    let shared = cluster.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let cluster = shared.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(tcp).await else {
                    return;
                };
                let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                    let cluster = cluster.clone();
                    async move {
                        let auth = request
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        assert_eq!(request.method(), hyper::Method::GET);
                        let (status, body) = if auth != format!("Bearer {TOKEN}") {
                            (401, json!({"kind": "Status", "code": 401}))
                        } else {
                            let path = request.uri().path().to_owned();
                            let query = request.uri().query().unwrap_or("").to_owned();
                            answer_for(&mut cluster.lock().unwrap(), &path, &query)
                        };
                        let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
                        *response.status_mut() = hyper::StatusCode::from_u16(status).unwrap();
                        Ok::<_, Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    Fake {
        port,
        ca_pem: ca.pem(),
        cluster,
    }
}

fn params(fake: &Fake, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::from([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), fake.port.to_string()),
        (TOKEN_PARAM.to_owned(), TOKEN.to_owned()),
        (CA_PARAM.to_owned(), fake.ca_pem.clone()),
    ]);
    for (k, v) in extra {
        if v.is_empty() {
            map.remove(*k);
        } else {
            map.insert((*k).to_owned(), (*v).to_owned());
        }
    }
    map
}

async fn collect(sensor: &Arc<dyn Sensor>) -> Vec<Sample> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    sensor.collect(deadline).await.unwrap()
}

fn value(samples: &[Sample], metric: &str, instance: &str) -> Option<f64> {
    samples
        .iter()
        .find(|s| s.sensor == metric && s.instance == instance)
        .map(|s| s.value)
}

fn up(samples: &[Sample]) -> &Sample {
    samples.iter().find(|s| s.sensor == "up").unwrap()
}

#[tokio::test]
async fn reads_nodes_pods_workloads_and_claims() {
    let fake = serve().await;
    let sensor = build(KUBERNETES, &params(&fake, &[]), Place::Relay).unwrap();
    let samples = collect(&sensor).await;
    let up = up(&samples);
    assert_eq!(up.value, 1.0, "{samples:#?}");
    assert_eq!(up.labels["version"], "v1.31.4+k3s1");

    assert_eq!(value(&samples, "nodes", ""), Some(2.0));
    assert_eq!(value(&samples, "nodes_not_ready", ""), Some(1.0));
    assert_eq!(value(&samples, "node_ready", "cp-1"), Some(1.0));
    assert_eq!(value(&samples, "node_ready", "worker-1"), Some(0.0));
    assert_eq!(
        value(&samples, "node_memory_pressure", "worker-1"),
        Some(1.0)
    );
    assert_eq!(value(&samples, "node_disk_pressure", "cp-1"), Some(0.0));
    assert_eq!(value(&samples, "node_unschedulable", "cp-1"), Some(0.0));
    let cp = samples
        .iter()
        .find(|s| s.sensor == "node_ready" && s.instance == "cp-1")
        .unwrap();
    assert_eq!(cp.labels["roles"], "control-plane");
    assert_eq!(cp.labels["kubelet_version"], "v1.31.4");

    // Both pages were read.
    assert_eq!(value(&samples, "pods", ""), Some(5.0));
    assert_eq!(value(&samples, "pods_not_running", ""), Some(2.0));
    assert_eq!(value(&samples, "namespace_pods", "shop"), Some(3.0));
    let shop = samples
        .iter()
        .find(|s| s.sensor == "namespace_pods_not_running" && s.instance == "shop")
        .unwrap();
    assert_eq!(shop.value, 1.0);
    assert_eq!(shop.labels["pods"], "api-1 CrashLoopBackOff");
    let system = samples
        .iter()
        .find(|s| s.sensor == "namespace_pods_not_running" && s.instance == "kube-system")
        .unwrap();
    assert_eq!(system.labels["pods"], "pending-1 Pending");
    // Restarts start counting at the second collection.
    assert_eq!(value(&samples, "namespace_pod_restarts", "shop"), None);

    assert_eq!(
        value(&samples, "workload_replicas_desired", "shop/deployment/web"),
        Some(3.0)
    );
    assert_eq!(
        value(&samples, "workload_replicas_ready", "shop/deployment/web"),
        Some(1.0)
    );
    let ratio = value(&samples, "workload_ready_ratio", "shop/deployment/web").unwrap();
    assert!((ratio - 1.0 / 3.0).abs() < 1e-9);
    assert_eq!(
        value(&samples, "workload_ready_ratio", "shop/deployment/idle"),
        None
    );
    // A stateful set without `replicas` wants one.
    assert_eq!(
        value(&samples, "workload_ready_ratio", "shop/statefulset/db"),
        Some(1.0)
    );
    assert_eq!(
        value(
            &samples,
            "workload_ready_ratio",
            "kube-system/daemonset/proxy"
        ),
        Some(1.0)
    );
    assert_eq!(value(&samples, "workloads_short", ""), Some(1.0));

    assert_eq!(value(&samples, "claims_pending", ""), Some(1.0));
    assert_eq!(
        value(&samples, "namespace_claims_pending", "shop"),
        Some(1.0)
    );
    assert_eq!(
        value(&samples, "namespace_claims_lost", "kube-system"),
        Some(1.0)
    );

    // Every metric is one the template lists.
    let template = crate::templates::of(KUBERNETES).unwrap();
    for sample in &samples {
        assert!(
            template.metrics.iter().any(|m| m.suffix == sample.sensor),
            "{}",
            sample.sensor
        );
        assert!(!format!("{sample:?}").contains(TOKEN));
    }

    // A restart, and a new pod that restarted already.
    {
        let mut cluster = fake.cluster.lock().unwrap();
        cluster.pods[1] = crashing("shop", "api-1", 6);
        cluster
            .pods
            .as_array_mut()
            .unwrap()
            .push(pod("shop", "web-2", "Running", true, 2));
    }
    let samples = collect(&sensor).await;
    assert_eq!(value(&samples, "namespace_pod_restarts", "shop"), Some(4.0));
    assert_eq!(
        value(&samples, "namespace_pod_restarts", "kube-system"),
        Some(0.0)
    );
    // Only reads.
    let seen = fake.cluster.lock().unwrap().seen.clone();
    assert!(seen
        .iter()
        .any(|s| s.starts_with("/apis/apps/v1/daemonsets")));
}

#[tokio::test]
async fn namespaces_narrow_pods_workloads_and_claims() {
    let fake = serve().await;
    let sensor = build(
        KUBERNETES,
        &params(&fake, &[("namespaces", "^shop$")]),
        Place::Relay,
    )
    .unwrap();
    let samples = collect(&sensor).await;
    assert_eq!(value(&samples, "pods", ""), Some(3.0));
    assert_eq!(value(&samples, "namespace_pods", "kube-system"), None);
    assert_eq!(
        value(
            &samples,
            "workload_ready_ratio",
            "kube-system/daemonset/proxy"
        ),
        None
    );
    assert_eq!(
        value(&samples, "namespace_claims_lost", "kube-system"),
        None
    );
    // Nodes are the cluster's, whatever the namespaces.
    assert_eq!(value(&samples, "nodes", ""), Some(2.0));
}

#[tokio::test]
async fn failures_are_samples_with_their_reason() {
    let fake = serve().await;
    let reason = |samples: &[Sample]| {
        let up = up(samples);
        assert_eq!(up.value, 0.0, "{samples:#?}");
        (up.labels["reason"].clone(), up.labels["error"].clone())
    };

    let wrong = build(
        KUBERNETES,
        &params(&fake, &[(TOKEN_PARAM, "not-the-token-at-all")]),
        Place::Relay,
    )
    .unwrap();
    let (why, error) = reason(&collect(&wrong).await);
    assert_eq!(why, "login");
    assert!(error.contains("401"), "{error}");
    assert!(!error.contains("not-the-token-at-all"));

    let none = build(
        KUBERNETES,
        &params(&fake, &[(TOKEN_PARAM, "")]),
        Place::Relay,
    )
    .unwrap();
    let (why, error) = reason(&collect(&none).await);
    assert_eq!(why, "login");
    assert!(
        error.contains("enter the service account's token"),
        "{error}"
    );

    fake.cluster.lock().unwrap().forbidden.push("/api/v1/pods");
    let sensor = build(KUBERNETES, &params(&fake, &[]), Place::Relay).unwrap();
    let (why, error) = reason(&collect(&sensor).await);
    assert_eq!(why, "permission");
    assert!(
        error.contains("/api/v1/pods") && error.contains("ClusterRole"),
        "{error}"
    );

    // Without the cluster's CA, the machine's roots do not trust it.
    let untrusted = build(KUBERNETES, &params(&fake, &[(CA_PARAM, "")]), Place::Relay).unwrap();
    let (why, error) = reason(&collect(&untrusted).await);
    assert_eq!(why, "tls", "{error}");
    assert!(error.contains("give the cluster's CA"), "{error}");

    // Nothing listens.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port().to_string();
    drop(closed);
    let down = build(KUBERNETES, &params(&fake, &[("port", &port)]), Place::Relay).unwrap();
    assert_eq!(reason(&collect(&down).await).0, "connect");
}

#[test]
fn parameters_and_places() {
    let error = |pairs: &[(&str, &str)], place: Place<'_>| {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        build(KUBERNETES, &map, place).err().unwrap().to_string()
    };
    assert!(error(&[], Place::Agent(None)).contains("not by an agent"));
    assert!(error(&[("host", "169.254.169.254")], Place::Central).contains("link-local"));
    assert!(error(&[("port", "0")], Place::Check).contains("between 1 and 65535"));
    assert!(error(&[(TOKEN_PARAM, "a b")], Place::Check).contains("without spaces"));
    assert!(error(&[(CA_PARAM, "not a certificate")], Place::Check)
        .contains("certificate-authority-data"));
    assert!(error(&[("namespaces", "(")], Place::Check).contains("regular expression"));
    assert!(error(&[("user", "x")], Place::Check).contains("unknown parameter `user`"));
    let map = BTreeMap::from([(TOKEN_PARAM.to_owned(), TOKEN.to_owned())]);
    let sensor = Kubernetes::from_params(&mut Params::new(KUBERNETES, &map), Place::Check).unwrap();
    assert!(!format!("{sensor:?}").contains(TOKEN));
    assert_eq!(sensor.scrub(&format!("x {TOKEN} y")), "x *** y");
}

#[test]
fn ca_in_pem_on_one_line_or_from_a_kubeconfig() {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
    let pem = ca.pem();
    let one = parse_ca(&pem).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].as_ref(), ca.der().as_ref());
    // Pasted in a one-line field, its line breaks gone.
    assert_eq!(parse_ca(&pem.replace('\n', "")).unwrap(), one);
    // `certificate-authority-data`: the base64 of the PEM.
    let data = base64::engine::general_purpose::STANDARD.encode(pem.as_bytes());
    assert_eq!(parse_ca(&data).unwrap(), one);
    // Two certificates, a bundle.
    assert_eq!(parse_ca(&format!("{pem}{pem}")).unwrap().len(), 2);
    assert!(parse_ca("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----").is_err());
    assert!(parse_ca("-----BEGIN CERTIFICATE-----\nAAAA").is_err());
    assert!(parse_ca("aGVsbG8=").is_err());
}

#[test]
fn token_and_ca_are_sealed_together() {
    assert_eq!(secret_params(KUBERNETES), [TOKEN_PARAM, CA_PARAM]);
    assert_eq!(secret_params("redis"), ["password"]);
    assert!(secret_params("docker").is_empty());
    let given = BTreeMap::from([
        (TOKEN_PARAM.to_owned(), TOKEN.to_owned()),
        (CA_PARAM.to_owned(), "PEM".to_owned()),
    ]);
    let sealed = seal_form(KUBERNETES, &given);
    let mut params = BTreeMap::new();
    open_into(KUBERNETES, &sealed, &mut params);
    assert_eq!(params, given);
    // Without a CA, none is put back.
    let sealed = seal_form(
        KUBERNETES,
        &BTreeMap::from([(TOKEN_PARAM.to_owned(), "t".to_owned())]),
    );
    let mut params = BTreeMap::new();
    open_into(KUBERNETES, &sealed, &mut params);
    assert_eq!(params.len(), 1);
    // A password stays as it is.
    let given = BTreeMap::from([("password".to_owned(), "pw".to_owned())]);
    assert_eq!(seal_form("mysql", &given), "pw");
}

#[test]
fn pods_that_do_not_run() {
    assert_eq!(pod_problem(&pod("a", "p", "Running", true, 0)), None);
    assert_eq!(pod_problem(&pod("a", "p", "Succeeded", false, 0)), None);
    assert_eq!(
        pod_problem(&pod("a", "p", "Running", false, 0)).as_deref(),
        Some("NotReady")
    );
    assert_eq!(
        pod_problem(&pod("a", "p", "Pending", false, 0)).as_deref(),
        Some("Pending")
    );
    assert_eq!(
        pod_problem(&crashing("a", "p", 3)).as_deref(),
        Some("CrashLoopBackOff")
    );
    let mut failed = pod("a", "p", "Failed", false, 0);
    failed["status"]["reason"] = json!("Evicted");
    assert_eq!(pod_problem(&failed).as_deref(), Some("Evicted"));
    // A failed job's pod is the job's business.
    failed["metadata"]["ownerReferences"] = json!([{"kind": "Job", "name": "backup"}]);
    assert_eq!(pod_problem(&failed), None);
    // Going away.
    let mut leaving = pod("a", "p", "Pending", false, 0);
    leaving["metadata"]["deletionTimestamp"] = json!("2026-11-24T00:00:00Z");
    assert_eq!(pod_problem(&leaving), None);
}

#[test]
fn many_names_are_cut() {
    let list: Vec<String> = (0..100).map(|i| format!("pod-{i:03}")).collect();
    let text = names(&list);
    assert!(text.len() <= MAX_NAMES);
    assert!(
        text.starts_with("pod-000, pod-001") && text.ends_with(" …"),
        "{text}"
    );
    assert_eq!(names(&["a".into(), "b".into()]), "a, b");
}
