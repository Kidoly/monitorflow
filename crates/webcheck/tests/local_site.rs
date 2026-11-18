//! Website checks against a local web server, over HTTP and over HTTPS
//! with certificates of a throwaway authority: valid, expired, for another
//! name, expiring soon and self-signed (ADR-0150).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use monitorflow_webcheck::certificate::Problem;
use monitorflow_webcheck::{Failure, Limits, Outcome, Spec, WebCheck};
use rcgen::{CertificateParams, DnType, IsCa, Issuer, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::RootCertStore;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::time::Instant;

async fn page(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = request.uri().path().to_owned();
    let answer = |status: u16, body: &str| {
        Response::builder()
            .status(status)
            .body(Full::new(Bytes::from(body.to_owned())))
            .unwrap()
    };
    let redirect = |to: &str| {
        Response::builder()
            .status(302)
            .header("location", to)
            .body(Full::new(Bytes::new()))
            .unwrap()
    };
    Ok(match path.as_str() {
        "/" => answer(200, "<h1>Welcome to the shop</h1>"),
        "/error" => answer(200, "<h1>Database Error</h1>"),
        "/missing" => answer(404, "not here"),
        "/slow" => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            answer(200, "late")
        }
        "/moved" => redirect("/"),
        "/away" => redirect("ftp://elsewhere/"),
        "/loop" => redirect("/loop"),
        "/private" => {
            // "monitor:s3cret"
            let ok = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                == Some("Basic bW9uaXRvcjpzM2NyZXQ=");
            if ok {
                answer(200, "members")
            } else {
                answer(401, "who are you")
            }
        }
        "/headers" => {
            let probe = request
                .headers()
                .get("x-probe")
                .and_then(|v| v.to_str().ok());
            answer(200, &format!("probe={}", probe.unwrap_or("none")))
        }
        _ => answer(500, "?"),
    })
}

/// Serves [`page`] on a free local port, over TLS when `tls` is given.
async fn serve(tls: Option<rustls::ServerConfig>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tls.map(|c| tokio_rustls::TlsAcceptor::from(Arc::new(c)));
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(page);
                let http = hyper::server::conn::http1::Builder::new();
                match acceptor {
                    Some(acceptor) => {
                        if let Ok(stream) = acceptor.accept(tcp).await {
                            let _ = http.serve_connection(TokioIo::new(stream), service).await;
                        }
                    }
                    None => {
                        let _ = http.serve_connection(TokioIo::new(tcp), service).await;
                    }
                }
            });
        }
    });
    addr
}

struct Authority {
    issuer: Issuer<'static, KeyPair>,
    der: CertificateDer<'static>,
}

fn authority() -> Authority {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "Website check test CA");
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    Authority {
        der: cert.der().clone(),
        issuer: Issuer::new(params, key),
    }
}

/// A server certificate for `name`, valid from `from` to `to` days from
/// now, signed by `ca` or by itself.
fn server_config(ca: Option<&Authority>, name: &str, from: i64, to: i64) -> rustls::ServerConfig {
    let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    let now = OffsetDateTime::now_utc();
    params.not_before = now + time::Duration::days(from);
    params.not_after = now + time::Duration::days(to);
    let key = KeyPair::generate().unwrap();
    let cert = match ca {
        Some(ca) => params.signed_by(&key, &ca.issuer).unwrap(),
        None => params.self_signed(&key).unwrap(),
    };
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap()
}

fn limits(ca: &Authority) -> Limits {
    let mut roots = RootCertStore::empty();
    roots.add(ca.der.clone()).unwrap();
    Limits {
        targets: None,
        roots: Some(Arc::new(roots)),
    }
}

async fn check(limits: &Limits, pairs: &[(&str, &str)]) -> Outcome {
    let params: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let check = WebCheck::new(Spec::parse(&params).unwrap(), limits.clone());
    check.run(Instant::now() + Duration::from_secs(2)).await
}

#[tokio::test]
async fn pages_over_http() {
    let addr = serve(None).await;
    let url = |path: &str| format!("http://{addr}{path}");
    let none = Limits::default();

    let ok = check(&none, &[("url", &url("/")), ("contains", "Welcome")]).await;
    assert_eq!(ok.failure, None);
    assert_eq!(ok.status, Some(200));
    assert_eq!(ok.content_ok, Some(true));
    assert!(ok.seconds.unwrap() < 2.0);
    assert!(ok.certificate.is_none());

    let missing = check(&none, &[("url", &url("/missing"))]).await;
    assert_eq!(
        (missing.failure, missing.status),
        (Some(Failure::Status), Some(404))
    );
    let expected = check(&none, &[("url", &url("/missing")), ("status", "404")]).await;
    assert_eq!(expected.failure, None);

    let refused = check(&none, &[("url", &url("/error")), ("not_contains", "Error")]).await;
    assert_eq!(refused.failure, Some(Failure::Content));
    let absent = check(&none, &[("url", &url("/error")), ("contains", "Welcome")]).await;
    assert_eq!(absent.failure, Some(Failure::Content));

    let head = check(&none, &[("url", &url("/")), ("method", "HEAD")]).await;
    assert_eq!((head.failure, head.content_ok), (None, None));

    let headers = check(
        &none,
        &[
            ("url", &url("/headers")),
            ("headers", "X-Probe: yes"),
            ("contains", "probe=yes"),
        ],
    )
    .await;
    assert_eq!(headers.failure, None);
}

#[tokio::test]
async fn redirects_followed_or_not() {
    let addr = serve(None).await;
    let url = |path: &str| format!("http://{addr}{path}");
    let none = Limits::default();

    let followed = check(&none, &[("url", &url("/moved"))]).await;
    assert_eq!((followed.failure, followed.status), (None, Some(200)));
    assert_eq!(followed.redirects, 1);
    assert_eq!(followed.final_url.as_str(), url("/"));

    let kept = check(
        &none,
        &[("url", &url("/moved")), ("follow_redirects", "false")],
    )
    .await;
    assert_eq!(
        (kept.failure, kept.status, kept.redirects),
        (None, Some(302), 0)
    );
    let strict = check(
        &none,
        &[
            ("url", &url("/moved")),
            ("follow_redirects", "false"),
            ("status", "200"),
        ],
    )
    .await;
    assert_eq!(strict.failure, Some(Failure::Status));

    let looping = check(&none, &[("url", &url("/loop"))]).await;
    assert_eq!(looping.failure, Some(Failure::Redirects));
    assert_eq!(looping.redirects, 11);
    let away = check(&none, &[("url", &url("/away"))]).await;
    assert_eq!(away.failure, Some(Failure::Redirects));
}

#[tokio::test]
async fn slow_closed_and_unknown_sites() {
    let addr = serve(None).await;
    let none = Limits::default();
    let started = Instant::now();
    let slow = check(&none, &[("url", &format!("http://{addr}/slow"))]).await;
    assert_eq!(slow.failure, Some(Failure::Timeout));
    assert!(started.elapsed() < Duration::from_millis(2500));

    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed = free.local_addr().unwrap();
    drop(free);
    let refused = check(&none, &[("url", &format!("http://{closed}/"))]).await;
    assert_eq!(refused.failure, Some(Failure::Connect));

    let unknown = check(&none, &[("url", "http://does-not-exist.invalid/")]).await;
    assert_eq!(unknown.failure, Some(Failure::Dns));

    let v4 = check(
        &none,
        &[
            ("url", &format!("http://localhost:{}/", addr.port())),
            ("ip", "4"),
        ],
    )
    .await;
    assert_eq!(v4.failure, None);
    let only6 = check(&none, &[("url", "http://localhost:1/"), ("ip", "6")]).await;
    assert!(matches!(
        only6.failure,
        Some(Failure::Connect | Failure::Dns)
    ));
}

#[tokio::test]
async fn certificates() {
    let ca = authority();
    let trusted = limits(&ca);
    let site = |config| async move { serve(Some(config)).await.port() };

    let valid = site(server_config(Some(&ca), "localhost", -1, 90)).await;
    let outcome = check(&trusted, &[("url", &format!("https://localhost:{valid}/"))]).await;
    assert_eq!(outcome.failure, None);
    let cert = outcome.certificate.unwrap();
    assert_eq!(cert.problem, None);
    assert!(cert.name_ok);
    assert_eq!(cert.issuer, "Website check test CA");
    assert_eq!(cert.subject, "localhost");
    let days = cert.days_left(OffsetDateTime::now_utc().unix_timestamp());
    assert!((89.0..=90.0).contains(&days), "{days}");

    let expired = site(server_config(Some(&ca), "localhost", -30, -2)).await;
    let outcome = check(
        &trusted,
        &[("url", &format!("https://localhost:{expired}/"))],
    )
    .await;
    // The page is still read: what is wrong is said by the certificate.
    assert_eq!(outcome.failure, None);
    let cert = outcome.certificate.unwrap();
    assert_eq!(cert.problem, Some(Problem::Expired));
    assert!(cert.days_left(OffsetDateTime::now_utc().unix_timestamp()) < -1.0);

    let wrong = site(server_config(Some(&ca), "elsewhere.test", -1, 90)).await;
    let cert = check(&trusted, &[("url", &format!("https://localhost:{wrong}/"))])
        .await
        .certificate
        .unwrap();
    assert_eq!(cert.problem, Some(Problem::Name));
    assert!(!cert.name_ok);

    let soon = site(server_config(Some(&ca), "localhost", -80, 5)).await;
    let cert = check(&trusted, &[("url", &format!("https://localhost:{soon}/"))])
        .await
        .certificate
        .unwrap();
    assert_eq!(cert.problem, None);
    let days = cert.days_left(OffsetDateTime::now_utc().unix_timestamp());
    assert!((4.0..=5.0).contains(&days), "{days}");

    let own = site(server_config(None, "localhost", -1, 90)).await;
    let cert = check(&trusted, &[("url", &format!("https://localhost:{own}/"))])
        .await
        .certificate
        .unwrap();
    assert_eq!(cert.problem, Some(Problem::SelfSigned));

    // The machine's own roots do not know the test authority.
    let cert = check(
        &Limits::default(),
        &[("url", &format!("https://localhost:{valid}/"))],
    )
    .await
    .certificate
    .unwrap();
    assert_eq!(cert.problem, Some(Problem::Untrusted));
}

#[tokio::test]
async fn passwords_go_only_to_a_trusted_certificate() {
    let ca = authority();
    let trusted = limits(&ca);
    let valid = serve(Some(server_config(Some(&ca), "localhost", -1, 90)))
        .await
        .port();
    let url = format!("https://localhost:{valid}/private");

    let without = check(&trusted, &[("url", &url)]).await;
    assert_eq!(
        (without.failure, without.status),
        (Some(Failure::Status), Some(401))
    );
    let with = check(
        &trusted,
        &[
            ("url", &url),
            ("username", "monitor"),
            ("password", "s3cret"),
        ],
    )
    .await;
    assert_eq!((with.failure, with.status), (None, Some(200)));

    let own = serve(Some(server_config(None, "localhost", -1, 90)))
        .await
        .port();
    let refused = check(
        &trusted,
        &[
            ("url", &format!("https://localhost:{own}/private")),
            ("username", "monitor"),
            ("password", "s3cret"),
        ],
    )
    .await;
    assert_eq!(refused.failure, Some(Failure::Certificate));
    assert_eq!(refused.status, None, "no request was sent");
}
