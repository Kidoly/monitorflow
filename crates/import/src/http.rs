//! Requests to another tool's API: http or https only, no redirects, a
//! timeout, answers read up to a cap, and only the addresses the person
//! may reach (ADR-0163). Errors are sentences for the person who
//! connected, without the token.

use std::fmt;
use std::sync::Arc;

use reqwest::dns::Resolve;
use reqwest::header::HeaderMap;
use reqwest::{Method, Url};

use crate::source::{Context, ReadError};

/// Largest answer read, in bytes.
pub const MAX_ANSWER: usize = 64 * 1024 * 1024;

/// What a connection may reach, for a person who may not make the server
/// reach its own side of the network (ADR-0163). Without one, any address.
pub trait Guard: Send + Sync + fmt::Debug {
    /// Refuses `url` when its host is an address written out that the
    /// person may not reach; a sentence saying why. Names are left to
    /// [`Guard::resolver`].
    fn check(&self, url: &Url) -> Result<(), String>;

    /// Resolves names to the addresses the person may reach only. The
    /// connection uses what it returns, so a name that changes after a
    /// check cannot slip through.
    fn resolver(&self) -> Arc<dyn Resolve>;
}

/// A guard's resolver, as the client builder takes one.
struct Resolver(Arc<dyn Resolve>);

impl Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.0.resolve(name)
    }
}

/// A client and the guard of its connection.
#[derive(Clone)]
pub struct Client {
    inner: reqwest::Client,
    guard: Option<Arc<dyn Guard>>,
}

/// A client for `context`. Redirects are not followed: a token sent to one
/// address never goes to another. With a guard, no proxy either, which
/// would resolve names itself.
pub fn client(context: &Context) -> Result<Client, ReadError> {
    let mut builder = reqwest::Client::builder()
        .timeout(context.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("MonitorFlow-import/", env!("CARGO_PKG_VERSION")));
    if let Some(guard) = &context.guard {
        builder = builder
            .no_proxy()
            .dns_resolver(Arc::new(Resolver(guard.resolver())));
    }
    let inner = builder
        .build()
        .map_err(|e| ReadError::Connect(format!("Cannot prepare the connection: {e}")))?;
    Ok(Client {
        inner,
        guard: context.guard.clone(),
    })
}

/// The URL typed in `field`, checked: http or https, with a host, without a
/// user or password in it.
pub fn base_url(text: &str, field: &str) -> Result<Url, ReadError> {
    let mut text = text.trim().to_owned();
    if !text.contains("://") {
        text = format!("https://{text}");
    }
    let url = Url::parse(&text)
        .map_err(|_| ReadError::Input(format!("{field}: {text} is not a URL.")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ReadError::Input(format!(
            "{field}: use an http or https address."
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ReadError::Input(format!(
            "{field}: put no user or password in the address."
        )));
    }
    Ok(url)
}

/// `base` with `path` appended to its path.
pub fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let joined = format!(
        "{}/{}",
        base.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    url.set_path(&joined);
    url
}

/// Sends a request and reads the answer, up to [`MAX_ANSWER`] bytes.
pub async fn send(
    client: &Client,
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
) -> Result<Vec<u8>, ReadError> {
    let shown = shown(&url);
    if let Some(guard) = &client.guard {
        guard.check(&url).map_err(ReadError::Connect)?;
    }
    let mut request = client.inner.request(method, url).headers(headers);
    if let Some(body) = body {
        request = request.body(body);
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| ReadError::Connect(format!("Cannot reach {shown}: {}", plain(&e))))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(ReadError::Refused(format!(
            "{shown} refused the credentials ({status})."
        )));
    }
    if status.is_redirection() {
        return Err(ReadError::Connect(format!(
            "{shown} answered with a redirect ({status}): type the address it redirects to."
        )));
    }
    if !status.is_success() {
        return Err(ReadError::Connect(format!("{shown} answered {status}.")));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ReadError::Connect(format!("{shown}: {}", plain(&e))))?
    {
        if body.len() + chunk.len() > MAX_ANSWER {
            return Err(ReadError::Connect(format!(
                "{shown} answered more than {} MB.",
                MAX_ANSWER / (1024 * 1024)
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A URL without its query, which may hold a token.
fn shown(url: &Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.to_string()
}

/// The innermost cause of a reqwest error, which says what failed.
fn plain(error: &reqwest::Error) -> String {
    let mut source: &dyn std::error::Error = error;
    while let Some(next) = source.source() {
        source = next;
    }
    source.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked() {
        assert_eq!(
            base_url("zabbix.example", "url").unwrap().as_str(),
            "https://zabbix.example/"
        );
        assert!(base_url("http://10.0.0.1:8080/zabbix", "url").is_ok());
        assert!(base_url("ftp://x", "url").is_err());
        assert!(base_url("https://user:pw@x", "url").is_err());
        assert!(base_url("", "url").is_err());
    }

    #[test]
    fn paths_are_joined() {
        let base = Url::parse("http://x/zabbix/").unwrap();
        assert_eq!(
            join(&base, "/api_jsonrpc.php").as_str(),
            "http://x/zabbix/api_jsonrpc.php"
        );
        let base = Url::parse("http://x").unwrap();
        assert_eq!(
            join(&base, "api/v1/rules").as_str(),
            "http://x/api/v1/rules"
        );
    }

    /// Refuses loopback addresses, written out or resolved.
    #[derive(Debug)]
    struct NoLoopback;

    struct NoNames;

    impl Resolve for NoNames {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let name = name.as_str().to_owned();
            Box::pin(async move { Err(format!("{name} is refused").into()) })
        }
    }

    impl Guard for NoLoopback {
        fn check(&self, url: &Url) -> Result<(), String> {
            match url
                .host_str()
                .and_then(|h| h.parse::<std::net::IpAddr>().ok())
            {
                Some(ip) if ip.is_loopback() => Err(format!("{ip} is refused")),
                _ => Ok(()),
            }
        }

        fn resolver(&self) -> Arc<dyn Resolve> {
            Arc::new(NoNames)
        }
    }

    /// With a guard, nothing reaches what it refuses: neither an address
    /// written out nor a name; without one, the same address is reached.
    #[tokio::test]
    async fn a_guard_keeps_requests_from_what_it_refuses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let guarded = Context::new(chrono::Utc::now()).with_guard(Arc::new(NoLoopback));
        let client = client(&guarded).unwrap();
        for url in [
            format!("http://127.0.0.1:{port}/api"),
            format!("http://localhost:{port}/api"),
        ] {
            let url = Url::parse(&url).unwrap();
            let error = send(&client, Method::GET, url, HeaderMap::new(), None)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("refused"), "{error}");
        }
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "a request reached the refused address");

        let open = client_for_any();
        let url = Url::parse(&format!("http://127.0.0.1:{port}/api")).unwrap();
        let request =
            tokio::spawn(
                async move { send(&open, Method::GET, url, HeaderMap::new(), None).await },
            );
        assert!(listener.accept().await.is_ok());
        request.abort();
    }

    fn client_for_any() -> Client {
        client(&Context::new(chrono::Utc::now())).unwrap()
    }

    #[test]
    fn queries_are_not_shown() {
        let url = Url::parse("http://x/a?token=s3cret").unwrap();
        assert_eq!(shown(&url), "http://x/a");
    }
}
