//! The Prometheus reader (ADR-0146): uploaded files (`prometheus.yml`,
//! file service discovery files, rule files, `alertmanager.yml`,
//! `blackbox.yml`), or a running Prometheus and Alertmanager through their
//! APIs.
//!
//! Both give a [`Collected`]: targets after relabeling, alerting rules,
//! the Alertmanager configuration and its silences. Then
//! [`hosts::Fleet`] makes hosts and checks of the targets, [`rules`] adds
//! alerts, thresholds and service checks, and [`alertmanager`] channels
//! and silences.

use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::model::{Inventory, Unmovable};
use crate::source::{Context, Field, Input, ReadError, Source, SourceInfo, MAX_HOSTS};

pub mod alertmanager;
pub mod api;
pub mod blackbox;
pub mod config;
pub mod files;
pub mod hosts;
pub mod promql;
pub mod rules;
pub mod targets;

/// Prometheus's scrape interval when the configuration sets none.
pub const DEFAULT_INTERVAL_S: u32 = 60;

/// What the reader read, before it is mapped.
#[derive(Debug, Default)]
pub struct Collected {
    pub targets: Vec<targets::Target>,
    /// Seconds between scrapes, by job.
    pub intervals: BTreeMap<String, u32>,
    pub rules: Vec<rules::Rule>,
    pub alertmanager: Option<config::AmConfig>,
    pub silences: Vec<alertmanager::SilenceIn>,
    /// Blackbox modules by name, from `blackbox.yml`.
    pub modules: BTreeMap<String, config::BlackboxModule>,
    /// What could not be read at all: missing files, other discoveries.
    pub unmovable: Vec<Unmovable>,
}

impl Collected {
    /// The inventory these describe.
    pub fn inventory(&self) -> Inventory {
        let mut fleet = hosts::Fleet::new(self);
        rules::apply(&mut fleet, &self.rules);
        if let Some(config) = &self.alertmanager {
            alertmanager::channels(&mut fleet.inventory, config);
        }
        alertmanager::silences(&mut fleet, &self.silences);
        let mut inventory = fleet.inventory;
        inventory.unmovable.extend(self.unmovable.iter().cloned());
        inventory
    }
}

/// The reader.
pub struct Prometheus;

#[async_trait]
impl Source for Prometheus {
    fn info(&self) -> SourceInfo {
        SourceInfo {
            id: "prometheus",
            name: "Prometheus",
            extensions: &["yml", "yaml", "json", "rules"],
            many_files: true,
            connect: vec![
                Field {
                    key: "url",
                    secret: false,
                    optional: false,
                    placeholder: "http://prometheus.example.com:9090",
                },
                Field {
                    key: "alertmanager_url",
                    secret: false,
                    optional: true,
                    placeholder: "http://alertmanager.example.com:9093",
                },
                Field {
                    key: "token",
                    secret: true,
                    optional: true,
                    placeholder: "",
                },
            ],
        }
    }

    async fn read(&self, input: Input, context: &Context) -> Result<Inventory, ReadError> {
        let collected = match input {
            Input::Files(files) => files::collect(&files)?,
            Input::Connect(connection) => api::collect(&connection, context).await?,
        };
        let inventory = collected.inventory();
        if inventory.hosts.len() > MAX_HOSTS {
            return Err(ReadError::Input(format!(
                "More than {MAX_HOSTS} hosts: import them in parts."
            )));
        }
        Ok(inventory)
    }
}
