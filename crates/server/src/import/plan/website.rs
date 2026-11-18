//! A source's web check as a website check (ADR-0150): the parameters of
//! a check of kind `http`, its name, and the alerts the web interface adds
//! with one.

use monitorflow_import::args;
use monitorflow_import::model::{Args, IpFamily, WebMethod, Website};
use monitorflow_import::Note;
use serde_json::{json, Map, Value};

use crate::import::NoteView;

/// The alert of a website that stops answering as expected, when the
/// source has none: critical after 2 minutes, as the web interface adds
/// it.
pub const UP_FOR_S: u32 = 120;

/// Most extra headers a check sends.
const MAX_HEADERS: usize = 16;

fn note(code: &str, args: Args) -> NoteView {
    (&Note::new(code, args)).into()
}

/// What a website check is named after: its URL's host, with the port
/// when it is not the scheme's own, as the web interface shows it.
pub fn label(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            (None, _) => url.to_owned(),
        },
        Err(_) => url.to_owned(),
    }
}

/// Whether the check speaks TLS, and so has a certificate to watch.
pub fn https(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("https://")
}

/// The parameters of the website check `site`, and the note of what it
/// leaves for the person to do or does not send: a password above all,
/// since the check fails without it. Never a secret: the source's
/// password or token stays behind.
pub fn params(site: &Website) -> (Map<String, Value>, Option<NoteView>) {
    let mut params = Map::new();
    params.insert("url".into(), json!(site.url.trim()));
    if site.method == WebMethod::Head {
        params.insert("method".into(), json!("HEAD"));
    }
    let texts = [
        ("status", &site.status),
        ("contains", &site.contains),
        ("not_contains", &site.not_contains),
    ];
    for (key, value) in texts {
        if let Some(value) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            params.insert(key.into(), json!(value));
        }
    }
    if !site.follow_redirects {
        params.insert("follow_redirects".into(), json!("false"));
    }
    match site.ip {
        IpFamily::Any => {}
        IpFamily::V4 => {
            params.insert("ip".into(), json!("4"));
        }
        IpFamily::V6 => {
            params.insert("ip".into(), json!("6"));
        }
    }
    let mut dropped = None;
    let mut token = false;
    let mut headers = Vec::new();
    for (name, value) in &site.headers {
        if name.trim().eq_ignore_ascii_case("authorization") {
            // A token in a header: a secret, kept out like a password.
            token = true;
            continue;
        }
        let line = format!("{}: {}", name.trim(), value.trim());
        let fits = headers.len() < MAX_HEADERS;
        if fits && monitorflow_webcheck::spec::parse_headers(&line).is_ok() {
            headers.push(line);
        } else if dropped.is_none() {
            dropped = Some(note("web_header", args! {"name" => name.trim()}));
        }
    }
    if !headers.is_empty() {
        params.insert("headers".into(), json!(headers.join("\n")));
    }
    // The user name goes with the password, which the person sets: the
    // check refuses one without the other.
    let password = if site.credentials || token || site.username.is_some() {
        Some(if !https(&site.url) {
            note("web_password_http", args! {})
        } else {
            match &site.username {
                Some(user) => note("web_password", args! {"username" => user}),
                None => note("web_token", args! {}),
            }
        })
    } else {
        None
    };
    (params, password.or(dropped))
}

/// A threshold the web interface adds with a website check.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// After `<name>_`.
    pub suffix: &'static str,
    pub comparison: &'static str,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub duration_s: u32,
}

/// The alerts that come with a website check besides its own: its
/// certificate for https, and a slow answer.
pub fn rules(url: &str) -> Vec<Rule> {
    let rule = |suffix, comparison, warning, critical, duration_s| Rule {
        suffix,
        comparison,
        warning,
        critical,
        duration_s,
    };
    let mut rules = Vec::new();
    if https(url) {
        rules.push(rule("cert_ok", "<", None, Some(1.0), 300));
        rules.push(rule("cert_days_left", "<", Some(14.0), Some(7.0), 0));
    }
    rules.push(rule("response_seconds", ">", Some(3.0), None, 300));
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_hosts_with_their_port() {
        assert_eq!(label("https://Shop.Example.com/a"), "shop.example.com");
        assert_eq!(label("https://shop.example.com:443/"), "shop.example.com");
        assert_eq!(
            label("http://shop.example.com:8080/"),
            "shop.example.com:8080"
        );
        assert_eq!(label("https://[fd00::1]:8443/"), "[fd00::1]:8443");
        assert_eq!(label("not a url"), "not a url");
    }

    #[test]
    fn every_setting_becomes_a_parameter() {
        let mut site = Website::new("https://shop.example.com/");
        site.method = WebMethod::Head;
        site.status = Some("200,301".into());
        site.follow_redirects = false;
        site.headers = vec![("Accept-Language".into(), "en".into())];
        site.ip = IpFamily::V6;
        let (params, note) = super::params(&site);
        assert_eq!(
            Value::Object(params),
            json!({
                "url": "https://shop.example.com/",
                "method": "HEAD",
                "status": "200,301",
                "follow_redirects": "false",
                "headers": "Accept-Language: en",
                "ip": "6",
            })
        );
        assert_eq!(note, None);
    }

    #[test]
    fn secrets_stay_behind() {
        let mut site = Website::new("https://shop.example.com/");
        site.username = Some("monitor".into());
        site.credentials = true;
        let (params, note) = super::params(&site);
        assert!(!params.contains_key("username") && !params.contains_key("password"));
        assert_eq!(note.unwrap().code, "web_password");
        site.username = None;
        assert_eq!(params_note(&site), "web_token");
        site.url = "http://shop.example.com/".into();
        assert_eq!(params_note(&site), "web_password_http");
        // A token in a header is not sent in clear either.
        let mut site = Website::new("https://api.example.com/");
        site.headers = vec![
            ("Authorization".into(), "Bearer abc".into()),
            ("X-Monitor".into(), "yes".into()),
        ];
        let (params, note) = super::params(&site);
        assert_eq!(params["headers"], json!("X-Monitor: yes"));
        assert_eq!(note.unwrap().code, "web_token");
    }

    fn params_note(site: &Website) -> String {
        super::params(site).1.unwrap().code
    }

    #[test]
    fn headers_the_check_refuses_are_noted() {
        let mut site = Website::new("https://shop.example.com/");
        site.headers = vec![
            ("Host".into(), "other.example.com".into()),
            ("Cookie".into(), "session=1".into()),
            ("X-Ok".into(), "1".into()),
        ];
        let (params, note) = super::params(&site);
        assert_eq!(params["headers"], json!("X-Ok: 1"));
        let note = note.unwrap();
        assert_eq!(note.code, "web_header");
        assert_eq!(note.args["name"], "Host");
    }

    #[test]
    fn the_alerts_of_a_website() {
        let suffixes = |url| rules(url).iter().map(|r| r.suffix).collect::<Vec<_>>();
        assert_eq!(
            suffixes("https://shop.example.com/"),
            ["cert_ok", "cert_days_left", "response_seconds"]
        );
        assert_eq!(suffixes("http://shop.example.com/"), ["response_seconds"]);
    }
}
