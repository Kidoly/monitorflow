//! A client against a fake vCenter that answers pages without end: every
//! page fits the limit alone, all of them together must not.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use monitorflow_vmware::soap::{Client, ServiceContent, MAX_ANSWER};
use monitorflow_vmware::tls::Trust;
use rcgen::KeyPair;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;

/// A page of one object padded to a third of the limit, with a token.
async fn answer(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = request.into_body().collect().await.unwrap().to_bytes();
    let call = String::from_utf8_lossy(&body);
    assert!(call.contains("RetrievePropertiesEx"), "{call}");
    let padding = "x".repeat(MAX_ANSWER / 3);
    let page = format!(
        r#"<?xml version="1.0"?><soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"><soapenv:Body><RetrievePropertiesExResponse xmlns="urn:vim25"><returnval><token>more</token><objects><obj type="VirtualMachine">vm-1</obj><propSet><name>name</name><val>{padding}</val></propSet></objects></returnval></RetrievePropertiesExResponse></soapenv:Body></soapenv:Envelope>"#
    );
    Ok(Response::new(Full::new(Bytes::from(page))))
}

async fn serve() -> u16 {
    let key = KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], key)
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(tcp).await {
                    let service = hyper::service::service_fn(answer);
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn pages_share_one_limit() {
    let port = serve().await;
    let mut client = Client::new("127.0.0.1", port, Trust::Look, Duration::from_secs(30)).unwrap();
    let content = ServiceContent {
        root_folder: "group-d1".into(),
        property_collector: "propertyCollector".into(),
        view_manager: "ViewManager".into(),
        session_manager: "SessionManager".into(),
        api_type: "VirtualCenter".into(),
        full_name: "fake".into(),
        version: "6.5.0".into(),
        instance_uuid: "uuid".into(),
    };
    match client.retrieve(&content, "session[1]view").await {
        Err(failure) => assert!(failure.message().contains("too large"), "{failure:?}"),
        Ok(objects) => panic!("{} objects read without end", objects.len()),
    }
}
