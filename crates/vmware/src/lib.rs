//! vCenter and ESXi checks (ADR-0156), run from a probe (ADR-0148).
//!
//! One kind, `vmware`. A collection keeps its vim25 session, reads every
//! host, cluster, virtual machine and datastore through one container
//! view and writes them as the samples of `monitorflow_sensors::virt`,
//! plus `up`: 1 when the API answered and the login worked, 0 with labels
//! `reason` (`connect`, `tls`, `certificate`, `login`, `api`) and `error`.
//!
//! Parameters: `host` (required), `port` (443), `user` and `password`
//! (a read-only account), `fingerprint` (the SHA-256 of a certificate the
//! machine's roots do not trust, confirmed by a person) and `what`:
//! `inventory` (the default) or `test`, which looks at the certificate
//! without trusting it, and logs in only when it is trusted, for "Add a
//! device". The server keeps the credentials sealed and adds them to the
//! check it sends a probe; [`SECRET_PARAMS`] are never stored with the
//! check nor shown.

pub mod tls;
pub mod xml;

use std::collections::BTreeMap;

/// The kind of the checks this crate builds.
pub const KIND: &str = "vmware";
/// Parameters that hold secrets.
pub const SECRET_PARAMS: [&str; 1] = ["password"];
/// Parameters of the credentials, secret or not.
pub const CREDENTIAL_PARAMS: [&str; 3] = ["user", "password", "fingerprint"];
/// Longest user name or password.
const MAX_SECRET: usize = 256;

/// What a check does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    Inventory,
    Test,
}

/// What a check reads and how.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub fingerprint: Option<tls::Fingerprint>,
    pub what: What,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<hidden>"))
            .field(
                "fingerprint",
                &self.fingerprint.as_ref().map(tls::show_fingerprint),
            )
            .field("what", &self.what)
            .finish()
    }
}

/// Reads and checks the parameters of a `vmware` check. Errors never
/// quote a secret.
pub fn parse(params: &BTreeMap<String, String>) -> Result<Target, String> {
    let get = |name: &str| params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty());
    if let Some(unknown) = params.keys().find(|k| {
        !["host", "port", "what"].contains(&k.as_str()) && !CREDENTIAL_PARAMS.contains(&k.as_str())
    }) {
        return Err(format!("unknown parameter `{}`", unknown.escape_debug()));
    }
    let host = get("host").ok_or("parameter `host` is required")?;
    monitorflow_sensors::port::parse_host(host).map_err(|e| format!("parameter `host`: {e}"))?;
    let port = match get("port") {
        None => 443,
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or("parameter `port` must be between 1 and 65535")?,
    };
    let what = match get("what") {
        None | Some("inventory") => What::Inventory,
        Some("test") => What::Test,
        Some(_) => return Err("parameter `what` must be `inventory` or `test`".into()),
    };
    let secret = |name: &str| -> Result<Option<String>, String> {
        match params.get(name).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(v) if v.len() > MAX_SECRET || v.chars().any(char::is_control) => Err(format!(
                "parameter `{name}` must be at most {MAX_SECRET} bytes without control characters"
            )),
            Some(v) => Ok(Some(v.clone())),
        }
    };
    let user = secret("user")?.map(|u| u.trim().to_owned());
    let password = secret("password")?;
    if what == What::Inventory && (user.is_none() || password.is_none()) {
        return Err("a `vmware` check needs a `user` and a `password`".into());
    }
    let fingerprint = match get("fingerprint") {
        None => None,
        Some(text) => Some(tls::parse_fingerprint(text).ok_or(
            "parameter `fingerprint` must be a SHA-256 fingerprint: 64 hexadecimal digits",
        )?),
    };
    Ok(Target {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
        port,
        user,
        password,
        fingerprint,
        what,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parameters_are_checked() {
        let target = parse(&params(&[
            ("host", "vcenter.example.test"),
            ("user", " reader@vsphere.local "),
            ("password", "secret password"),
        ]))
        .unwrap();
        assert_eq!(target.port, 443);
        assert_eq!(target.what, What::Inventory);
        assert_eq!(target.user.as_deref(), Some("reader@vsphere.local"));
        assert!(!format!("{target:?}").contains("secret password"));
        let pinned = parse(&params(&[
            ("host", "[2001:db8::5]"),
            ("port", "8989"),
            ("user", "u"),
            ("password", "p"),
            ("fingerprint", &format!("{}0A", "0A:".repeat(31))),
        ]))
        .unwrap();
        assert_eq!(pinned.host, "2001:db8::5");
        assert_eq!(pinned.fingerprint, Some([0x0a; 32]));
        // A test needs no credentials.
        assert!(parse(&params(&[("host", "esx1"), ("what", "test")])).is_ok());
        for bad in [
            params(&[("host", "esx1")]),
            params(&[("host", "esx1"), ("what", "test"), ("port", "0")]),
            params(&[("host", "a b"), ("what", "test")]),
            params(&[("host", "esx1"), ("what", "everything")]),
            params(&[("host", "esx1"), ("what", "test"), ("fingerprint", "abc")]),
            params(&[("host", "esx1"), ("what", "test"), ("community", "x")]),
            params(&[("host", "esx1"), ("user", "u"), ("password", "a\nb")]),
        ] {
            let error = parse(&bad).unwrap_err();
            assert!(!error.contains("a\nb"), "{error}");
        }
    }
}
