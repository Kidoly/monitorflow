//! The PRTG reader: through its HTTP API, with an API token or a user and
//! passhash, or from its configuration file (`PRTG Configuration.dat`).
//! Both give a [`tree::Tree`] of probes, groups, devices and sensors, which
//! [`mapping`] turns into the inventory. Nothing is ever written to PRTG:
//! the API reader sends only GET requests to reading endpoints.

pub mod api;
pub mod discovery;
pub mod export;
pub mod lookups;
pub mod mapping;
pub mod sensors;
pub mod snmp;
pub mod tree;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Duration;
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, Url};

use crate::http;
use crate::model::Inventory;
use crate::source::{
    blocking, Connection, Context, Field, File, Input, ReadError, Source, SourceInfo,
};

/// The PRTG reader.
pub struct Prtg;

/// The API, authenticated by query parameters as PRTG asks.
struct HttpFetch {
    client: Client,
    base: Url,
    auth: Vec<(&'static str, String)>,
}

#[async_trait]
impl api::Fetch for HttpFetch {
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError> {
        let mut url = http::join(&self.base, path);
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())))
            .extend_pairs(self.auth.iter().map(|(k, v)| (*k, v.as_str())));
        let body = http::send(&self.client, Method::GET, url, HeaderMap::new(), None).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

/// The credentials of `connection`: an API token, or a user and passhash.
fn auth(connection: &Connection) -> Result<Vec<(&'static str, String)>, ReadError> {
    match (
        connection.get("apitoken"),
        connection.get("username"),
        connection.get("passhash"),
    ) {
        (Some(token), None, None) => Ok(vec![("apitoken", token.to_owned())]),
        (None, Some(user), Some(passhash)) => Ok(vec![
            ("username", user.to_owned()),
            ("passhash", passhash.to_owned()),
        ]),
        _ => Err(ReadError::Input(
            "Fill in an API token, or a user and its passhash.".into(),
        )),
    }
}

/// Gives each custom SNMP sensor the lookups its channels and settings
/// name, with their values from `lookups` when a file had them.
pub fn attach_lookups(
    tree: &mut tree::Tree,
    lookups: &std::collections::BTreeMap<String, crate::model::SnmpLookup>,
) {
    for object in &mut tree.objects {
        let custom = object
            .sensor_type
            .as_deref()
            .is_some_and(|t| sensors::family(t) == sensors::Family::SnmpCustom);
        if !custom {
            continue;
        }
        let mut ids: Vec<String> = object
            .settings
            .iter()
            .filter(|(k, _)| k.ends_with("valuelookup") || k.as_str() == "lookups")
            .map(|(_, v)| v.trim().to_ascii_lowercase())
            .collect();
        for channel in &mut object.channels {
            if let Some(lookup) = &mut channel.lookup {
                ids.push(lookup.name.clone());
                if let Some(found) = lookups.get(&lookup.name) {
                    *lookup = found.clone();
                }
            }
        }
        for id in ids.into_iter().filter(|i| !i.is_empty() && i != "none") {
            if let Some(found) = lookups.get(&id) {
                object.lookups.insert(id, found.clone());
            }
        }
    }
}

#[async_trait]
impl Source for Prtg {
    fn info(&self) -> SourceInfo {
        let field = |key, secret, optional, placeholder| Field {
            key,
            secret,
            optional,
            placeholder,
        };
        SourceInfo {
            id: "prtg",
            name: "PRTG",
            extensions: &["dat", "xml", "ovl"],
            many_files: true,
            connect: vec![
                field("url", false, false, "https://prtg.example.com"),
                field("apitoken", true, true, ""),
                field("username", false, true, "prtgadmin"),
                field("passhash", true, true, ""),
            ],
        }
    }

    async fn read(&self, input: Input, context: &Context) -> Result<Inventory, ReadError> {
        let tree = match input {
            Input::Files(files) => blocking(move || read_files(&files)).await?,
            Input::Connect(connection) => {
                let base = http::base_url(connection.get("url").unwrap_or_default(), "url")?;
                let fetch = HttpFetch {
                    client: http::client(context)?,
                    base,
                    auth: auth(&connection)?,
                };
                let mut tree = api::read(Arc::new(fetch), context.now).await?;
                // The API has no lookup files: channels name theirs only.
                attach_lookups(&mut tree, &Default::default());
                tree
            }
        };
        let now = context.now;
        blocking(move || Ok(mapping::inventory(&tree, now))).await
    }
}

/// The tree of an upload: the configuration file, and lookup files
/// (`.ovl`) beside it for custom SNMP sensors (ADR-0157).
fn read_files(files: &[File]) -> Result<tree::Tree, ReadError> {
    let is_lookup = |name: &str| name.to_ascii_lowercase().ends_with(".ovl");
    let file = files
        .iter()
        .find(|f| !is_lookup(&f.name))
        .ok_or_else(|| ReadError::Input("Choose PRTG's configuration file to read.".into()))?;
    if files.iter().filter(|f| !is_lookup(&f.name)).count() > 1 {
        return Err(ReadError::Input(
            "Choose one configuration file, with lookup files (.ovl) if you have some.".into(),
        ));
    }
    let text = String::from_utf8_lossy(&file.content);
    // The file does not say PRTG's time zone: read as UTC, and
    // the windows say so.
    let mut tree = export::read(&file.name, &text, Duration::zero())?;
    tree.local_times = true;
    let ovl: Vec<(String, String)> = files
        .iter()
        .filter(|f| is_lookup(&f.name))
        .map(|f| {
            (
                f.name.clone(),
                String::from_utf8_lossy(&f.content).into_owned(),
            )
        })
        .collect();
    let (lookups, failed) = lookups::read_all(&ovl);
    for (name, why) in failed {
        tracing::debug!(file = %name, %why, "a PRTG lookup file does not read");
    }
    attach_lookups(&mut tree, &lookups);
    Ok(tree)
}
