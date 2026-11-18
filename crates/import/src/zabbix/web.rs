//! Web scenarios and HTTP agent items as website checks (ADR-0150).
//!
//! A scenario becomes one check of its first step: its URL and query
//! fields, required codes and string, redirects, headers (the scenario's
//! and the step's) and authentication. What a check cannot do (later
//! steps, variables, a POST, a regular expression, NTLM) is noted, and the
//! rest is kept. A password is never read: the planner says to set it.

use reqwest::Url;

use crate::args;
use crate::model::{WebMethod, Website, ANY_STATUS};
use crate::notes::Note;
use crate::web;

use super::config::{HttpAgent, WebAuth, WebRequest, WebScenario};
use super::macros::Macros;

/// `text` with its macros expanded, or the note of the first one without
/// a value; a low-level discovery macro has none either.
fn expand(text: &str, macros: &Macros) -> Result<String, Note> {
    let expanded = macros
        .expand(text)
        .map_err(|name| Note::new("unresolved_macro", args! {"macro" => name}))?;
    if let Some(at) = expanded.find("{#") {
        let end = expanded[at..]
            .find('}')
            .map_or(expanded.len(), |e| at + e + 1);
        return Err(Note::new(
            "unresolved_macro",
            args! {"macro" => &expanded[at..end]},
        ));
    }
    Ok(expanded)
}

fn push(notes: &mut Vec<Note>, note: Note) {
    if !notes.contains(&note) {
        notes.push(note);
    }
}

/// The URL Zabbix requests: `http://` when it names no scheme, with the
/// query fields added.
fn url_of(request: &WebRequest, macros: &Macros) -> Result<String, Note> {
    let url = expand(request.url.trim(), macros)?;
    let url = if url.contains("://") {
        url
    } else {
        format!("http://{url}")
    };
    if request.query.is_empty() {
        return Ok(url);
    }
    let Ok(mut parsed) = Url::parse(&url) else {
        return Ok(url);
    };
    {
        let mut query = parsed.query_pairs_mut();
        for (name, value) in &request.query {
            query.append_pair(&expand(name, macros)?, &expand(value, macros)?);
        }
    }
    Ok(parsed.to_string())
}

/// The check `request` makes, with `auth` and the scenario's `shared`
/// headers, and the notes of what it leaves out, most telling first.
/// `Err` when it cannot be read at all.
pub fn website(
    request: &WebRequest,
    auth: &WebAuth,
    shared: &[(String, String)],
    macros: &Macros,
) -> Result<(Website, Vec<Note>), Note> {
    let mut notes = Vec::new();
    let mut site = Website::new(url_of(request, macros)?);
    site.method = match request.method.trim().to_ascii_uppercase().as_str() {
        "" | "GET" if request.headers_only => WebMethod::Head,
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
    if request.body && notes.is_empty() {
        push(
            &mut notes,
            Note::new("web_setting", args! {"setting" => "posts"}),
        );
    }
    let codes = expand(request.status_codes.trim(), macros)?;
    site.status = if codes.is_empty() {
        Some(ANY_STATUS.to_owned())
    } else {
        let list = web::status_list(&codes);
        if list.is_none() {
            push(
                &mut notes,
                Note::new("web_setting", args! {"setting" => "status_codes"}),
            );
        }
        list
    };
    let required = expand(&request.required, macros)?;
    if !required.is_empty() {
        if site.method == WebMethod::Head {
            // Zabbix looked in the headers; a check reads the page.
            push(
                &mut notes,
                Note::new("web_setting", args! {"setting" => "retrieve_mode"}),
            );
        } else {
            match web::condition(&required, true, "required") {
                Ok(text) => site.contains = Some(text),
                Err(note) => push(&mut notes, note),
            }
        }
    }
    site.follow_redirects = request.follow_redirects;
    for (name, value) in shared.iter().chain(&request.headers) {
        site.headers
            .push((expand(name, macros)?, expand(value, macros)?));
    }
    if request.variables {
        push(
            &mut notes,
            Note::new("web_setting", args! {"setting" => "variables"}),
        );
    }
    match auth.kind.as_str() {
        "" => {}
        "basic" => {
            let user = expand(auth.user.trim(), macros).unwrap_or_default();
            site.username = Some(user).filter(|u| !u.is_empty());
            site.credentials = auth.password;
        }
        other => push(&mut notes, Note::new("web_auth", args! {"kind" => other})),
    }
    if let Some(timeout) = &request.timeout {
        site.timeout_s = expand(timeout, macros)
            .ok()
            .and_then(|t| crate::text::seconds(&t))
            .and_then(|s| u32::try_from(s).ok())
            .filter(|s| *s > 0);
    }
    Ok((site, notes))
}

/// The check of a web scenario: its first step.
pub fn scenario(scenario: &WebScenario, macros: &Macros) -> Result<(Website, Vec<Note>), Note> {
    let Some(first) = scenario.steps.first() else {
        return Err(Note::plain("no_equivalent"));
    };
    let (site, mut notes) = website(first, &scenario.auth, &scenario.headers, macros)?;
    if scenario.variables {
        push(
            &mut notes,
            Note::new("web_setting", args! {"setting" => "variables"}),
        );
    }
    if scenario.steps.len() > 1 {
        notes.insert(
            0,
            Note::new("web_steps", args! {"count" => scenario.steps.len()}),
        );
    }
    Ok((site, notes))
}

/// The check of an HTTP agent item.
pub fn item(http: &HttpAgent, macros: &Macros) -> Result<(Website, Vec<Note>), Note> {
    website(&http.request, &http.auth, &[], macros)
}

#[cfg(test)]
mod tests {
    use super::super::config::Macro;
    use super::super::macros::HostValues;
    use super::*;

    fn macros() -> Macros {
        Macros::new(
            vec![vec![
                Macro {
                    name: "{$SHOP}".into(),
                    value: Some("shop.example.com".into()),
                },
                Macro {
                    name: "{$CODES}".into(),
                    value: Some("200,301".into()),
                },
            ]],
            HostValues::default(),
        )
    }

    fn step(url: &str) -> WebRequest {
        WebRequest {
            url: url.into(),
            method: "GET".into(),
            follow_redirects: true,
            ..WebRequest::default()
        }
    }

    #[test]
    fn a_plain_step_maps_exactly() {
        let mut request = step("https://{$SHOP}/");
        request.status_codes = "{$CODES}".into();
        request.required = "Welcome".into();
        request.follow_redirects = false;
        request.headers = vec![("Accept-Language".into(), "en".into())];
        request.timeout = Some("10s".into());
        let (site, notes) = website(&request, &WebAuth::default(), &[], &macros()).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(site.url, "https://shop.example.com/");
        assert_eq!(site.status.as_deref(), Some("200,301"));
        assert_eq!(site.contains.as_deref(), Some("Welcome"));
        assert!(!site.follow_redirects);
        assert_eq!(site.headers, vec![("Accept-Language".into(), "en".into())]);
        assert_eq!(site.timeout_s, Some(10));
    }

    #[test]
    fn what_a_step_leaves_out_is_noted() {
        // No scheme, query fields, any code, a pattern, a POST.
        let mut request = step("shop.example.com/search");
        request.query = vec![("q".into(), "a b".into())];
        request.required = "Total: \\d+".into();
        request.method = "POST".into();
        request.body = true;
        let (site, notes) = website(&request, &WebAuth::default(), &[], &macros()).unwrap();
        assert_eq!(site.url, "http://shop.example.com/search?q=a+b");
        assert_eq!(site.status.as_deref(), Some(ANY_STATUS));
        assert_eq!(site.contains, None);
        assert_eq!(site.method, WebMethod::Get);
        assert_eq!(
            notes,
            vec![
                Note::new("web_method", args! {"method" => "POST"}),
                Note::new("web_regex", args! {"pattern" => "Total: \\d+"}),
            ]
        );
        // Headers only is a HEAD request.
        let mut request = step("https://shop.example.com/");
        request.headers_only = true;
        let (site, _) = website(&request, &WebAuth::default(), &[], &macros()).unwrap();
        assert_eq!(site.method, WebMethod::Head);
    }

    #[test]
    fn authentication() {
        let basic = WebAuth {
            kind: "basic".into(),
            user: "monitor".into(),
            password: true,
        };
        let (site, notes) =
            website(&step("https://shop.example.com/"), &basic, &[], &macros()).unwrap();
        assert!(notes.is_empty());
        assert_eq!(site.username.as_deref(), Some("monitor"));
        assert!(site.credentials);
        let ntlm = WebAuth {
            kind: "ntlm".into(),
            ..basic
        };
        let (site, notes) =
            website(&step("https://shop.example.com/"), &ntlm, &[], &macros()).unwrap();
        assert!(!site.credentials);
        assert_eq!(notes, vec![Note::new("web_auth", args! {"kind" => "ntlm"})]);
    }

    #[test]
    fn scenarios_keep_their_first_step() {
        let scenario = WebScenario {
            name: "Shop".into(),
            delay: None,
            enabled: true,
            steps: vec![
                step("https://shop.example.com/"),
                step("https://shop.example.com/cart"),
            ],
            headers: vec![("X-Monitor".into(), "zabbix".into())],
            variables: false,
            auth: WebAuth::default(),
        };
        let (site, notes) = super::scenario(&scenario, &macros()).unwrap();
        assert_eq!(site.url, "https://shop.example.com/");
        assert_eq!(site.headers, vec![("X-Monitor".into(), "zabbix".into())]);
        assert_eq!(notes, vec![Note::new("web_steps", args! {"count" => 2})]);
        let empty = WebScenario {
            steps: vec![],
            ..scenario
        };
        assert_eq!(
            super::scenario(&empty, &macros()).unwrap_err(),
            Note::plain("no_equivalent")
        );
        assert_eq!(
            website(
                &step("https://{$NOPE}/"),
                &WebAuth::default(),
                &[],
                &macros()
            )
            .unwrap_err()
            .code,
            "unresolved_macro"
        );
    }
}
