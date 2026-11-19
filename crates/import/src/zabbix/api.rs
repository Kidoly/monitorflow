//! Reading a Zabbix through its JSON-RPC API (`api_jsonrpc.php`), with an
//! API token: as `Authorization: Bearer` from Zabbix 6.4, in the request's
//! `auth` field before. Only `.get` methods are called: nothing is ever
//! written to Zabbix. The token never goes into an error or a log.
//!
//! Hosts' items, triggers and web scenarios come with what their templates
//! give them, and discovered items and triggers are read as such (a file
//! system's threshold for `/` rather than for every file system); a
//! prototype is read only when its rule has discovered nothing yet.

use std::collections::{BTreeMap, HashMap, HashSet};

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, Url};
use serde_json::{json, Value};

use super::config::{
    Action, ActionSource, Config, DiscoveryCheck, DiscoveryRule, Host, HttpAgent, Interface,
    InterfaceKind, Item, ItemKind, Macro, Maintenance, MediaKind, MediaType, Period, PeriodKind,
    Priority, Service, Sla, StatusRule, TagFilter, Template, Trigger, TriggerRef, UserMedia,
    WebAuth, WebRequest, WebScenario,
};
use super::export::{list, text};
use crate::http;
use crate::source::{Connection, Context, ReadError};

/// A session with one Zabbix.
pub struct Api {
    client: Client,
    url: Url,
    token: String,
    /// Major and minor version, as `apiinfo.version` says.
    version: (u32, u32),
    next_id: u64,
}

/// The API's URL from what was typed: a Zabbix's address, with or
/// without `/zabbix`, or a page of it such as `.../api_jsonrpc.php` or
/// `.../zabbix.php?action=dashboard.view`.
pub fn endpoint(text: &str) -> Result<Url, ReadError> {
    let mut url = http::base_url(text, "url")?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().to_owned();
    let path = match path.rsplit_once('/') {
        Some((dir, page)) if page.ends_with(".php") => format!("{dir}/"),
        _ => path,
    };
    url.set_path(&path);
    Ok(http::join(&url, "api_jsonrpc.php"))
}

impl Api {
    /// Connects: asks the version, which needs no token, then checks the
    /// token with a first request.
    pub async fn connect(connection: &Connection, context: &Context) -> Result<Self, ReadError> {
        let url = endpoint(connection.get("url").unwrap_or_default())?;
        let token = connection
            .get("token")
            .ok_or_else(|| ReadError::Input("Fill in token.".into()))?
            .to_owned();
        let mut api = Self {
            client: http::client(context)?,
            url,
            token,
            version: (0, 0),
            next_id: 0,
        };
        let version = api.call("apiinfo.version", json!({}), false).await?;
        let version = version.as_str().unwrap_or_default().to_owned();
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        api.version = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        if api.version < (5, 0) {
            return Err(ReadError::Connect(format!(
                "Zabbix {version} is too old: the import reads Zabbix 5.0 and later."
            )));
        }
        tracing::debug!(version = %version, "connected to Zabbix");
        Ok(api)
    }

    fn bearer(&self) -> bool {
        self.version >= (6, 4)
    }

    /// One JSON-RPC call; `authenticated` adds the token.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        authenticated: bool,
    ) -> Result<Value, ReadError> {
        self.next_id += 1;
        let mut body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": self.next_id,
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json-rpc"),
        );
        if authenticated {
            if self.bearer() {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", self.token))
                    .map_err(|_| ReadError::Input("token: not a valid token.".into()))?;
                value.set_sensitive(true);
                headers.insert(AUTHORIZATION, value);
            } else {
                body["auth"] = json!(self.token);
            }
        }
        let body = serde_json::to_vec(&body).expect("JSON");
        let answer = http::send(
            &self.client,
            Method::POST,
            self.url.clone(),
            headers,
            Some(body),
        )
        .await?;
        let answer: Value = serde_json::from_slice(&answer).map_err(|_| {
            ReadError::Connect(format!(
                "{} did not answer as the Zabbix API does: check the address.",
                self.url
            ))
        })?;
        if let Some(error) = answer.get("error") {
            let message = format!("{} {}", text(error, "message"), text(error, "data"));
            let message = message.replace(&self.token, "[token]").trim().to_owned();
            let lower = message.to_ascii_lowercase();
            if ["not authori", "session terminated", "re-login", "token"]
                .iter()
                .any(|w| lower.contains(w))
            {
                return Err(ReadError::Refused(format!(
                    "Zabbix refused the token: {message}"
                )));
            }
            return Err(ReadError::Connect(format!(
                "Zabbix answered {method} with an error: {message}"
            )));
        }
        answer.get("result").cloned().ok_or_else(|| {
            ReadError::Connect(format!("Zabbix answered {method} without a result."))
        })
    }

    /// An authenticated `.get` call, its result as a list.
    async fn get(&mut self, method: &str, params: Value) -> Result<Vec<Value>, ReadError> {
        match self.call(method, params, true).await? {
            Value::Array(items) => Ok(items),
            _ => Err(ReadError::Connect(format!(
                "Zabbix answered {method} with something else than a list."
            ))),
        }
    }

    /// [`Self::get`], with nothing when Zabbix answers an error, as it
    /// does to a token whose role may not read these.
    async fn get_optional(&mut self, method: &str, params: Value) -> Result<Vec<Value>, ReadError> {
        match self.get(method, params).await {
            Err(ReadError::Connect(why)) => {
                tracing::debug!(%method, %why, "not read");
                Ok(Vec::new())
            }
            other => other,
        }
    }

    /// The selector and result key of host groups: `selectHostGroups` and
    /// `hostgroups` since 6.2, `selectGroups` and `groups` before.
    fn groups_selector(&self) -> (&'static str, &'static str) {
        if self.version >= (6, 2) {
            ("selectHostGroups", "hostgroups")
        } else {
            ("selectGroups", "groups")
        }
    }

    /// Everything the import reads.
    pub async fn read(&mut self) -> Result<Config, ReadError> {
        let (select_groups, groups_key) = self.groups_selector();
        let group_names: HashMap<String, String> = self
            .get("hostgroup.get", json!({"output": ["groupid", "name"]}))
            .await?
            .iter()
            .map(|g| (text(g, "groupid"), text(g, "name")))
            .collect();

        let raw_hosts = self
            .get(
                "host.get",
                json!({
                    "output": ["hostid", "host", "name", "status"],
                    "selectInterfaces": ["type", "ip", "dns", "useip", "main"],
                    "selectInventory": ["os", "os_full"],
                    "selectParentTemplates": ["templateid", "host"],
                    (select_groups): ["groupid", "name"],
                }),
            )
            .await?;
        let host_ids: Vec<String> = raw_hosts.iter().map(|h| text(h, "hostid")).collect();

        // Templates: their links and macros, for macro resolution; their
        // items, to tell which template a host's item comes from.
        let raw_templates = self
            .get(
                "template.get",
                json!({
                    "output": ["templateid", "host"],
                    "selectParentTemplates": ["templateid", "host"],
                }),
            )
            .await?;
        let template_names: HashMap<String, String> = raw_templates
            .iter()
            .map(|t| (text(t, "templateid"), text(t, "host")))
            .collect();
        let used = used_templates(&raw_hosts, &raw_templates);
        let mut owners: HashMap<String, String> = HashMap::new();
        if !used.is_empty() {
            for method in ["item.get", "itemprototype.get"] {
                for item in self
                    .get(
                        method,
                        json!({"templateids": used, "output": ["itemid", "hostid"]}),
                    )
                    .await?
                {
                    if let Some(name) = template_names.get(&text(&item, "hostid")) {
                        owners.insert(text(&item, "itemid"), name.clone());
                    }
                }
            }
        }

        let mut macro_ids = host_ids.clone();
        macro_ids.extend(used.iter().cloned());
        let mut macros: HashMap<String, Vec<Macro>> = HashMap::new();
        if !macro_ids.is_empty() {
            for m in self
                .get(
                    "usermacro.get",
                    json!({"hostids": macro_ids, "output": ["hostid", "macro", "value", "type"]}),
                )
                .await?
            {
                macros
                    .entry(text(&m, "hostid"))
                    .or_default()
                    .push(macro_of(&m));
            }
        }
        let global_macros = self
            .get(
                "usermacro.get",
                json!({"globalmacro": true, "output": ["macro", "value", "type"]}),
            )
            .await?
            .iter()
            .map(macro_of)
            .collect();

        let mut hosts: BTreeMap<String, Host> = BTreeMap::new();
        for raw in &raw_hosts {
            let id = text(raw, "hostid");
            hosts.insert(
                id.clone(),
                host_of(raw, groups_key, macros.remove(&id).unwrap_or_default()),
            );
        }

        if !host_ids.is_empty() {
            self.read_items(&host_ids, &owners, &mut hosts).await?;
            self.read_triggers(&host_ids, &mut hosts).await?;
            for scenario in self
                .get(
                    "httptest.get",
                    json!({
                        "hostids": host_ids,
                        "output": [
                            "httptestid", "hostid", "name", "delay", "status", "headers",
                            "variables", "authentication", "http_user", "http_password",
                        ],
                        "selectSteps": [
                            "no", "name", "url", "query_fields", "posts", "required",
                            "status_codes", "follow_redirects", "retrieve_mode", "headers",
                            "variables", "timeout",
                        ],
                    }),
                )
                .await?
            {
                if let Some(host) = hosts.get_mut(&text(&scenario, "hostid")) {
                    host.web.push(web_of(&scenario));
                }
            }
        }

        let templates = raw_templates
            .iter()
            .filter(|t| used.contains(&text(t, "templateid")))
            .map(|t| {
                let id = text(t, "templateid");
                Template {
                    host: text(t, "host"),
                    templates: list(t, "parentTemplates")
                        .into_iter()
                        .map(|p| text(p, "host"))
                        .collect(),
                    macros: macros.remove(&id).unwrap_or_default(),
                    items: vec![],
                    triggers: vec![],
                    web: vec![],
                }
            })
            .map(|t| (t.host.clone(), t))
            .collect();

        let host_names: HashMap<String, String> = hosts
            .iter()
            .map(|(id, h)| (id.clone(), h.host.clone()))
            .collect();
        let maintenances = self
            .get(
                "maintenance.get",
                json!({
                    "output": "extend",
                    "selectHosts": ["hostid"],
                    (select_groups): ["groupid"],
                    "selectTimeperiods": "extend",
                    "selectTags": "extend",
                }),
            )
            .await?
            .iter()
            .map(|m| maintenance_of(m, groups_key, &host_names, &group_names))
            .collect();

        let raw_media = self
            .get("mediatype.get", json!({"output": "extend"}))
            .await?;
        let media_names: HashMap<String, String> = raw_media
            .iter()
            .map(|m| (text(m, "mediatypeid"), text(m, "name")))
            .collect();
        let media_types = raw_media.iter().map(media_type_of).collect();
        let user_field = if self.version >= (5, 4) {
            "username"
        } else {
            "alias"
        };
        let mut user_media = Vec::new();
        for user in self
            .get(
                "user.get",
                json!({
                    "output": ["userid", user_field],
                    "selectMedias": ["mediatypeid", "sendto", "active"],
                }),
            )
            .await?
        {
            for media in list(&user, "medias") {
                let Some(media_type) = media_names.get(&text(media, "mediatypeid")) else {
                    continue;
                };
                let sendto = match media.get("sendto") {
                    Some(Value::Array(all)) => all
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                    _ => vec![text(media, "sendto")],
                };
                user_media.push(UserMedia {
                    user: text(&user, user_field),
                    media_type: media_type.clone(),
                    sendto,
                    enabled: text(media, "active") == "0",
                });
            }
        }

        let (discovery, actions) = self.read_discovery().await?;
        let (services, slas) = if self.version >= (6, 0) {
            self.read_services().await?
        } else {
            (vec![], vec![])
        };

        Ok(Config {
            hosts: hosts.into_values().collect(),
            templates,
            global_macros,
            maintenances,
            media_types,
            services,
            slas,
            user_media: Some(user_media),
            discovery,
            actions,
            inherited: true,
        })
    }

    /// Network discovery rules, the proxies that run them, and the
    /// actions on what they find or on agents registering. A token that
    /// may not read them reads none.
    async fn read_discovery(&mut self) -> Result<(Vec<DiscoveryRule>, Vec<Action>), ReadError> {
        let raw = self
            .get_optional(
                "drule.get",
                json!({"output": "extend", "selectDChecks": "extend"}),
            )
            .await?;
        let proxy_key = if self.version >= (7, 0) {
            "proxyid"
        } else {
            "proxy_hostid"
        };
        let mut proxies: HashMap<String, String> = HashMap::new();
        if raw
            .iter()
            .any(|r| !matches!(text(r, proxy_key).as_str(), "" | "0"))
        {
            let name = if self.version >= (7, 0) {
                "name"
            } else {
                "host"
            };
            for p in self
                .get_optional("proxy.get", json!({"output": ["proxyid", name]}))
                .await?
            {
                proxies.insert(text(&p, "proxyid"), text(&p, name));
            }
        }
        let rules = raw
            .iter()
            .map(|r| DiscoveryRule {
                id: text(r, "druleid"),
                name: text(r, "name"),
                iprange: text(r, "iprange"),
                delay: text(r, "delay"),
                enabled: text(r, "status") == "0",
                proxy: proxies.get(&text(r, proxy_key)).cloned(),
                checks: list(r, "dchecks").into_iter().map(dcheck_of).collect(),
            })
            .collect();
        let actions = self
            .get_optional(
                "action.get",
                json!({
                    "output": ["actionid", "name", "eventsource", "status"],
                    "filter": {"eventsource": ["1", "2"]},
                    "selectFilter": "extend",
                    "selectOperations": ["operationtype"],
                }),
            )
            .await?
            .iter()
            .filter_map(action_of)
            .collect();
        Ok((rules, actions))
    }

    /// Services and SLAs (ADR-0155), which exports do not have.
    async fn read_services(&mut self) -> Result<(Vec<Service>, Vec<Sla>), ReadError> {
        let raw = self
            .get(
                "service.get",
                json!({
                    "output": "extend",
                    "selectChildren": ["serviceid", "weight"],
                    "selectProblemTags": "extend",
                    "selectStatusRules": "extend",
                    "selectTags": "extend",
                }),
            )
            .await?;
        let services = raw.iter().map(service_of).collect();
        let slas = self
            .get(
                "sla.get",
                json!({
                    "output": "extend",
                    "selectServiceTags": "extend",
                    "selectSchedule": "extend",
                    "selectExcludedDowntimes": "extend",
                }),
            )
            .await?
            .iter()
            .map(sla_of)
            .collect();
        Ok((services, slas))
    }

    /// Hosts' items: plain and discovered ones, and the prototypes of
    /// rules that have discovered nothing.
    async fn read_items(
        &mut self,
        host_ids: &[String],
        owners: &HashMap<String, String>,
        hosts: &mut BTreeMap<String, Host>,
    ) -> Result<(), ReadError> {
        let fields = json!([
            "itemid",
            "hostid",
            "name",
            "key_",
            "delay",
            "type",
            "status",
            "url",
            "templateid",
            "query_fields",
            "request_method",
            "posts",
            "status_codes",
            "follow_redirects",
            "retrieve_mode",
            "headers",
            "authtype",
            "username",
            "password",
            "timeout"
        ]);
        let items = self
            .get(
                "item.get",
                json!({
                    "hostids": host_ids,
                    "output": fields,
                    "selectItemDiscovery": ["parent_itemid"],
                }),
            )
            .await?;
        let prototypes = self
            .get(
                "itemprototype.get",
                json!({"hostids": host_ids, "output": fields}),
            )
            .await?;
        let discovered: HashSet<String> = items
            .iter()
            .filter_map(|i| i.get("itemDiscovery"))
            .map(|d| text(d, "parent_itemid"))
            .filter(|p| !p.is_empty() && p != "0")
            .collect();
        // A prototype's template, for the items discovered from it.
        let prototype_owner: HashMap<String, String> = prototypes
            .iter()
            .filter_map(|p| {
                owners
                    .get(&text(p, "templateid"))
                    .map(|o| (text(p, "itemid"), o.clone()))
            })
            .collect();
        for raw in &items {
            let parent = raw
                .get("itemDiscovery")
                .map(|d| text(d, "parent_itemid"))
                .unwrap_or_default();
            let owner = owners
                .get(&text(raw, "templateid"))
                .or_else(|| prototype_owner.get(&parent))
                .cloned();
            if let Some(host) = hosts.get_mut(&text(raw, "hostid")) {
                host.items.push(item_of(raw, false, owner));
            }
        }
        for raw in &prototypes {
            if discovered.contains(&text(raw, "itemid")) {
                continue;
            }
            let owner = owners.get(&text(raw, "templateid")).cloned();
            if let Some(host) = hosts.get_mut(&text(raw, "hostid")) {
                host.items.push(item_of(raw, true, owner));
            }
        }
        Ok(())
    }

    /// Hosts' triggers, expressions expanded to `func(/host/key)`; trigger
    /// prototypes only when nothing was discovered from them.
    async fn read_triggers(
        &mut self,
        host_ids: &[String],
        hosts: &mut BTreeMap<String, Host>,
    ) -> Result<(), ReadError> {
        let fields = json!([
            "triggerid",
            "description",
            "expression",
            "recovery_mode",
            "recovery_expression",
            "priority",
            "status"
        ]);
        let triggers = self
            .get(
                "trigger.get",
                json!({
                    "hostids": host_ids,
                    "output": fields,
                    "expandExpression": true,
                    "selectHosts": ["hostid"],
                    "selectTags": "extend",
                    "selectTriggerDiscovery": ["parent_triggerid"],
                    "selectDependencies": ["triggerid", "description"],
                }),
            )
            .await?;
        let prototypes = self
            .get(
                "triggerprototype.get",
                json!({
                    "hostids": host_ids,
                    "output": fields,
                    "expandExpression": true,
                    "selectHosts": ["hostid"],
                }),
            )
            .await?;
        let discovered: HashSet<String> = triggers
            .iter()
            .filter_map(|t| t.get("triggerDiscovery"))
            .map(|d| text(d, "parent_triggerid"))
            .filter(|p| !p.is_empty() && p != "0")
            .collect();
        let all = triggers.iter().map(|t| (t, false)).chain(
            prototypes
                .iter()
                .filter(|p| !discovered.contains(&text(p, "triggerid")))
                .map(|p| (p, true)),
        );
        for (raw, prototype) in all {
            // A trigger on several hosts belongs to each of them.
            for host in list(raw, "hosts") {
                if let Some(host) = hosts.get_mut(&text(host, "hostid")) {
                    host.triggers.push(trigger_of(raw, prototype));
                }
            }
        }
        Ok(())
    }
}

/// Templates linked to hosts, directly or through other templates.
fn used_templates(hosts: &[Value], templates: &[Value]) -> Vec<String> {
    let links: HashMap<String, Vec<String>> = templates
        .iter()
        .map(|t| {
            (
                text(t, "templateid"),
                list(t, "parentTemplates")
                    .into_iter()
                    .map(|p| text(p, "templateid"))
                    .collect(),
            )
        })
        .collect();
    let mut queue: Vec<String> = hosts
        .iter()
        .flat_map(|h| list(h, "parentTemplates"))
        .map(|p| text(p, "templateid"))
        .collect();
    let mut seen = Vec::new();
    while let Some(id) = queue.pop() {
        if seen.contains(&id) {
            continue;
        }
        queue.extend(links.get(&id).cloned().unwrap_or_default());
        seen.push(id);
    }
    seen.sort();
    seen
}

fn macro_of(raw: &Value) -> Macro {
    // Type 1 is secret text and 2 a vault path: their value is not given.
    let secret = matches!(text(raw, "type").as_str(), "1" | "2");
    let value = raw.get("value").and_then(Value::as_str);
    Macro {
        name: text(raw, "macro"),
        value: if secret {
            None
        } else {
            value.map(str::to_owned)
        },
    }
}

fn host_of(raw: &Value, groups_key: &str, macros: Vec<Macro>) -> Host {
    let interfaces = list(raw, "interfaces")
        .into_iter()
        .map(|i| Interface {
            kind: match text(i, "type").as_str() {
                "2" => InterfaceKind::Snmp,
                "3" => InterfaceKind::Ipmi,
                "4" => InterfaceKind::Jmx,
                _ => InterfaceKind::Agent,
            },
            ip: text(i, "ip"),
            dns: text(i, "dns"),
            use_ip: text(i, "useip") == "1",
        })
        .collect();
    let os = raw.get("inventory").and_then(|inventory| {
        [text(inventory, "os"), text(inventory, "os_full")]
            .into_iter()
            .find(|os| !os.trim().is_empty())
    });
    Host {
        host: text(raw, "host"),
        name: text(raw, "name"),
        enabled: text(raw, "status") == "0",
        interfaces,
        os,
        groups: list(raw, groups_key)
            .into_iter()
            .map(|g| text(g, "name"))
            .collect(),
        templates: list(raw, "parentTemplates")
            .into_iter()
            .map(|t| text(t, "host"))
            .collect(),
        macros,
        items: vec![],
        triggers: vec![],
        web: vec![],
    }
}

/// An item type number of the API.
fn item_kind(number: &str) -> ItemKind {
    match number {
        "0" | "7" => ItemKind::Agent,
        "3" => ItemKind::Simple,
        "1" | "4" | "6" | "20" => ItemKind::Snmp,
        "12" => ItemKind::Ipmi,
        "16" => ItemKind::Jmx,
        "5" => ItemKind::Internal,
        "18" => ItemKind::Dependent,
        "15" => ItemKind::Calculated,
        "19" => ItemKind::HttpAgent,
        "10" | "13" | "14" | "21" | "22" => ItemKind::Command,
        _ => ItemKind::Other,
    }
}

fn item_of(raw: &Value, prototype: bool, template: Option<String>) -> Item {
    let kind = item_kind(&text(raw, "type"));
    Item {
        name: text(raw, "name"),
        key: text(raw, "key_"),
        kind,
        delay: Some(text(raw, "delay")).filter(|d| !d.is_empty()),
        enabled: text(raw, "status") == "0",
        prototype,
        http: (kind == ItemKind::HttpAgent).then(|| Box::new(http_agent_of(raw))),
        snmp_oid: Some(text(raw, "snmp_oid")).filter(|o| !o.is_empty()),
        template,
    }
}

/// Name and value pairs: `[{"name": .., "value": ..}]` as Zabbix 7.0 and
/// web scenarios write them, `{"name": "value"}` or `[{"name": "value"}]`
/// as items did before.
fn pairs(raw: &Value, key: &str) -> Vec<(String, String)> {
    let scalar = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let mut out = Vec::new();
    match raw.get(key) {
        Some(Value::Array(items)) => {
            for item in items {
                if item.get("name").is_some() {
                    out.push((text(item, "name"), text(item, "value")));
                } else if let Value::Object(map) = item {
                    out.extend(map.iter().map(|(k, v)| (k.clone(), scalar(v))));
                }
            }
        }
        Some(Value::Object(map)) => out.extend(map.iter().map(|(k, v)| (k.clone(), scalar(v)))),
        _ => {}
    }
    out.retain(|(name, _)| !name.is_empty());
    out
}

/// Whether `key` holds anything: a non-empty text, list or object.
fn has(raw: &Value, key: &str) -> bool {
    match raw.get(key) {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        _ => false,
    }
}

/// An authentication number of the API, with its user and password keys.
fn auth_of(raw: &Value, kind: &str, user: &str, password: &str) -> WebAuth {
    let kind = match text(raw, kind).as_str() {
        "1" => "basic",
        "2" => "ntlm",
        "3" => "kerberos",
        "4" => "digest",
        _ => "",
    };
    WebAuth {
        kind: kind.into(),
        user: if kind.is_empty() {
            String::new()
        } else {
            text(raw, user)
        },
        password: !kind.is_empty() && !text(raw, password).is_empty(),
    }
}

fn http_agent_of(raw: &Value) -> HttpAgent {
    let method = match text(raw, "request_method").as_str() {
        "1" => "POST",
        "2" => "PUT",
        "3" => "HEAD",
        _ => "GET",
    };
    HttpAgent {
        request: WebRequest {
            name: String::new(),
            url: text(raw, "url"),
            query: pairs(raw, "query_fields"),
            method: method.into(),
            body: has(raw, "posts"),
            status_codes: text(raw, "status_codes"),
            required: String::new(),
            follow_redirects: text(raw, "follow_redirects") != "0",
            headers_only: text(raw, "retrieve_mode") == "1",
            headers: pairs(raw, "headers"),
            variables: false,
            timeout: Some(text(raw, "timeout")).filter(|t| !t.is_empty()),
        },
        auth: auth_of(raw, "authtype", "username", "password"),
    }
}

fn step_of(raw: &Value) -> WebRequest {
    WebRequest {
        name: text(raw, "name"),
        url: text(raw, "url"),
        query: pairs(raw, "query_fields"),
        method: "GET".into(),
        body: has(raw, "posts"),
        status_codes: text(raw, "status_codes"),
        required: text(raw, "required"),
        follow_redirects: text(raw, "follow_redirects") != "0",
        headers_only: text(raw, "retrieve_mode") == "1",
        headers: pairs(raw, "headers"),
        variables: has(raw, "variables"),
        timeout: Some(text(raw, "timeout")).filter(|t| !t.is_empty()),
    }
}

fn trigger_of(raw: &Value, prototype: bool) -> Trigger {
    Trigger {
        name: text(raw, "description"),
        expression: text(raw, "expression"),
        recovery: (text(raw, "recovery_mode") == "1")
            .then(|| text(raw, "recovery_expression"))
            .filter(|r| !r.trim().is_empty()),
        priority: Priority::parse(&text(raw, "priority")),
        enabled: text(raw, "status") == "0",
        prototype,
        template: None,
        tags: list(raw, "tags")
            .into_iter()
            .map(|t| (text(t, "tag"), text(t, "value")))
            .collect(),
        id: Some(text(raw, "triggerid")).filter(|id| !id.is_empty()),
        dependencies: list(raw, "dependencies")
            .into_iter()
            .map(|d| TriggerRef {
                id: Some(text(d, "triggerid")),
                name: text(d, "description"),
                expression: String::new(),
            })
            .filter(|d| d.id.as_deref().is_some_and(|id| !id.is_empty()))
            .collect(),
    }
}

fn web_of(raw: &Value) -> WebScenario {
    let mut steps = list(raw, "steps");
    steps.sort_by_key(|s| text(s, "no").parse::<u32>().unwrap_or(0));
    WebScenario {
        name: text(raw, "name"),
        delay: Some(text(raw, "delay")).filter(|d| !d.is_empty()),
        enabled: text(raw, "status") == "0",
        steps: steps.into_iter().map(step_of).collect(),
        headers: pairs(raw, "headers"),
        variables: has(raw, "variables"),
        auth: auth_of(raw, "authentication", "http_user", "http_password"),
    }
}

fn number<T: std::str::FromStr + Default>(raw: &Value, key: &str) -> T {
    text(raw, key).parse().unwrap_or_default()
}

fn maintenance_of(
    raw: &Value,
    groups_key: &str,
    hosts: &HashMap<String, String>,
    groups: &HashMap<String, String>,
) -> Maintenance {
    let periods = list(raw, "timeperiods")
        .into_iter()
        .filter_map(|p| {
            let kind = match text(p, "timeperiod_type").as_str() {
                "0" => PeriodKind::Once,
                "2" => PeriodKind::Daily,
                "3" => PeriodKind::Weekly,
                "4" => PeriodKind::Monthly,
                _ => return None,
            };
            Some(Period {
                kind,
                every: number(p, "every"),
                months: number(p, "month"),
                weekdays: number(p, "dayofweek"),
                day: number(p, "day"),
                start_time: number(p, "start_time"),
                length: number(p, "period"),
                start_date: number(p, "start_date"),
            })
        })
        .collect();
    Maintenance {
        name: text(raw, "name"),
        description: text(raw, "description"),
        active_since: number(raw, "active_since"),
        active_till: number(raw, "active_till"),
        hosts: list(raw, "hosts")
            .into_iter()
            .filter_map(|h| hosts.get(&text(h, "hostid")).cloned())
            .collect(),
        groups: list(raw, groups_key)
            .into_iter()
            .filter_map(|g| groups.get(&text(g, "groupid")).cloned())
            .collect(),
        periods,
        tags: list(raw, "tags")
            .into_iter()
            .map(|t| {
                let value = text(t, "value");
                if value.is_empty() {
                    text(t, "tag")
                } else {
                    format!("{}={value}", text(t, "tag"))
                }
            })
            .collect(),
        tag_filters: list(raw, "tags")
            .into_iter()
            .map(|t| TagFilter {
                tag: text(t, "tag"),
                value: text(t, "value"),
                // 0 is "contains", 2 "equals".
                contains: text(t, "operator") != "2",
            })
            .collect(),
        tags_all: text(raw, "tags_evaltype") != "2",
        collect_data: text(raw, "maintenance_type") != "1",
    }
}

fn dcheck_of(raw: &Value) -> DiscoveryCheck {
    DiscoveryCheck {
        kind: number(raw, "type"),
        ports: text(raw, "ports"),
        community: text(raw, "snmp_community"),
        security_name: text(raw, "snmpv3_securityname"),
        security_level: number(raw, "snmpv3_securitylevel"),
        auth_protocol: number(raw, "snmpv3_authprotocol"),
        auth_passphrase: text(raw, "snmpv3_authpassphrase"),
        priv_protocol: number(raw, "snmpv3_privprotocol"),
        priv_passphrase: text(raw, "snmpv3_privpassphrase"),
    }
}

/// A discovery (event source 1) or auto-registration (2) action.
fn action_of(raw: &Value) -> Option<Action> {
    let source = match text(raw, "eventsource").as_str() {
        "1" => ActionSource::Discovery,
        "2" => ActionSource::Registration,
        _ => return None,
    };
    let filter = raw.get("filter").unwrap_or(&Value::Null);
    Some(Action {
        name: text(raw, "name"),
        source,
        enabled: text(raw, "status") == "0",
        // Condition type 18 is "discovery rule".
        rules: list(filter, "conditions")
            .into_iter()
            .filter(|c| text(c, "conditiontype") == "18" && text(c, "operator") == "0")
            .map(|c| text(c, "value"))
            .collect(),
        // Add a host, add it to a group, link a template.
        adds_hosts: list(raw, "operations")
            .into_iter()
            .any(|o| matches!(text(o, "operationtype").as_str(), "2" | "4" | "6")),
    })
}

/// Tag conditions of a service or SLA: operator 0 is equals, 2 contains.
fn tag_filters(raw: &Value, key: &str) -> Vec<TagFilter> {
    list(raw, key)
        .iter()
        .map(|t| TagFilter {
            tag: text(t, "tag"),
            value: text(t, "value"),
            contains: text(t, "operator") == "2",
        })
        .collect()
}

fn service_of(raw: &Value) -> Service {
    Service {
        id: text(raw, "serviceid"),
        name: text(raw, "name"),
        algorithm: number(raw, "algorithm"),
        problem_tags: tag_filters(raw, "problem_tags"),
        children: list(raw, "children")
            .iter()
            .map(|c| text(c, "serviceid"))
            .collect(),
        status_rules: list(raw, "status_rules")
            .iter()
            .map(|r| StatusRule {
                kind: number(r, "type"),
                limit: number(r, "limit_value"),
            })
            .collect(),
        tags: list(raw, "tags")
            .iter()
            .map(|t| (text(t, "tag"), text(t, "value")))
            .collect(),
        weighted: list(raw, "children")
            .iter()
            .any(|c| number::<u32>(c, "weight") > 0),
    }
}

fn sla_of(raw: &Value) -> Sla {
    Sla {
        name: text(raw, "name"),
        enabled: text(raw, "status") != "0",
        slo: number(raw, "slo"),
        scheduled: !list(raw, "schedule").is_empty(),
        excluded: list(raw, "excluded_downtimes").len(),
        service_tags: tag_filters(raw, "service_tags"),
    }
}

fn media_type_of(raw: &Value) -> MediaType {
    let kind = match text(raw, "type").as_str() {
        "1" => MediaKind::Script,
        "2" => MediaKind::Sms,
        "4" => MediaKind::Webhook,
        _ => MediaKind::Email,
    };
    let authenticated = text(raw, "smtp_authentication") == "1";
    let password = text(raw, "passwd");
    MediaType {
        name: text(raw, "name"),
        kind,
        enabled: text(raw, "status") == "0",
        smtp_server: text(raw, "smtp_server"),
        smtp_port: text(raw, "smtp_port").parse().unwrap_or(25),
        smtp_email: text(raw, "smtp_email"),
        username: if authenticated {
            text(raw, "username")
        } else {
            String::new()
        },
        password: (authenticated && !password.is_empty()).then_some(password),
        parameters: list(raw, "parameters")
            .into_iter()
            .map(|p| (text(p, "name"), text(p, "value")))
            .collect(),
    }
}

#[cfg(test)]
mod tests;
