//! What an import's connection may reach (ADR-0163): any address for a
//! global administrator, public addresses only for someone else, as a
//! notification channel they save (ADR-0078). The central server's side
//! of the network (its database, its stores, cloud metadata) is not
//! theirs to probe.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use monitorflow_import::http::Guard;
use monitorflow_import::Context;
use reqwest::dns::Resolve;
use reqwest::Url;

use crate::notify::send::{refuse_private_address_in, PublicResolver};

/// Public addresses only.
#[derive(Debug)]
struct PublicOnly;

impl Guard for PublicOnly {
    fn check(&self, url: &Url) -> Result<(), String> {
        refuse_private_address_in(url.as_str()).map_err(|e| e.to_string())
    }

    fn resolver(&self) -> Arc<dyn Resolve> {
        Arc::new(PublicResolver)
    }
}

/// The context of a reading at `now` by someone who is a global
/// administrator or not.
pub fn context(now: DateTime<Utc>, global_admin: bool) -> Context {
    let context = Context::new(now);
    if global_admin {
        context
    } else {
        context.with_guard(Arc::new(PublicOnly))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_global_administrator_reaches_any_address() {
        assert!(context(Utc::now(), true).guard.is_none());
    }

    #[test]
    fn someone_else_reaches_public_addresses_only() {
        let context = context(Utc::now(), false);
        let guard = context.guard.unwrap();
        for refused in [
            "http://127.0.0.1:9090/",
            "http://10.1.2.3/zabbix",
            "http://169.254.169.254/latest/meta-data/",
            "https://[::1]:8443/",
            "http://[::ffff:127.0.0.1]/",
            "http://100.64.0.1/",
        ] {
            let error = guard.check(&Url::parse(refused).unwrap()).unwrap_err();
            assert!(error.contains("private address"), "{refused}: {error}");
        }
        for allowed in ["https://zabbix.example.com/", "http://93.184.216.34:8080/"] {
            assert!(
                guard.check(&Url::parse(allowed).unwrap()).is_ok(),
                "{allowed}"
            );
        }
    }

    #[tokio::test]
    async fn names_resolve_to_public_addresses_only() {
        let context = context(Utc::now(), false);
        let resolver = context.guard.unwrap().resolver();
        let resolved = resolver.resolve("localhost".parse().unwrap()).await;
        assert!(resolved.is_err());
    }
}
