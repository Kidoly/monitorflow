//! The cloud sensors against fake AWS and Azure APIs over plain HTTP on
//! the loopback: no real cloud is ever called.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use monitorflow_cloud::{parse, CloudSensor, AWS, AZURE};
use monitorflow_sensors::{Sample, Sensor};
use tokio::net::TcpListener;

type Handler = Arc<dyn Fn(&str, &str, &str, &str) -> (u16, String) + Send + Sync>;

/// Serves `handler(method, path and query, authorization, body)`; the
/// requests seen are kept.
async fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (handler, log) = (handler.clone(), log.clone());
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                    let (handler, log) = (handler.clone(), log.clone());
                    async move {
                        let method = request.method().to_string();
                        let target = request.uri().to_string();
                        let auth = request
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        let body = String::from_utf8_lossy(&body).into_owned();
                        log.lock()
                            .unwrap()
                            .push(format!("{method} {target} {body}"));
                        let (status, answer) = handler(&method, &target, &auth, &body);
                        let mut response = Response::new(Full::new(Bytes::from(answer)));
                        *response.status_mut() = StatusCode::from_u16(status).unwrap();
                        Ok::<_, Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    (base, seen)
}

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn collect(sensor: &CloudSensor) -> Vec<Sample> {
    sensor
        .collect(tokio::time::Instant::now() + Duration::from_secs(20))
        .await
        .unwrap()
}

fn find<'a>(samples: &'a [Sample], sensor: &str, instance: &str) -> Option<&'a Sample> {
    samples
        .iter()
        .find(|s| s.sensor == sensor && s.instance == instance)
}

const KEY: &str = "AKIAIOSFODNN7EXAMPLE";

fn aws(what: &str) -> BTreeMap<String, String> {
    params(&[
        ("key_id", KEY),
        ("secret", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
        ("regions", "eu-west-3"),
        ("what", what),
    ])
}

/// A fake EC2 and CloudWatch; `cloudwatch` says whether metrics are
/// allowed.
fn fake_aws(cloudwatch: bool) -> Handler {
    Arc::new(move |method, _, auth, body| {
        assert_eq!(method, "POST");
        if !auth.starts_with(&format!("AWS4-HMAC-SHA256 Credential={KEY}/")) {
            return (
                401,
                "<Response><Errors><Error><Code>AuthFailure</Code><Message>AWS was not able to validate the provided access credentials</Message></Error></Errors></Response>".into(),
            );
        }
        assert!(auth.contains("/eu-west-3/"), "{auth}");
        if body.contains("Action=DescribeInstances&") {
            assert!(auth.contains("/ec2/aws4_request"), "{auth}");
            (200, include_str!("fixtures/describe_instances.xml").into())
        } else if body.contains("Action=DescribeInstanceStatus") {
            (200, "<DescribeInstanceStatusResponse><instanceStatusSet><item><instanceId>i-0123456789abcdef0</instanceId><systemStatus><status>ok</status></systemStatus><instanceStatus><status>impaired</status></instanceStatus></item></instanceStatusSet></DescribeInstanceStatusResponse>".into())
        } else if body.contains("Action=GetMetricData") {
            assert!(auth.contains("/monitoring/aws4_request"), "{auth}");
            if !cloudwatch {
                return (403, "<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>User is not authorized to perform: cloudwatch:GetMetricData</Message></Error></ErrorResponse>".into());
            }
            assert!(body.contains("i-0123456789abcdef0"), "{body}");
            assert!(!body.contains("i-0fedcba9876543210"), "stopped: not asked");
            let member = |id: &str, value: &str| {
                format!("<member><Id>{id}</Id><StatusCode>Complete</StatusCode><Values><member>{value}</member><member>1</member></Values></member>")
            };
            (
                200,
                format!(
                    "<GetMetricDataResponse><GetMetricDataResult><MetricDataResults>{}{}{}</MetricDataResults></GetMetricDataResult></GetMetricDataResponse>",
                    member("q0_0", "12.5"),
                    member("q0_1", "30000"),
                    member("q0_3", "600")
                ),
            )
        } else {
            panic!("unexpected call: {body}");
        }
    })
}

#[tokio::test]
async fn an_aws_account_gives_its_instances() {
    let (base, seen) = serve(fake_aws(true)).await;
    let sensor = CloudSensor::with_base(parse(AWS, &aws("inventory")).unwrap(), &base).unwrap();
    let samples = collect(&sensor).await;
    let up = find(&samples, "up", "").unwrap();
    assert_eq!(up.value, 1.0, "{:?}", up.labels);
    assert_eq!(up.labels["provider"], "aws");
    assert_eq!(up.labels["product"], "Amazon EC2 (eu-west-3)");
    let info = find(&samples, "vm_info", "web-01").unwrap();
    assert_eq!(info.labels["key"], "i-0123456789abcdef0");
    assert_eq!(info.labels["host"], "eu-west-3a");
    assert_eq!(info.labels["size"], "t3.micro");
    assert_eq!(info.labels["macs"], "0a:1b:2c:3d:4e:5f");
    assert!(info.labels["addresses"].starts_with("10.0.1.15,"));
    assert_eq!(
        info.labels["guest_hostname"],
        "ip-10-0-1-15.eu-west-3.compute.internal"
    );
    let value = |sensor: &str, instance: &str| find(&samples, sensor, instance).map(|s| s.value);
    assert_eq!(value("vm_powered_on", "web-01"), Some(1.0));
    assert_eq!(value("vm_cpu_usage_ratio", "web-01"), Some(0.125));
    assert_eq!(
        value("vm_network_in_bytes_per_second", "web-01"),
        Some(100.0)
    );
    assert_eq!(value("vm_disk_read_bytes_per_second", "web-01"), Some(2.0));
    assert_eq!(value("vm_status_checks_failed", "web-01"), Some(1.0));
    assert_eq!(value("vm_status", "web-01"), Some(2.0));
    assert_eq!(value("vm_powered_on", "i-0fedcba9876543210"), Some(0.0));
    assert_eq!(
        value("vm_status_checks_failed", "i-0fedcba9876543210"),
        None
    );
    assert!(samples.iter().all(|s| !s.sensor.contains("snapshot")));
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(
        calls.iter().all(|c| !c.contains("EXAMPLEKEY")),
        "the secret never travels"
    );
}

#[tokio::test]
async fn aws_failures_say_why_and_keep_what_was_read() {
    let (base, _) = serve(fake_aws(false)).await;
    let sensor = CloudSensor::with_base(parse(AWS, &aws("inventory")).unwrap(), &base).unwrap();
    let samples = collect(&sensor).await;
    let up = find(&samples, "up", "").unwrap();
    assert_eq!(up.value, 0.0);
    assert_eq!(up.labels["reason"], "permission");
    assert!(
        up.labels["error"].contains("CloudWatch eu-west-3: AccessDenied"),
        "{:?}",
        up.labels
    );
    assert!(
        find(&samples, "vm_info", "web-01").is_some(),
        "machines are still listed"
    );

    let test = CloudSensor::with_base(parse(AWS, &aws("test")).unwrap(), &base).unwrap();
    let samples = collect(&test).await;
    assert_eq!(find(&samples, "login", "").unwrap().value, 1.0);
    assert_eq!(find(&samples, "vms", "").unwrap().value, 2.0);
    assert_eq!(find(&samples, "running", "").unwrap().value, 1.0);
    let metrics = find(&samples, "metrics", "").unwrap();
    assert_eq!(
        (metrics.value, metrics.labels["reason"].as_str()),
        (0.0, "permission")
    );

    let mut wrong = aws("test");
    wrong.insert("key_id".into(), "AKIAWRONGWRONGWRONG0".into());
    let sensor = CloudSensor::with_base(parse(AWS, &wrong).unwrap(), &base).unwrap();
    let samples = collect(&sensor).await;
    let login = find(&samples, "login", "").unwrap();
    assert_eq!(login.value, 0.0);
    assert_eq!(login.labels["reason"], "login");
    assert!(login.labels["error"].contains("AuthFailure"));
}

const TENANT: &str = "11111111-2222-3333-4444-555555555555";
const CLIENT: &str = "66666666-7777-8888-9999-000000000000";
const SUB: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn azure(what: &str, secret: &str) -> BTreeMap<String, String> {
    params(&[
        ("tenant", TENANT),
        ("key_id", CLIENT),
        ("secret", secret),
        ("subscription", SUB),
        ("what", what),
    ])
}

fn fake_azure(base: Arc<Mutex<String>>) -> Handler {
    Arc::new(move |method, target, auth, body| {
        let base = base.lock().unwrap().clone();
        if target == format!("/login/{TENANT}/oauth2/v2.0/token") {
            assert_eq!(method, "POST");
            assert!(body.contains("grant_type=client_credentials"), "{body}");
            assert!(
                body.contains("scope=https%3A%2F%2Fmanagement.azure.com%2F.default"),
                "{body}"
            );
            if !body.contains("client_secret=right") {
                return (401, r#"{"error":"invalid_client","error_description":"AADSTS7000215: Invalid client secret provided.\r\nTrace ID: x"}"#.into());
            }
            return (
                200,
                r#"{"token_type":"Bearer","expires_in":3599,"access_token":"token-1"}"#.into(),
            );
        }
        assert_eq!(method, "GET");
        assert_eq!(auth, "Bearer token-1", "{target}");
        let vm = |name: &str, id: &str, power: &str| {
            format!(
                r#"{{"id":"/subscriptions/{SUB}/resourceGroups/RG/providers/Microsoft.Compute/virtualMachines/{name}","name":"{name}","location":"westeurope","properties":{{"vmId":"{id}","hardwareProfile":{{"vmSize":"Standard_B2s"}},"osProfile":{{"computerName":"{name}"}},"instanceView":{{"statuses":[{{"code":"PowerState/{power}"}}]}}}}}}"#
            )
        };
        if target.starts_with(&format!(
            "/subscriptions/{SUB}/providers/Microsoft.Compute/virtualMachines?"
        )) {
            assert!(target.contains("statusOnly=true"), "{target}");
            if target.contains("page=2") {
                // A next link elsewhere is not followed with the token.
                let page = format!(
                    r#"{{"value":[{}],"nextLink":"http://elsewhere.test/steal"}}"#,
                    vm(
                        "db-01",
                        "22222222-0000-4000-8000-000000000002",
                        "deallocated"
                    )
                );
                return (200, page);
            }
            let page = format!(
                r#"{{"value":[{}],"nextLink":"{base}/subscriptions/{SUB}/providers/Microsoft.Compute/virtualMachines?api-version=2024-07-01&statusOnly=true&page=2"}}"#,
                vm("web-01", "11111111-0000-4000-8000-000000000001", "running")
            );
            return (200, page);
        }
        if target.starts_with(&format!(
            "/subscriptions/{SUB}/providers/Microsoft.Network/networkInterfaces?"
        )) {
            return (
                200,
                format!(
                    r#"{{"value":[{{"properties":{{"macAddress":"00-0D-3A-1B-2C-3D","virtualMachine":{{"id":"/subscriptions/{SUB}/resourceGroups/rg/providers/Microsoft.Compute/virtualMachines/web-01"}},"ipConfigurations":[{{"properties":{{"privateIPAddress":"10.1.0.4"}}}}]}}}}]}}"#
                ),
            );
        }
        if target.contains("/virtualMachines/web-01/providers/Microsoft.Insights/metrics?") {
            assert!(
                target.contains("metricnames=Percentage+CPU%2C")
                    || target.contains("metricnames=Percentage%20CPU%2C"),
                "{target}"
            );
            return (200, r#"{"value":[
                {"name":{"value":"Percentage CPU"},"timeseries":[{"data":[{"timeStamp":"2026-11-24T10:05:00Z","average":50.0}]}]},
                {"name":{"value":"Disk Write Bytes"},"timeseries":[{"data":[{"timeStamp":"2026-11-24T10:05:00Z","total":1500.0}]}]},
                {"name":{"value":"VmAvailabilityMetric"},"timeseries":[{"data":[{"timeStamp":"2026-11-24T10:05:00Z","average":0.0}]}]}]}"#.into());
        }
        panic!("unexpected call: {method} {target}");
    })
}

#[tokio::test]
async fn an_azure_subscription_gives_its_machines() {
    let shared = Arc::new(Mutex::new(String::new()));
    let (base, seen) = serve(fake_azure(shared.clone())).await;
    *shared.lock().unwrap() = base.clone();
    let sensor =
        CloudSensor::with_base(parse(AZURE, &azure("inventory", "right")).unwrap(), &base).unwrap();
    let samples = collect(&sensor).await;
    let up = find(&samples, "up", "").unwrap();
    assert_eq!(up.value, 1.0, "{:?}", up.labels);
    assert_eq!(up.labels["provider"], "azure");
    let info = find(&samples, "vm_info", "web-01").unwrap();
    assert_eq!(info.labels["key"], "11111111-0000-4000-8000-000000000001");
    assert_eq!(info.labels["uuid"], "11111111-0000-4000-8000-000000000001");
    assert_eq!(info.labels["macs"], "00:0d:3a:1b:2c:3d");
    assert_eq!(info.labels["addresses"], "10.1.0.4");
    assert_eq!(info.labels["host"], "westeurope");
    let value = |sensor: &str, instance: &str| find(&samples, sensor, instance).map(|s| s.value);
    assert_eq!(value("vm_cpu_usage_ratio", "web-01"), Some(0.5));
    assert_eq!(value("vm_disk_write_bytes_per_second", "web-01"), Some(5.0));
    assert_eq!(value("vm_status_checks_failed", "web-01"), Some(1.0));
    assert_eq!(value("vm_status", "web-01"), Some(2.0));
    assert_eq!(value("vm_powered_on", "db-01"), Some(0.0));
    assert!(find(&samples, "vm_cpu_usage_ratio", "db-01").is_none());

    // The token is kept for the next collection.
    collect(&sensor).await;
    let calls = seen.lock().unwrap();
    assert_eq!(
        calls.iter().filter(|c| c.contains("/oauth2/")).count(),
        1,
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("steal")));
}

#[tokio::test]
async fn a_refused_azure_secret_says_so() {
    let shared = Arc::new(Mutex::new(String::new()));
    let (base, _) = serve(fake_azure(shared.clone())).await;
    *shared.lock().unwrap() = base.clone();
    let sensor =
        CloudSensor::with_base(parse(AZURE, &azure("inventory", "wrong")).unwrap(), &base).unwrap();
    let samples = collect(&sensor).await;
    let up = find(&samples, "up", "").unwrap();
    assert_eq!(up.value, 0.0);
    assert_eq!(up.labels["reason"], "login");
    assert_eq!(
        up.labels["error"],
        "invalid_client: AADSTS7000215: Invalid client secret provided."
    );
    let test =
        CloudSensor::with_base(parse(AZURE, &azure("test", "right")).unwrap(), &base).unwrap();
    let samples = collect(&test).await;
    assert_eq!(find(&samples, "login", "").unwrap().value, 1.0);
    assert_eq!(find(&samples, "vms", "").unwrap().value, 2.0);
    assert_eq!(find(&samples, "metrics", "").unwrap().value, 1.0);
}

#[tokio::test]
async fn real_endpoints_are_https_only() {
    // Built without a base, a sensor never speaks plain HTTP; nothing is
    // called here.
    assert!(CloudSensor::new(parse(AWS, &aws("inventory")).unwrap()).is_ok());
}
