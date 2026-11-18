//! Blackbox exporter modules as website checks (ADR-0150): the `http`
//! prober's settings, and `tcp` with `tls`, which checks a certificate.
//! A module the reader has not seen (no `blackbox.yml` uploaded, or read
//! through the API) is taken as the usual `http_2xx`.

use crate::args;
use crate::model::{IpFamily, WebMethod, Website};
use crate::notes::Note;
use crate::web;

use super::config::{BlackboxModule, HttpProbe};

fn push(notes: &mut Vec<Note>, note: Note) {
    if !notes.contains(&note) {
        notes.push(note);
    }
}

fn setting(name: &str) -> Note {
    Note::new("web_setting", args! {"setting" => name})
}

/// The addresses a probe tries: either family when it may fall back,
/// otherwise the preferred one (IPv6 unless told otherwise).
fn ip(preferred: &str, fallback: Option<bool>) -> IpFamily {
    if fallback.unwrap_or(true) {
        return IpFamily::Any;
    }
    match preferred.trim() {
        "ip4" | "ipv4" | "4" => IpFamily::V4,
        _ => IpFamily::V6,
    }
}

fn timeout(module: &BlackboxModule) -> Option<u32> {
    module
        .timeout
        .as_deref()
        .and_then(crate::text::seconds)
        .and_then(|s| u32::try_from(s).ok())
        .filter(|s| *s > 0)
}

/// Texts that fail the probe when found, or when missing: the first that
/// is plain text becomes the check's condition, the rest a note.
fn texts(patterns: &[String], setting_name: &str, notes: &mut Vec<Note>) -> Option<String> {
    let mut kept = None;
    for pattern in patterns {
        match web::condition(pattern, true, setting_name) {
            Ok(text) if kept.is_none() => kept = Some(text),
            Ok(_) => push(notes, setting(setting_name)),
            Err(note) => push(notes, note),
        }
    }
    kept
}

/// The website check of an `http` probe of `url` with `module`, and what
/// it leaves out, most telling first.
pub fn http(url: &str, module: Option<&BlackboxModule>) -> (Website, Vec<Note>) {
    let mut site = Website::new(url);
    let mut notes = Vec::new();
    let Some(module) = module else {
        return (site, notes);
    };
    let probe: &HttpProbe = &module.http;
    site.method = match probe.method.trim().to_ascii_uppercase().as_str() {
        "" | "GET" => WebMethod::Get,
        "HEAD" => WebMethod::Head,
        other => {
            push(
                &mut notes,
                Note::new("web_method", args! {"method" => other}),
            );
            WebMethod::Get
        }
    };
    if (!probe.body.is_empty() || !probe.body_file.is_empty()) && notes.is_empty() {
        push(&mut notes, setting("body"));
    }
    site.follow_redirects = probe
        .follow_redirects
        .unwrap_or(!probe.no_follow_redirects.unwrap_or(false));
    site.status = web::status_codes(&probe.valid_status_codes);
    if site.status.is_none() && !site.follow_redirects {
        // The exporter's default is 2xx; the check's, without following,
        // takes a redirect too.
        site.status = Some("200-299".into());
    }
    if site.method == WebMethod::Get {
        site.contains = texts(
            &probe.fail_if_body_not_matches_regexp,
            "fail_if_body_not_matches_regexp",
            &mut notes,
        );
        site.not_contains = texts(
            &probe.fail_if_body_matches_regexp,
            "fail_if_body_matches_regexp",
            &mut notes,
        );
    } else if !probe.fail_if_body_not_matches_regexp.is_empty()
        || !probe.fail_if_body_matches_regexp.is_empty()
    {
        push(&mut notes, setting("fail_if_body_matches_regexp"));
    }
    if !probe.fail_if_header_matches.is_empty() {
        push(&mut notes, setting("fail_if_header_matches"));
    }
    if !probe.fail_if_header_not_matches.is_empty() {
        push(&mut notes, setting("fail_if_header_not_matches"));
    }
    if probe.fail_if_ssl {
        push(&mut notes, setting("fail_if_ssl"));
    }
    if probe.fail_if_not_ssl && !url.trim().to_ascii_lowercase().starts_with("https://") {
        push(&mut notes, setting("fail_if_not_ssl"));
    }
    site.headers = probe
        .headers
        .iter()
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    if let Some(basic) = &probe.basic_auth {
        site.username = Some(basic.username.trim().to_owned()).filter(|u| !u.is_empty());
        site.credentials = basic.password.is_some() || basic.password_file.is_some();
    }
    if probe.bearer_token.is_some()
        || probe.bearer_token_file.is_some()
        || probe.authorization.is_some()
    {
        site.credentials = true;
    }
    if probe.oauth2.is_some() {
        push(
            &mut notes,
            Note::new("web_auth", args! {"kind" => "OAuth 2.0"}),
        );
    }
    site.ip = ip(&probe.preferred_ip_protocol, probe.ip_protocol_fallback);
    site.timeout_s = timeout(module);
    (site, notes)
}

/// The website check of a `tcp` probe with TLS of `host:port`: its
/// certificate, whatever the page answers.
pub fn certificate(host: &str, port: u16, module: &BlackboxModule) -> (Website, Vec<Note>) {
    let mut site = Website::new(web::https_url(host, port)).any_status();
    site.ip = ip(
        &module.tcp.preferred_ip_protocol,
        module.tcp.ip_protocol_fallback,
    );
    site.timeout_s = timeout(module);
    let mut notes = vec![Note::plain("web_certificate_only")];
    if !module.tcp.query_response.is_empty() {
        push(&mut notes, setting("query_response"));
    }
    (site, notes)
}

#[cfg(test)]
mod tests {
    use super::super::config::BlackboxConfig;
    use super::*;

    fn modules(yaml: &str) -> BlackboxConfig {
        serde_norway::from_str(yaml).unwrap()
    }

    #[test]
    fn the_usual_module_maps_exactly() {
        let config = modules(
            "modules:\n  http_2xx:\n    prober: http\n    timeout: 5s\n    http:\n      valid_status_codes: []\n      method: GET\n      preferred_ip_protocol: ip4\n",
        );
        let (site, notes) = http("https://shop.example.com/", config.modules.get("http_2xx"));
        assert!(notes.is_empty(), "{notes:?}");
        let mut expected = Website::new("https://shop.example.com/");
        expected.timeout_s = Some(5);
        assert_eq!(site, expected);
        // Unknown module: the defaults.
        assert_eq!(
            http("https://shop.example.com/", None),
            (Website::new("https://shop.example.com/"), vec![])
        );
    }

    #[test]
    fn every_setting_that_maps() {
        let config = modules(
            r#"
modules:
  shop:
    prober: http
    http:
      valid_status_codes: [200, 301]
      method: HEAD
      no_follow_redirects: true
      headers:
        Accept-Language: en
      basic_auth:
        username: monitor
        password: s3cret
      preferred_ip_protocol: ip6
      ip_protocol_fallback: false
  api:
    prober: http
    http:
      fail_if_body_not_matches_regexp: ["Welcome"]
      fail_if_body_matches_regexp: ["Maintenance\\ mode"]
      follow_redirects: false
      bearer_token: abc
"#,
        );
        let (site, notes) = http("https://shop.example.com/", config.modules.get("shop"));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(site.method, WebMethod::Head);
        assert_eq!(site.status.as_deref(), Some("200,301"));
        assert!(!site.follow_redirects);
        assert_eq!(site.headers, vec![("Accept-Language".into(), "en".into())]);
        assert_eq!(site.username.as_deref(), Some("monitor"));
        assert!(site.credentials);
        assert_eq!(site.ip, IpFamily::V6);
        let (site, notes) = http("https://api.example.com/", config.modules.get("api"));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(site.contains.as_deref(), Some("Welcome"));
        assert_eq!(site.not_contains.as_deref(), Some("Maintenance mode"));
        // Not following, with the exporter's 2xx default.
        assert_eq!(site.status.as_deref(), Some("200-299"));
        assert!(site.credentials && site.username.is_none());
    }

    #[test]
    fn what_does_not_map_is_noted() {
        let config = modules(
            r#"
modules:
  post:
    prober: http
    http:
      method: POST
      body: '{"q": 1}'
      fail_if_body_not_matches_regexp: ['"status":\s*"ok"']
      fail_if_header_matches:
        - header: Server
          regexp: nginx
"#,
        );
        let (site, notes) = http("https://api.example.com/", config.modules.get("post"));
        assert_eq!(site.method, WebMethod::Get);
        assert_eq!(site.contains, None);
        assert_eq!(
            notes,
            vec![
                Note::new("web_method", args! {"method" => "POST"}),
                Note::new("web_regex", args! {"pattern" => "\"status\":\\s*\"ok\""}),
                setting("fail_if_header_matches"),
            ]
        );
    }

    #[test]
    fn tls_probes_check_the_certificate() {
        let config = modules("modules:\n  tls:\n    prober: tcp\n    tcp:\n      tls: true\n");
        let (site, notes) = certificate("mail.example.com", 8443, &config.modules["tls"]);
        assert_eq!(site.url, "https://mail.example.com:8443/");
        assert_eq!(site.status.as_deref(), Some(crate::model::ANY_STATUS));
        assert_eq!(notes, vec![Note::plain("web_certificate_only")]);
    }
}
