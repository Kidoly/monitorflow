//! The PRTG reader, through its HTTP API with an API token or a user and
//! passhash. It gives a [`tree::Tree`] of probes, groups, devices and
//! sensors, which [`mapping`] turns into the inventory. Nothing is ever written to PRTG:
//! the API reader sends only GET requests to reading endpoints.

pub mod api;
pub mod mapping;
pub mod sensors;
pub mod tree;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, Url};

use crate::http;
use crate::model::Inventory;
use crate::source::{Connection, Context, Field, Input, ReadError, Source, SourceInfo};

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
            extensions: &[],
            many_files: false,
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
            Input::Files(_) => {
                return Err(ReadError::Input(
                    "PRTG is read through its API: connect to it instead.".into(),
                ))
            }
            Input::Connect(connection) => {
                let base = http::base_url(connection.get("url").unwrap_or_default(), "url")?;
                let fetch = HttpFetch {
                    client: http::client(context)?,
                    base,
                    auth: auth(&connection)?,
                };
                api::read(Arc::new(fetch), context.now).await?
            }
        };
        Ok(mapping::inventory(&tree, context.now))
    }
}
