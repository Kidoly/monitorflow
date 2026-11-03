//! tonic over our own rustls configs.
//!
//! tonic's `ServerTlsConfig` and `ClientTlsConfig` cannot take a custom
//! client verifier or CRLs, so the server is fed already-handshaken streams
//! through `serve_with_incoming`, and the client dials through a custom
//! connector. tonic still extracts peer certificates from those streams.

use std::future::Future;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ServerConfig};
use tokio::net::{lookup_host, TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::time::timeout;
use tokio_rustls::{client, server, TlsAcceptor, TlsConnector};
use tokio_stream::wrappers::ReceiverStream;
use tonic::codegen::http::Uri;
use tonic::transport::{Channel, Endpoint};
use tower::Service;

use super::TlsError;
use crate::grpc_url::{GrpcUrl, DEFAULT_GRPC_PORT};

/// Default for TCP connect and TLS handshake timeouts, both sides.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Handshaken connections waiting for tonic to pick them up.
const BACKLOG: usize = 64;

/// Limits for [`incoming`].
#[derive(Debug, Clone)]
pub struct AcceptOptions {
    /// Handshakes in progress at once. When reached, no new connection is
    /// accepted until one finishes; the rest wait in the kernel backlog.
    pub max_handshakes: usize,
    /// A client that does not finish its handshake in time is dropped.
    pub handshake_timeout: Duration,
}

impl Default for AcceptOptions {
    fn default() -> Self {
        Self {
            max_handshakes: 1024,
            handshake_timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Accepts connections on `listener` and yields them once the TLS handshake
/// succeeds, for `tonic::transport::Server::serve_with_incoming`.
///
/// Handshakes run concurrently, up to `options.max_handshakes`, so a slow
/// client cannot stall the others and a flood of idle connections cannot
/// exhaust file descriptors. Each handshake uses the config current in
/// `config` at accept time: send a new one (for example with a fresh CRL) to
/// apply it to new connections. For a fixed config, pass the receiver of
/// `watch::channel(config)`.
pub fn incoming(
    listener: TcpListener,
    config: watch::Receiver<Arc<ServerConfig>>,
    options: AcceptOptions,
) -> ReceiverStream<Result<server::TlsStream<TcpStream>, io::Error>> {
    let (tx, rx) = mpsc::channel(BACKLOG);
    let slots = Arc::new(Semaphore::new(options.max_handshakes.max(1)));
    tokio::spawn(async move {
        loop {
            let permit = tokio::select! {
                _ = tx.closed() => return,
                permit = slots.clone().acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                },
            };
            let (tcp, addr) = tokio::select! {
                _ = tx.closed() => return,
                accepted = listener.accept() => match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Usually out of file descriptors; back off.
                        tracing::warn!(%error, "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                },
            };
            let acceptor = TlsAcceptor::from(config.borrow().clone());
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tcp.set_nodelay(true);
                let handshake = timeout(options.handshake_timeout, acceptor.accept(tcp)).await;
                drop(permit);
                // Failures are routine (scanners, revoked agents) and cheap to
                // trigger, so they stay at debug.
                match handshake {
                    Ok(Ok(tls)) => {
                        let _ = tx.send(Ok(tls)).await;
                    }
                    Ok(Err(error)) => tracing::debug!(%addr, %error, "TLS handshake rejected"),
                    Err(_) => tracing::debug!(%addr, "TLS handshake timed out"),
                }
            });
        }
    });
    ReceiverStream::new(rx)
}

/// Builds a tonic endpoint for a gRPC upstream URL (see [`GrpcUrl::parse`]).
///
/// tonic refuses `https` URIs with a custom connector, so the endpoint
/// carries `http://` with the port made explicit. This does not weaken
/// anything: the transport is whatever the connector returns, and
/// [`connector`] always does mTLS.
pub fn endpoint(url: &str) -> Result<Endpoint, TlsError> {
    let invalid = |reason: String| TlsError::Url {
        url: url.to_owned(),
        reason,
    };
    let parsed = GrpcUrl::parse(url).map_err(invalid)?;
    Endpoint::from_shared(format!("http://{}", parsed.authority()))
        .map_err(|e| invalid(e.to_string()))
}

/// Connects to `url` over mTLS, giving up on the TCP connect or the TLS
/// handshake after `connect_timeout` each ([`DEFAULT_TIMEOUT`] is a sane
/// value). The server name checked against the server's certificate is the
/// URL host.
pub async fn connect(
    url: &str,
    config: Arc<ClientConfig>,
    connect_timeout: Duration,
) -> Result<Channel, TlsError> {
    endpoint(url)?
        .connect_timeout(connect_timeout)
        .connect_with_connector(connector(config, connect_timeout))
        .await
        .map_err(|error| TlsError::Connect {
            url: url.to_owned(),
            error,
        })
}

/// Connector for `Endpoint::connect_with_connector[_lazy]`, for callers that
/// need to tune the endpoint (keepalive, lazy connect) before connecting.
pub fn connector(
    config: Arc<ClientConfig>,
    connect_timeout: Duration,
) -> impl Service<
    Uri,
    Response = TokioIo<client::TlsStream<TcpStream>>,
    Error = io::Error,
    Future = impl Future<Output = io::Result<TokioIo<client::TlsStream<TcpStream>>>> + Send,
> + Clone
       + Send
       + 'static {
    let connector = TlsConnector::from(config);
    tower::service_fn(move |uri: Uri| dial(connector.clone(), connect_timeout, uri))
}

async fn dial(
    connector: TlsConnector,
    connect_timeout: Duration,
    uri: Uri,
) -> io::Result<TokioIo<client::TlsStream<TcpStream>>> {
    let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
    let timed_out =
        |what: &str| io::Error::new(io::ErrorKind::TimedOut, format!("{what} timed out"));
    let host = uri
        .host()
        .ok_or_else(|| invalid(format!("{uri}: no host")))?;
    // IPv6 literals come bracketed from the URI.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(DEFAULT_GRPC_PORT);
    let name = ServerName::try_from(host.to_owned())
        .map_err(|e| invalid(format!("{host}: invalid server name: {e}")))?;
    let tcp = connect_any(host, port, connect_timeout).await?;
    tcp.set_nodelay(true)?;
    let tls = timeout(connect_timeout, connector.connect(name, tcp))
        .await
        .map_err(|_| timed_out("TLS handshake"))??;
    Ok(TokioIo::new(tls))
}

/// Connects to one of the addresses `host` resolves to, each tried in
/// turn with its own timeout, from a random one: a name with one address
/// per server instance spreads its peers, and a dead instance costs one
/// timeout, not every attempt (ADR-0089).
async fn connect_any(host: &str, port: u16, connect_timeout: Duration) -> io::Result<TcpStream> {
    let addresses: Vec<SocketAddr> = timeout(connect_timeout, lookup_host((host, port)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "name lookup timed out"))??
        .collect();
    let mut last = io::Error::new(
        io::ErrorKind::NotFound,
        format!("{host}: no address to connect to"),
    );
    for address in rotated(addresses, RandomState::new().hash_one(host)) {
        match timeout(connect_timeout, TcpStream::connect(address)).await {
            Ok(Ok(tcp)) => return Ok(tcp),
            Ok(Err(error)) => last = error,
            Err(_) => {
                last = io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("TCP connect to {address} timed out"),
                )
            }
        }
    }
    Err(last)
}

/// `items` starting at `start` modulo their count, wrapping around.
fn rotated<T>(mut items: Vec<T>, start: u64) -> Vec<T> {
    if let Ok(len) = u64::try_from(items.len()) {
        if len > 0 {
            // Lossless: the remainder is below `len`, a `usize`.
            items.rotate_left((start % len) as usize);
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_every_address_once() {
        assert_eq!(rotated(vec![1, 2, 3], 4), [2, 3, 1]);
        assert_eq!(rotated(vec![1, 2, 3], 0), [1, 2, 3]);
        assert_eq!(rotated(Vec::<u8>::new(), 7), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn a_dead_address_does_not_stop_the_next_one() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // `localhost` may resolve to ::1 first, where nothing listens.
        let tcp = connect_any("localhost", port, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(tcp.peer_addr().unwrap().port(), port);
    }

    #[test]
    fn endpoint_makes_port_explicit() {
        let ep = endpoint("https://relay.example.com").unwrap();
        assert_eq!(ep.uri().to_string(), "http://relay.example.com:7443/");
        let ep = endpoint("https://[::1]:9000").unwrap();
        assert_eq!(ep.uri().to_string(), "http://[::1]:9000/");
    }

    #[test]
    fn endpoint_uses_the_shared_url_rule() {
        let err = endpoint("https://relay/prefix").unwrap_err().to_string();
        assert_eq!(
            err,
            "invalid gRPC URL `https://relay/prefix`: path is not allowed"
        );
    }
}
