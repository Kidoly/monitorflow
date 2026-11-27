//! AWS: EC2 instances, their status checks and CloudWatch's basic
//! metrics, over the Query API signed with Signature Version 4.
//!
//! Calls, all read-only: `ec2:DescribeInstances`,
//! `ec2:DescribeInstanceStatus` and `cloudwatch:GetMetricData`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use monitorflow_sensors::virt::{Status, Vm};
use monitorflow_vmware::xml::{self, Element};
use ring::{digest, hmac};

use crate::http::{bounded, encode, form, is_local, Failure, Http};
use crate::CloudVm;

const EC2_VERSION: &str = "2016-11-15";
const CLOUDWATCH_VERSION: &str = "2010-08-01";
/// Instances per page of `DescribeInstances`.
const PAGE: usize = 1000;
/// Pages read at most per region and call.
const MAX_PAGES: usize = 20;
/// Queries per `GetMetricData`, its own limit.
const QUERIES: usize = 500;
/// CloudWatch's period for basic monitoring, in seconds.
pub const PERIOD: i64 = 300;
/// How far back metrics are asked for: the newest point of basic
/// monitoring is often 10 minutes old.
const WINDOW: i64 = 20 * 60;
const FORM: &str = "application/x-www-form-urlencoded; charset=utf-8";

/// What CloudWatch is asked per running instance: its metric, statistic,
/// and the measure it becomes, divided by the period for sums.
const METRICS: [(&str, &str, &str); 5] = [
    ("CPUUtilization", "Average", "cpu_usage_ratio"),
    ("NetworkIn", "Sum", "network_in_bytes_per_second"),
    ("NetworkOut", "Sum", "network_out_bytes_per_second"),
    ("EBSReadBytes", "Sum", "disk_read_bytes_per_second"),
    ("EBSWriteBytes", "Sum", "disk_write_bytes_per_second"),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(data: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, data).as_ref())
}

fn mac(key: &[u8], data: &str) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes())
}

/// One request to sign.
pub struct Request<'a> {
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    /// Already canonical: names and values encoded, sorted by name.
    pub query: &'a str,
    /// Lower-case names, sorted, `host` and `x-amz-date` among them.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

/// The `Authorization` header of `request` (Signature Version 4).
pub fn authorization(
    key_id: &str,
    secret: &str,
    region: &str,
    service: &str,
    amz_date: &str,
    request: &Request<'_>,
) -> String {
    let date = &amz_date[..8];
    let canonical_headers: String = request
        .headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed: Vec<&str> = request.headers.iter().map(|(k, _)| *k).collect();
    let signed = signed.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed}\n{}",
        request.method,
        request.path,
        request.query,
        sha256(request.body)
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256(canonical.as_bytes())
    );
    let key = mac(format!("AWS4{secret}").as_bytes(), date);
    let key = mac(key.as_ref(), region);
    let key = mac(key.as_ref(), service);
    let key = mac(key.as_ref(), "aws4_request");
    let signature = hex(mac(key.as_ref(), &to_sign).as_ref());
    format!("AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={signed}, Signature={signature}")
}

/// An AWS account read with one access key.
pub struct Client {
    pub http: Http,
    pub key_id: String,
    pub secret: String,
    /// `http://127.0.0.1:port` for tests; the regional endpoints otherwise.
    pub base: Option<String>,
}

impl Client {
    fn url(&self, service: &str, region: &str) -> (String, String) {
        match &self.base {
            // A test server on this machine is the only endpoint other
            // than AWS's, and the only one reached without TLS: a signed
            // request never leaves the machine in clear text.
            Some(base) if is_local(base) => {
                let host = base.split("://").nth(1).unwrap_or(base).to_owned();
                (format!("{base}/"), host)
            }
            Some(_) | None => {
                let host = format!("{service}.{region}.amazonaws.com");
                (format!("https://{host}/"), host)
            }
        }
    }

    /// One signed Query API call; its answer's root element.
    async fn call(
        &self,
        service: &str,
        region: &str,
        params: &[(String, String)],
    ) -> Result<Element, Failure> {
        let (url, host) = self.url(service, region);
        let body = form(params);
        let amz_date = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let headers = [
            ("content-type", FORM),
            ("host", host.as_str()),
            ("x-amz-date", amz_date.as_str()),
        ];
        let auth = authorization(
            &self.key_id,
            &self.secret,
            region,
            service,
            &amz_date,
            &Request {
                method: "POST",
                host: &host,
                path: "/",
                query: "",
                headers: &headers,
                body: body.as_bytes(),
            },
        );
        let request = self
            .http
            .client()
            .post(url)
            .header("Content-Type", FORM)
            .header("X-Amz-Date", &amz_date)
            .header("Authorization", auth)
            .body(body);
        let (status, bytes) = self.http.send(request).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let root = tokio::task::spawn_blocking(move || xml::parse(&text))
            .await
            .map_err(|_| Failure::Api("the answer could not be read".into()))?;
        let root = match root {
            Ok(root) => root,
            Err(_) if !status.is_success() => {
                return Err(Failure::Api(format!("the API answered {status}")))
            }
            Err(e) => {
                return Err(Failure::Api(bounded(&format!(
                    "the answer does not read: {e}"
                ))))
            }
        };
        if let Some(error) = root.find("Error") {
            return Err(refusal(
                error.text_of("Code").unwrap_or_default(),
                error
                    .text_of("Message")
                    .unwrap_or("the API refused the call"),
            ));
        }
        if !status.is_success() {
            return Err(Failure::Api(format!("the API answered {status}")));
        }
        Ok(root)
    }

    /// The instances of `region`, every page.
    pub async fn instances(&self, region: &str, max: usize) -> Result<Vec<Instance>, Failure> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = vec![
                ("Action".to_string(), "DescribeInstances".to_string()),
                ("Version".into(), EC2_VERSION.into()),
                ("MaxResults".into(), PAGE.to_string()),
            ];
            if let Some(token) = &token {
                params.push(("NextToken".into(), token.clone()));
            }
            let root = self.call("ec2", region, &params).await?;
            if let Some(set) = root.child("reservationSet") {
                for reservation in set.all("item") {
                    if let Some(instances) = reservation.child("instancesSet") {
                        out.extend(
                            instances
                                .all("item")
                                .filter_map(|i| Instance::read(i, region)),
                        );
                    }
                }
            }
            if out.len() >= max {
                out.truncate(max);
                return Ok(out);
            }
            token = root
                .text_of("nextToken")
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            if token.is_none() {
                return Ok(out);
            }
        }
        Ok(out)
    }

    /// The status checks of the running instances of `region`.
    pub async fn statuses(&self, region: &str) -> Result<HashMap<String, Checks>, Failure> {
        let mut out = HashMap::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = vec![
                ("Action".to_string(), "DescribeInstanceStatus".to_string()),
                ("Version".into(), EC2_VERSION.into()),
                ("MaxResults".into(), PAGE.to_string()),
            ];
            if let Some(token) = &token {
                params.push(("NextToken".into(), token.clone()));
            }
            let root = self.call("ec2", region, &params).await?;
            if let Some(set) = root.child("instanceStatusSet") {
                for item in set.all("item") {
                    if let Some(id) = item.text_of("instanceId") {
                        out.insert(id.to_owned(), Checks::read(item));
                    }
                }
            }
            token = root
                .text_of("nextToken")
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            if token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// The latest basic metrics of `instances` of `region`, by instance
    /// ID and measure.
    pub async fn metrics(
        &self,
        region: &str,
        instances: &[&str],
        now: DateTime<Utc>,
    ) -> Result<HashMap<String, Vec<(&'static str, f64)>>, Failure> {
        let mut out: HashMap<String, Vec<(&'static str, f64)>> = HashMap::new();
        let queries: Vec<(usize, usize)> = (0..instances.len())
            .flat_map(|i| (0..METRICS.len()).map(move |m| (i, m)))
            .collect();
        let start = (now - chrono::Duration::seconds(WINDOW)).format("%Y-%m-%dT%H:%M:%SZ");
        let end = now.format("%Y-%m-%dT%H:%M:%SZ");
        for batch in queries.chunks(QUERIES) {
            let mut params = vec![
                ("Action".to_string(), "GetMetricData".to_string()),
                ("Version".into(), CLOUDWATCH_VERSION.into()),
                ("StartTime".into(), start.to_string()),
                ("EndTime".into(), end.to_string()),
                ("ScanBy".into(), "TimestampDescending".into()),
            ];
            for (n, (i, m)) in batch.iter().enumerate() {
                let (metric, stat, _) = METRICS[*m];
                let p = format!("MetricDataQueries.member.{}", n + 1);
                params.extend([
                    (format!("{p}.Id"), format!("q{i}_{m}")),
                    (format!("{p}.MetricStat.Metric.Namespace"), "AWS/EC2".into()),
                    (format!("{p}.MetricStat.Metric.MetricName"), metric.into()),
                    (
                        format!("{p}.MetricStat.Metric.Dimensions.member.1.Name"),
                        "InstanceId".into(),
                    ),
                    (
                        format!("{p}.MetricStat.Metric.Dimensions.member.1.Value"),
                        instances[*i].into(),
                    ),
                    (format!("{p}.MetricStat.Period"), PERIOD.to_string()),
                    (format!("{p}.MetricStat.Stat"), stat.into()),
                    (format!("{p}.ReturnData"), "true".into()),
                ]);
            }
            let root = self.call("monitoring", region, &params).await?;
            for (id, value) in latest(&root) {
                let Some((i, m)) = id
                    .strip_prefix('q')
                    .and_then(|r| r.split_once('_'))
                    .and_then(|(i, m)| Some((i.parse::<usize>().ok()?, m.parse::<usize>().ok()?)))
                    .filter(|(i, m)| *i < instances.len() && *m < METRICS.len())
                else {
                    continue;
                };
                let (_, stat, measure) = METRICS[m];
                let value = match (stat, m) {
                    (_, 0) => value / 100.0,
                    ("Sum", _) => value / PERIOD as f64,
                    _ => value,
                };
                out.entry(instances[i].to_owned())
                    .or_default()
                    .push((measure, value));
            }
        }
        Ok(out)
    }
}

/// The newest value of each query of a `GetMetricData` answer.
fn latest(root: &Element) -> Vec<(String, f64)> {
    let Some(results) = root
        .find("MetricDataResults")
        .map(|r| r.all("member").collect::<Vec<_>>())
    else {
        return Vec::new();
    };
    results
        .into_iter()
        .filter_map(|result| {
            let id = result.text_of("Id")?.to_owned();
            let value = result
                .child("Values")?
                .all("member")
                .find_map(|v| v.text.trim().parse::<f64>().ok())?;
            value.is_finite().then_some((id, value))
        })
        .collect()
}

/// An API error code as a [`Failure`].
fn refusal(code: &str, message: &str) -> Failure {
    let message = bounded(&format!("{code}: {message}"));
    match code {
        "AuthFailure"
        | "InvalidClientTokenId"
        | "SignatureDoesNotMatch"
        | "OptInRequired"
        | "IncompleteSignature"
        | "MissingAuthenticationToken" => Failure::Login(message),
        "UnauthorizedOperation" | "AccessDenied" | "AccessDeniedException" => {
            Failure::Permission(message)
        }
        _ => Failure::Api(message),
    }
}

/// One EC2 instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub state: String,
    pub zone: String,
    pub kind: Option<String>,
    pub hostname: Option<String>,
    pub platform: Option<String>,
    pub cpu_count: Option<f64>,
    pub addresses: Vec<String>,
    pub macs: Vec<String>,
}

impl Instance {
    fn read(item: &Element, region: &str) -> Option<Self> {
        let id = item
            .text_of("instanceId")
            .filter(|i| !i.is_empty())?
            .to_owned();
        let text = |name: &str| {
            item.text_of(name)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        };
        let name = item
            .child("tagSet")
            .into_iter()
            .flat_map(|t| t.all("item"))
            .find(|t| t.text_of("key") == Some("Name"))
            .and_then(|t| t.text_of("value"))
            .filter(|n| !n.is_empty())
            .map_or_else(|| id.clone(), str::to_owned);
        let mut addresses = Vec::new();
        let mut push = |a: &str| {
            if !a.is_empty() && !addresses.iter().any(|x| x == a) {
                addresses.push(a.to_owned());
            }
        };
        if let Some(ip) = item.text_of("privateIpAddress") {
            push(ip);
        }
        let mut macs = Vec::new();
        for nic in item
            .child("networkInterfaceSet")
            .into_iter()
            .flat_map(|n| n.all("item"))
        {
            if let Some(mac) = nic.text_of("macAddress").filter(|m| !m.is_empty()) {
                macs.push(mac.to_ascii_lowercase());
            }
            for set in ["privateIpAddressesSet", "ipv6AddressesSet"] {
                for address in nic.child(set).into_iter().flat_map(|s| s.all("item")) {
                    if let Some(ip) = address
                        .text_of("privateIpAddress")
                        .or_else(|| address.text_of("ipv6Address"))
                    {
                        push(ip);
                    }
                }
            }
        }
        if let Some(ip) = item.text_of("ipAddress") {
            push(ip);
        }
        let cpu = item.child("cpuOptions").and_then(|c| {
            let cores: f64 = c.text_of("coreCount")?.parse().ok()?;
            let threads: f64 = c.text_of("threadsPerCore").unwrap_or("1").parse().ok()?;
            Some(cores * threads)
        });
        Some(Self {
            name,
            state: item
                .child("instanceState")
                .and_then(|s| s.text_of("name"))
                .unwrap_or_default()
                .to_owned(),
            zone: item
                .child("placement")
                .and_then(|p| p.text_of("availabilityZone"))
                .filter(|z| !z.is_empty())
                .unwrap_or(region)
                .to_owned(),
            kind: text("instanceType"),
            hostname: text("privateDnsName"),
            platform: text("platformDetails"),
            cpu_count: cpu,
            addresses,
            macs,
            id,
        })
    }

    /// As a virtual machine of the inventory.
    pub fn into_vm(self, checks: Option<&Checks>) -> CloudVm {
        let running = self.state == "running";
        let mut measures = Vec::new();
        let mut status = None;
        if let Some(checks) = checks.filter(|_| running) {
            measures.push(("status_checks_failed", f64::from(checks.failed)));
            status = Some(checks.status);
        }
        CloudVm {
            vm: Vm {
                key: self.id,
                name: self.name,
                host: Some(self.zone),
                power_state: self.state,
                powered_on: running,
                cpu_count: self.cpu_count,
                status,
                guest_hostname: self.hostname,
                guest_os: self.platform,
                addresses: self.addresses,
                macs: self.macs,
                ..Vm::default()
            },
            size: self.kind,
            measures,
        }
    }
}

/// The status checks of one running instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checks {
    /// Of the system, instance and attached volume checks, how many fail.
    pub failed: u8,
    /// Critical when one fails, a warning while they start or lack data.
    pub status: Status,
}

impl Checks {
    fn read(item: &Element) -> Self {
        let mut failed = 0;
        let mut status = Status::Ok;
        for check in ["systemStatus", "instanceStatus", "attachedEbsStatus"] {
            match item.child(check).and_then(|c| c.text_of("status")) {
                Some("impaired") => {
                    failed += 1;
                    status = Status::Critical;
                }
                Some("initializing" | "insufficient-data") => status = status.max(Status::Warning),
                _ => {}
            }
        }
        Self { failed, status }
    }
}

/// `region` as AWS names them, like `eu-west-3` or `us-gov-west-1`.
pub fn valid_region(region: &str) -> bool {
    let mut parts = region.split('-');
    let first = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    first.len() == 2
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && rest.len() >= 2
        && rest[..rest.len() - 1]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()))
        && rest[rest.len() - 1].parse::<u8>().is_ok()
}

/// What `encode` does to a query value, for callers building canonical
/// query strings.
pub fn canonical_query(pairs: &mut [(String, String)]) -> String {
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_machine_is_reached_without_tls() {
        assert!(is_local("http://127.0.0.1:8080"));
        assert!(is_local("http://localhost:9000"));
        assert!(is_local("http://[::1]:9000"));
        for base in [
            "http://example.com",
            "http://127.0.0.1.example.com",
            "https://127.0.0.1:1",
        ] {
            assert!(!is_local(base), "{base}");
        }
        let client = Client {
            http: Http::new(std::time::Duration::from_secs(1), true).unwrap(),
            key_id: String::new(),
            secret: String::new(),
            base: Some("http://example.com".into()),
        };
        assert_eq!(
            client.url("ec2", "eu-west-3").0,
            "https://ec2.eu-west-3.amazonaws.com/"
        );
    }

    #[test]
    fn signatures_match_the_aws_test_suite() {
        // `get-vanilla` of the Signature Version 4 test suite.
        let auth = authorization(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "service",
            "20150830T123600Z",
            &Request {
                method: "GET",
                host: "example.amazonaws.com",
                path: "/",
                query: "",
                headers: &[
                    ("host", "example.amazonaws.com"),
                    ("x-amz-date", "20150830T123600Z"),
                ],
                body: b"",
            },
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        // `get-vanilla-query-order-key-case`.
        let query = canonical_query(&mut [
            ("Param2".into(), "value2".into()),
            ("Param1".into(), "value1".into()),
        ]);
        assert_eq!(query, "Param1=value1&Param2=value2");
        let auth = authorization(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "service",
            "20150830T123600Z",
            &Request {
                method: "GET",
                host: "example.amazonaws.com",
                path: "/",
                query: &query,
                headers: &[
                    ("host", "example.amazonaws.com"),
                    ("x-amz-date", "20150830T123600Z"),
                ],
                body: b"",
            },
        );
        assert!(
            auth.ends_with("b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"),
            "{auth}"
        );
    }

    #[test]
    fn regions_are_checked() {
        for good in ["eu-west-3", "us-east-1", "ap-southeast-2", "us-gov-west-1"] {
            assert!(valid_region(good), "{good}");
        }
        for bad in [
            "",
            "eu",
            "eu-west",
            "EU-west-1",
            "eu-west-x",
            "eu--1",
            "eu-west-3.evil",
        ] {
            assert!(!valid_region(bad), "{bad}");
        }
    }

    #[test]
    fn instances_and_checks_read_from_answers() {
        let root = xml::parse(include_str!("../tests/fixtures/describe_instances.xml")).unwrap();
        let items: Vec<Instance> = root
            .find("instancesSet")
            .unwrap()
            .all("item")
            .filter_map(|i| Instance::read(i, "eu-west-3"))
            .collect();
        assert_eq!(items.len(), 2);
        let web = &items[0];
        assert_eq!(web.id, "i-0123456789abcdef0");
        assert_eq!(web.name, "web-01");
        assert_eq!(web.zone, "eu-west-3a");
        assert_eq!(web.cpu_count, Some(2.0));
        assert_eq!(web.macs, ["0a:1b:2c:3d:4e:5f"]);
        assert_eq!(
            web.addresses,
            ["10.0.1.15", "10.0.1.16", "2001:db8::15", "203.0.113.15"]
        );
        assert_eq!(items[1].name, "i-0fedcba9876543210", "no Name tag");
        assert_eq!(items[1].zone, "eu-west-3", "no placement");

        let status = xml::parse(
            "<r><item><instanceId>i-1</instanceId><systemStatus><status>ok</status></systemStatus>\
             <instanceStatus><status>impaired</status></instanceStatus></item>\
             <item><instanceId>i-2</instanceId><systemStatus><status>initializing</status></systemStatus></item></r>",
        )
        .unwrap();
        let checks: Vec<Checks> = status.all("item").map(Checks::read).collect();
        assert_eq!(
            checks[0],
            Checks {
                failed: 1,
                status: Status::Critical
            }
        );
        assert_eq!(
            checks[1],
            Checks {
                failed: 0,
                status: Status::Warning
            }
        );
        let vm = web.clone().into_vm(Some(&checks[0]));
        assert_eq!(vm.measures, [("status_checks_failed", 1.0)]);
        assert_eq!(vm.vm.status, Some(Status::Critical));
        assert!(vm.vm.powered_on);
        assert_eq!(vm.size.as_deref(), Some("t3.micro"));
        let stopped = items[1].clone().into_vm(Some(&checks[0]));
        assert!(
            stopped.measures.is_empty(),
            "a stopped instance has no checks"
        );
    }

    #[test]
    fn errors_say_what_was_refused() {
        assert_eq!(
            refusal("AuthFailure", "AWS was not able to validate").reason(),
            "login"
        );
        assert_eq!(
            refusal("UnauthorizedOperation", "You are not authorized").reason(),
            "permission"
        );
        assert_eq!(refusal("RequestLimitExceeded", "slow down").reason(), "api");
        let root = xml::parse(
            "<GetMetricDataResponse><GetMetricDataResult><MetricDataResults>\
             <member><Id>q0_0</Id><Values><member>12.5</member><member>3</member></Values></member>\
             <member><Id>q0_1</Id><Values></Values></member>\
             </MetricDataResults></GetMetricDataResult></GetMetricDataResponse>",
        )
        .unwrap();
        assert_eq!(latest(&root), [("q0_0".to_string(), 12.5)]);
    }
}
