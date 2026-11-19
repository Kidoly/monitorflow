//! Zabbix (ADR-0146): configuration exports of hosts, templates and media
//! types in YAML, XML or JSON (Zabbix 5.0 to 7.4), or a live Zabbix
//! through its JSON-RPC API with an API token.
//!
//! Reading goes in two steps: exports ([`export`]) and the API ([`api`])
//! both give a [`config::Config`], the configuration in Zabbix's terms,
//! which [`map`] turns into the neutral inventory. Items become checks or
//! measures every server has ([`items`]), triggers thresholds or alerts
//! ([`triggers`], with the expression parser in [`expression`] and user
//! macros in [`macros`]), maintenances silences ([`maintenance`]), media
//! types channels ([`media`]), discovery rules network scans
//! ([`discovery`]). Stock templates an export links without
//! including are read from Zabbix's defaults ([`stock`]).

pub mod api;
pub mod config;
pub mod discovery;
pub mod export;
pub mod expression;
pub mod items;
pub mod macros;
pub mod maintenance;
pub mod map;
pub mod media;
pub mod services;
pub mod stock;
pub mod triggers;
pub mod web;

use async_trait::async_trait;

use crate::model::Inventory;
use crate::source::{Context, Field, Input, ReadError, Source, SourceInfo};

/// The Zabbix reader.
pub struct Zabbix;

#[async_trait]
impl Source for Zabbix {
    fn info(&self) -> SourceInfo {
        SourceInfo {
            id: "zabbix",
            name: "Zabbix",
            extensions: &["yaml", "yml", "xml", "json"],
            many_files: true,
            connect: vec![
                Field {
                    key: "url",
                    secret: false,
                    optional: false,
                    placeholder: "https://zabbix.example.com",
                },
                Field {
                    key: "token",
                    secret: true,
                    optional: false,
                    placeholder: "",
                },
            ],
        }
    }

    async fn read(&self, input: Input, context: &Context) -> Result<Inventory, ReadError> {
        let config = match input {
            Input::Files(files) => export::read(&files)?,
            Input::Connect(connection) => {
                let mut api = api::Api::connect(&connection, context).await?;
                api.read().await?
            }
        };
        if config.hosts.len() > crate::source::MAX_HOSTS {
            return Err(ReadError::Input(format!(
                "More than {} hosts: import them in parts.",
                crate::source::MAX_HOSTS
            )));
        }
        let inventory = map::inventory(&config, context.now);
        if inventory.is_empty() {
            return Err(ReadError::Input(
                "Nothing to import: the files or the Zabbix have no hosts, media types, maintenances or discovery rules."
                    .into(),
            ));
        }
        Ok(inventory)
    }
}
