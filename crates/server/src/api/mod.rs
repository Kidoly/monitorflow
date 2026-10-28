//! REST API consumed by the web interface (architecture §9).
//!
//! The listener speaks plain HTTP and sits behind a TLS reverse proxy
//! (ADR-0006). Every route lives under `/api`, except the agent release
//! files under `/agent` (ADR-0054).
//!
//! # JSON conventions
//!
//! - Bodies are JSON objects; field names are `snake_case`.
//! - Timestamps are RFC 3339 strings in UTC, IDs are UUID strings.
//! - Errors use the HTTP status and a body `{"error": "<message>"}`. Messages
//!   are generic and never carry internal details.
//!
//! # Routes
//!
//! | Method | Path | Auth | Response |
//! | --- | --- | --- | --- |
//! | GET | `/api/health` | none | `{"status": "ok"}`: the process is up |
//! | GET | `/api/health/ready` | none | `{"status": "ready"}`, or 503 `{"status": "unavailable"}` when PostgreSQL is unreachable |
//! | POST | `/api/auth/login` | none | body `{"username", "password"}`; 200 [`Me`] and the session cookie, or 401 |
//! | POST | `/api/auth/logout` | session | 204, clears the cookie |
//! | GET | `/api/setup` | none | [`setup::Status`]: `{"pending"}`, true while no user exists (ADR-0057) |
//! | POST | `/api/setup` | setup link | body `{"token", "username", "password"}`; 200 [`Me`] and the session cookie; 400 for a bad username or password, 410 for a used, replaced or expired link, 409 once a user exists |
//! | GET | `/api/auth/me` | session | [`Me`]: `{"id", "username", "roles": [{"role", "customer_id", "site_id"}]}` |
//! | GET | `/api/servers` | any role | [`ServerList`], see below |
//! | GET | `/api/servers/{id}` | read | [`ServerDetail`]: one listing row plus `certificate`, see below |
//! | GET | `/api/servers/{id}/sensors` | read | [`sensors::Effective`]: effective sensors, origins and drift |
//! | GET | `/api/servers/{id}/sensor_configs` | read | [`sensors::EntryList`] of the server's own rows |
//! | POST | `/api/servers/{id}/sensor_configs` | modify | body [`sensors::EntryRequest`]; 201 [`sensors::Entry`] |
//! | GET | `/api/sites/{id}/sensors` | read | [`sensors::SiteEffective`]: built-in defaults overlaid with the site's rows |
//! | GET | `/api/sites/{id}/sensor_configs` | read | [`sensors::EntryList`] of the site's rows |
//! | POST | `/api/sites/{id}/sensor_configs` | modify | body [`sensors::EntryRequest`]; 201 [`sensors::Entry`] |
//! | PUT | `/api/sensor_configs/{id}` | modify | body [`sensors::EntryRequest`], replaces the row; 200 [`sensors::Entry`] |
//! | DELETE | `/api/sensor_configs/{id}` | modify | 204 |
//! | GET | `/api/sites` | any role | [`sites::SiteList`]: `{"sites": [{"id", "name", "server_count", "relay_count", "customer_id", "customer_name"}]}`, readable sites by name |
//! | GET | `/api/relays` | any role | [`relays::RelayList`], paginated like servers, see below |
//! | GET | `/api/relays/{id}` | read | [`relays::RelayDetail`]: a listing row plus `certificate` and attached `servers` |
//! | POST | `/api/servers/{id}/revoke`, `/api/relays/{id}/revoke` | customer administrator | body `{"reason"?}`; 200 [`revoke::Revoked`], 409 if already revoked |
//! | GET | `/api/tokens` | customer administrator | [`tokens::TokenList`]: enrollment tokens without secrets, see below |
//! | POST | `/api/tokens` | customer administrator | body [`tokens::CreateRequest`]; 201 [`tokens::CreatedToken`], the only time the token is returned |
//! | DELETE | `/api/tokens/{id}` | customer administrator | 204; 409 once used |
//! | GET | `/api/customers` | any role | [`customers::CustomerList`]: `{"customers": [{"id", "name", "site_count"}]}`, see below |
//! | POST | `/api/customers` | global administrator | body `{"name"}`; 201 [`customers::CustomerView`] |
//! | POST | `/api/customers/{id}/sites` | customer administrator | body `{"name"}`; 201 [`sites::SiteView`] |
//! | PUT | `/api/sites/{id}/agent-auto-update` | customer administrator | body `{"enabled": true, false or null}`; [`sites::SiteView`]; audited (ADR-0055) |
//! | POST | `/api/relays/{id}/update` | customer administrator | body [`relays::UpdateRequest`]; 202 with the relay command; 409 while one is in flight; audited (ADR-0055) |
//! | GET | `/api/auto-update` | global administrator | [`auto_update::StatusView`]: settings and rollout progress (ADR-0055) |
//! | GET | `/api/settings/auto-update` | global administrator | [`policy::Settings`](crate::auto_update::policy::Settings) as they apply (ADR-0055) |
//! | PUT | `/api/settings/auto-update` | global administrator | body [`auto_update::SettingsRequest`]; the settings; 409 against the configuration; audited |
//! | GET | `/api/settings/public-address` | customer administrator | [`public_address::PublicAddress`]: the address in install commands, the default, the certificate's names and whether it covers the address |
//! | PUT | `/api/settings/public-address` | global administrator | body [`public_address::SetRequest`]; 200 [`public_address::PublicAddress`]; audited |
//! | POST | `/api/auto-update/rollouts/{kind}/{version}/resume` | global administrator | resumes a halted rollout; audited |
//! | GET | `/api/mirrors` | global administrator | [`mirrors::MirrorList`], see below |
//! | GET | `/api/mirrors/{id}` | global administrator | [`mirrors::MirrorDetail`]: a listing item plus `snapshots` and the last `runs` |
//! | POST | `/api/mirrors/{id}/sync` | global administrator | 202 [`mirrors::RunSummary`] of the started sync; 409 while busy |
//! | POST | `/api/mirrors/{id}/snapshots` | global administrator | body `{"name"?}`; 202 [`mirrors::RunSummary`]; 409 while busy or never synced |
//! | GET | `/api/incidents` | any role | [`incidents::IncidentList`], see below |
//! | GET | `/api/incidents/{id}` | read | [`incidents::IncidentDetail`]: the incident and its `events` |
//! | GET | `/api/remediation/incidents` | any role | [`remediation_incidents::IncidentList`], see below |
//! | GET | `/api/remediation/incidents/{id}` | read | [`remediation_incidents::IncidentDetail`]: the incident, its `events` and `chain` |
//! | GET | `/api/remediation/approvals` | any role | [`approvals::PendingList`], see below |
//! | POST | `/api/remediation/approvals/{id}/approve` | approver, see below | body `{"command"?}`; 200 [`approvals::DecisionView`]; 409 once decided or past the deadline |
//! | POST | `/api/remediation/approvals/{id}/reject` | approver | body `{"reason"}`; 200 [`approvals::DecisionView`]; 409 as for approve |
//! | GET | `/api/remediation/events` | any role | [`remediation_events::EventList`], see below |
//! | GET | `/api/servers/{id}/thresholds/effective` | read | [`thresholds::Effective`]: thresholds in force, muted ones and origins |
//! | GET | `/api/servers/{id}/thresholds` | read | [`thresholds::EntryList`] of the server's own rows |
//! | POST | `/api/servers/{id}/thresholds` | modify | body [`thresholds::EntryRequest`]; 201 [`thresholds::Entry`] |
//! | GET | `/api/sites/{id}/thresholds/effective` | read | [`thresholds::SiteEffective`]: built-in defaults overlaid with the site's rows |
//! | GET | `/api/sites/{id}/thresholds` | read | [`thresholds::EntryList`] of the site's rows |
//! | POST | `/api/sites/{id}/thresholds` | modify | body [`thresholds::EntryRequest`]; 201 [`thresholds::Entry`] |
//! | PUT | `/api/thresholds/{id}` | modify | body [`thresholds::EntryRequest`], replaces the row; 200 [`thresholds::Entry`] |
//! | DELETE | `/api/thresholds/{id}` | modify | 204 |
//! | GET | `/api/servers/{id}/metrics` | read | [`metrics::MetricList`]: metric names and instances of the last day |
//! | GET | `/api/metrics/query` | read | [`metrics::QueryResult`], see below |
//! | GET | `/api/metrics/query_range` | read | [`metrics::QueryResult`], see below |
//! | GET | `/api/servers/{id}/packages` | read | [`packages::PackageList`], see below |
//! | GET | `/api/servers/{id}/commands` | read | [`commands::CommandList`], newest first, paginated |
//! | POST | `/api/servers/{id}/commands` | customer administrator | body [`commands::CreateRequest`]; 201 [`commands::CommandDetail`] |
//! | GET | `/api/commands/{id}` | read | [`commands::CommandDetail`]: the command and its output |
//! | GET | `/agent/{version}/{file}`, `/agent/latest/{file}` | none | A file of a cached agent release, for installs (ADR-0054); 404 with a plain-text reason |
//! | GET | `/api/settings/agent-install` | customer administrator | [`settings::AgentInstall`]: default install source, custom URL, what the configuration sets, the release `central` installs (ADR-0054) |
//! | PUT | `/api/settings/agent-install` | global administrator | body [`settings::AgentInstallRequest`]; 200 [`settings::AgentInstall`]; 409 against the configuration; audited |
//! | GET | `/api/agent-releases` | any role | [`commands::AgentReleases`]: cached agent versions and the latest (ADR-0053) |
//! | GET | `/api/server-update` | global administrator | [`server_update::UpdateView`]: running version, updater and last update (ADR-0056) |
//! | POST | `/api/server-update/check` | global administrator | [`server_update::CheckResult`]: the latest release, verified, and what it changes |
//! | POST | `/api/server-update` | global administrator | body `{"version"}`; 202 `{"id"}`; 400 unless newer, 409 without a running updater or while busy; audited |
//! | GET | `/api/campaigns` | customer role | [`campaigns::CampaignList`], newest first, paginated |
//! | POST | `/api/campaigns` | customer administrator | body [`campaigns::CreateRequest`]; 201 [`campaigns::CampaignDetail`] |
//! | GET | `/api/campaigns/{id}` | customer role | [`campaigns::CampaignDetail`]: the campaign, its waves and servers |
//! | POST | `/api/campaigns/{id}/start`, `/pause`, `/resume`, `/stop` | customer administrator | 200 [`campaigns::CampaignDetail`]; 409 when the state does not allow it |
//! | GET | `/api/remediation/rules` | customer role | [`remediation::RuleList`], see below |
//! | POST | `/api/remediation/rules` | customer administrator | body [`remediation::RuleRequest`]; 201 the stored rule |
//! | DELETE | `/api/remediation/rules/{id}` | customer administrator | 204 |
//! | GET | `/api/remediation/policies` | customer role | [`remediation::PolicyList`] |
//! | PUT | `/api/remediation/policies` | customer administrator | body [`remediation::PolicyRequest`]; 200 the stored policy |
//! | POST | `/api/remediation/classify` | customer role | body `{"customer_id", "site_id"?, "command"}`; 200 the decision |
//! | GET | `/api/servers/{id}/proxmox` | customer administrator | [`proxmox::ProxmoxView`], see below |
//! | PUT | `/api/servers/{id}/proxmox` | customer administrator | body [`proxmox::MappingRequest`]; 200 [`proxmox::ProxmoxView`] |
//! | DELETE | `/api/servers/{id}/proxmox` | customer administrator | 204; 404 without a mapping |
//!
//! # Authentication
//!
//! Login sets a session cookie (`__Host-mf_session`, or `mf_session` when
//! `api_cookie_secure` is off) that is `HttpOnly` and `SameSite=Strict`.
//! Sessions live server-side (see [`session`]) and end on logout, after
//! `api_session_lifetime`, or after `api_session_idle_timeout` without a
//! request. A missing or dead session gives 401 `authentication required`.
//!
//! Every request other than GET, HEAD, OPTIONS and TRACE, login included,
//! must carry the header `X-MonitorFlow-CSRF: 1` and must not be marked
//! cross-site by `Sec-Fetch-Site`; otherwise 403.
//!
//! # Authorization
//!
//! Every role (viewer, operator, approver, administrator) can read the sites
//! it is granted on. A grant is global (`customer_id` and `site_id` null),
//! on one customer and every site it has, or on one site (ADR-0026). A user
//! without any role gets 403 on data routes. Grants are resolved to sites in
//! [`access`] only; handlers never inspect them.
//!
//! "read" routes need a role covering the site of the resource, "modify"
//! routes an operator role or above on it. A site or server outside the
//! user's sites is 404, as if it did not exist; one the user reads but may
//! not change is 403.
//!
//! # Customers
//!
//! Every site belongs to one customer; single-tenant installs only have
//! `default` (ADR-0026). `GET /api/customers` lists the customers of a
//! global grant, of customer grants and of readable sites; `site_count`
//! counts only the sites the user reads. Creating a customer needs a global
//! administrator role, creating a site under one an administrator role on
//! it (global or customer grant): a customer the user does not see is 404,
//! one it sees without that right 403. Names are 1 to 64 characters
//! without control characters or surrounding spaces; site names are unique
//! across customers. Duplicates are 409. Both are audited
//! (`customer.create`, `site.create`). Sites never move between customers
//! through the API.
//!
//! # Relays and enrollment tokens
//!
//! `GET /api/relays` lists the relays of readable sites by site name and
//! peer ID, with `limit` and `cursor` as for servers. Each item has `id`,
//! `peer_id`, `name`, site and customer, `created_at`, `revoked_at`, the
//! live `status`, `connected_at` and `last_heartbeat` of the relay's own
//! stream, `cert_not_after`, the `server_names` of its certificate,
//! `server_count` (servers whose latest connection came through it) and
//! `connected_agents` (streams it forwards now). The detail adds
//! `certificate` as for servers and `servers`: `[{"id", "peer_id",
//! "hostname", "status"}]`.
//!
//! Enrollment tokens (ADR-0007) need an administrator role on the site's
//! customer, global or by customer grant; a site grant is not enough
//! (ADR-0029). `POST /api/tokens` takes `{"role": "agent" | "relay",
//! "peer_id", "site_id", "ttl_s"?, "server_names"?}` (TTL 60 s to 7 days,
//! default one hour; names for relays only) and answers the stored token
//! plus `token`, `upstream_url` and `enroll_url` (at the public address),
//! `install` and `address`. The token string is never returned again. `GET /api/tokens` takes optional `state` (`pending`,
//! `used`, `expired`), `limit` (1 to 500, default 100) and `cursor`, and
//! lists newest first with `{"id", "role", "peer_id", "site_id",
//! "site_name", "customer_id", "customer_name", "server_names",
//! "created_by", "created_by_username", "created_at", "expires_at",
//! "used_at", "state"}`. `DELETE` removes an unused token, so enrollment
//! refuses it; the audit log keeps `enrollment_token.create` and
//! `enrollment_token.revoke`.
//!
//! Revoking a server or relay (ADR-0029) sets its `revoked_at`, revokes
//! every unexpired certificate of the peer with `reason` (`unspecified`,
//! `key_compromise`, `affiliation_changed`, `superseded` or
//! `cessation_of_operation`; default `unspecified`), publishes a new CRL
//! and closes its live streams, and a relay's forwarded ones, with
//! `PERMISSION_DENIED`. The answer is `{"revoked_at", "reason", "serials",
//! "closed_streams"}`. It is audited as `server.revoke` or `relay.revoke`
//! and cannot be undone.
//!
//! # Mirrors
//!
//! Package mirrors are global (ADR-0030), so every mirror route needs a
//! global administrator role; anyone else gets 403. A listing item is the
//! mirror (`id`, `name`, `family`, `upstream_url`, `distribution`,
//! `components`, `architectures`, `sync_interval_s`, `auto_snapshot`,
//! `created_at`) plus `current_snapshot` (the newest published one's name,
//! served at `/<family>/<name>/current/` on the mirror listener) and
//! `last_run`. Snapshots have `id`, `name`, `state` (`creating`,
//! `published`, `failed`), `size_bytes`, `error`, `created_at` and
//! `published_at`; runs `id`, `kind` (`sync`, `snapshot`), `trigger`
//! (`schedule`, `manual`), `requested_by`, `state` (`running`,
//! `succeeded`, `failed`), `snapshot_id`, `error`, `started_at`,
//! `finished_at`, and in the detail `output`, the last 64 KiB of the
//! commands and what they printed. Starting a job answers at once with
//! the running run; poll the detail for its end. Requests are audited
//! (`mirror.sync`, `mirror.snapshot`). Mirrors are created with the CLI.
//!
//! # Sensor configuration
//!
//! Site and server rows of `sensor_configs` (ADR-0016). A body is
//! `{"kind", "name"?, "instance"?, "interval_ms", "timeout_ms"?, "params"?,
//! "enabled"?}`: `name` defaults to the kind, `params` is a flat object of
//! strings, numbers and booleans. Rows are checked with the agent's rules
//! and sensor registry; an invalid one is 400 with `{"error"}` saying why,
//! a second row with the same name and instance for one owner 409. Every
//! change is audited (`sensor_config.create`, `.update`, `.delete`,
//! parameter names only) and bumps the revision, so agents get it at the
//! next poll.
//!
//! # Thresholds
//!
//! Site and server rows of `thresholds` (ADR-0017). A body is `{"metric",
//! "instance"?, "comparison", "warning"?, "critical"?, "duration_s"?,
//! "hysteresis"?, "enabled"?}`: `metric` is `monitorflow_` and a sensor
//! metric name, a null or absent `instance` covers every instance,
//! `comparison` is `>`, `>=`, `<` or `<=`. An enabled row needs a warning
//! or a critical value, critical on the alarm side of warning; `duration_s`
//! is 0 to 86400 and `hysteresis` 0 or more. A disabled row mutes the same
//! metric and instance of the layers below, built-in defaults included.
//! Invalid rows are 400 saying why, a second row with the same metric and
//! instance for one owner 409. Changes are audited (`threshold.create`,
//! `.update`, `.delete`) and bump the revision, so the engine reloads them
//! at its next poll. The API never writes incidents or alert states.
//!
//! # Incidents
//!
//! Read-only: the threshold engine owns incidents and their events
//! (ADR-0019). `GET /api/incidents` takes optional `state` (`open`,
//! `resolved`), `severity` (`warning`, `critical`), `server_id`,
//! `site_id`, `from` and `to` (RFC 3339; incidents active at some point of
//! the range), `limit` (1 to 500, default 100) and `cursor`. Open incidents
//! come first, then newest `opened_at` first; `next_cursor` works as for
//! servers. Unknown parameters and bad values are 400. A server or site
//! outside the user's scope gives an empty list, an incident outside it 404.
//!
//! # Remediation incidents
//!
//! Read-only: the AI remediation loop owns them (ADR-0042).
//! `GET /api/remediation/incidents` takes optional `state` (`open`,
//! `running`, `awaiting_approval`, `resolved`, `escalated`, `failed`),
//! `customer_id`, `site_id`, `server_id`, `alert_incident_id`, `limit` (1
//! to 500, default 100) and `cursor`, newest `opened_at` first. An item is
//! `{"id", "customer_id", "customer_name", "site_id", "site_name",
//! "server_id", "hostname", "alert_incident_id", "metric", "instance",
//! "state", "opened_at", "closed_at", "max_commands", "max_duration_s",
//! "event_count"}`. The detail adds `events`: `[{"seq", "kind",
//! "actor_kind", "actor_id", "payload", "created_at", "prev_hash",
//! "hash"}]` by `seq`, hashes in hex, and `chain`: `{"valid", "events",
//! "broken_at", "reason", "head_hash"}`, recomputed on every request.
//! Scoping is as for incidents.
//!
//! # Approval queue
//!
//! Commands the risk rules parked for a human (ADR-0043, ADR-0045).
//! `GET /api/remediation/approvals` takes optional `customer_id`,
//! `site_id` and `server_id` and lists up to 500 pending commands of
//! readable sites, nearest `deadline` first. An item is `{"id",
//! "incident_id", "customer_id", "customer_name", "site_id", "site_name",
//! "server_id", "hostname", "alert_incident_id", "metric", "instance",
//! "command", "diagnosis", "reasoning", "tier", "rule", "reason",
//! "commands_run", "max_commands", "opened_at", "proposed_at",
//! "deadline", "can_approve", "can_reject", "snapshots": {"available",
//! "reason"}}`; `diagnosis` and `reasoning` are the AI's untrusted text.
//! Approving a `low_impact` command needs an approver or administrator
//! role covering the server's site, a `high_impact` one an administrator
//! role; rejecting needs an approver role or above whatever the tier.
//! `approve` takes `{}` or `{"command"}` to run an edited command, which
//! the rules classify again: forbidden is 400, and the role needed is
//! that of the higher tier of the two. `reject` takes `{"reason"}`, 1 to
//! 1000 characters, which the AI gets back. A decision appends `edit`
//! and `approval`, or `rejection`, and a `state_change` to the incident's
//! chain, and the loop resumes on its own. It is 409 when the command was
//! already decided, the incident no longer waits for approval, or its
//! deadline passed. Decisions are audited (`remediation.approve`,
//! `remediation.reject`). A pending command outside the user's sites is
//! 404, one it reads without the role 403.
//!
//! `GET /api/remediation/events` is the remediation audit log: the events
//! of the incidents of readable sites, newest first, filtered by optional
//! `customer_id`, `site_id`, `server_id`, `incident_id`, `kind`,
//! `actor_kind` (`user`, `system`, `ai`, `peer`), `actor_id`, `from` and
//! `to` (RFC 3339, `from` included), with `limit` (1 to 500, default 100)
//! and `cursor`. An item is an incident event (`incident_id`, `seq`,
//! `kind`, `actor_kind`, `actor_id`, `actor_name`, `payload`,
//! `created_at`, `hash`) with its incident's customer, site, server,
//! `metric` and `instance`. `payload` leaves out `context.rendered` and
//! command `stdout`/`stderr`; the incident detail has them and verifies
//! the chain.
//!
//! # Commands and packages
//!
//! Commands for agents (ADR-0031). `POST /api/servers/{id}/commands` takes
//! `{"kind": "apply_updates", "packages"?, "security_only"?,
//! "allow_reboot"?, "ttl_s"?}`: `packages` empty or absent upgrades
//! everything, names are letters, digits and `.+_:-` not starting with
//! `-`, and for a Windows server KB numbers (`KB5034441`) or update IDs
//! (ADR-0049); `ttl_s` (60 s to 7 days, default one hour) is how long the agent
//! may still start it. It needs an administrator role on the server's
//! customer, as revocation does (ADR-0029); a revoked server is 409. The
//! command is queued, sent as soon as the agent is connected and has no
//! other command in flight, and audited as `command.create`. A command is
//! `{"id", "server_id", "kind", "parameters", "state", "issued_at",
//! "expires_at", "sent_at", "started_at", "finished_at", "exit_code",
//! "error", "requested_by", "campaign_id"}`; `state` is `queued`, `sent`,
//! `running`, `succeeded`, `failed`, `expired` or `cancelled`. The detail
//! adds `stdout` and `stderr`, the last 64 KiB of each. Listings take
//! `limit` (1 to 500, default 100) and `cursor`.
//!
//! `GET /api/servers/{id}/packages` returns `{"reboot_required",
//! "reported_at", "packages": [{"name", "arch", "version",
//! "upgradable_version", "title", "classification", "severity",
//! "size_bytes", "reboot_required"}], "next_cursor"}` from the latest
//! report, by name and architecture; the four fields after
//! `upgradable_version` are `null` and `reboot_required` false except for
//! Windows updates (ADR-0049); `upgradable=true` keeps packages with an upgrade,
//! `limit` is 1 to 5000 (default 500). `reboot_required` and
//! `reported_at` are `null` until the agent reported.
//!
//! # Campaigns
//!
//! Update campaigns (ADR-0033, which documents the JSON). A campaign
//! belongs to one customer; reading it needs a role on that customer as a
//! whole (global or customer grant), creating and acting an administrator
//! role on it. Another customer's campaign is 404, one the user reads
//! without administering it 403. Targets are `server_ids` and/or the
//! unrevoked servers of `site_id`, all of the customer (others are 400
//! `unknown server`), frozen into `waves` at creation. Listings take
//! `customer_id`, `state`, `limit` (1 to 500, default 100) and `cursor`.
//! Actions answer 409 `cannot <action> a campaign that is <state>` when
//! the state does not allow them. Everything is audited (`campaign.*`).
//!
//! # Remediation rules
//!
//! Administrator rules of the risk classifier (ADR-0041), per customer
//! and optionally per site, with the same access rules as campaigns. A
//! rule is `{"id", "customer_id", "site_id", "kind", "pattern", "tier",
//! "description", "created_by", "created_at"}`: `kind` `allow` (commands
//! no built-in rule knows that match `pattern` as a whole get `tier`,
//! `read_only` or `low_impact`), `deny` (commands containing a match
//! anywhere are forbidden) or `cache_dir` (`pattern` is a directory that
//! `find <dir> -mindepth 1 -delete` may clear as a low-impact change).
//! Patterns are words where `*` and `?` glob within a word and `**`
//! spans words. Invalid rules are 400 saying why, a site of another
//! customer 400 `unknown site`, duplicates 409. Listings take
//! `customer_id` and `site_id`. A policy is `{"customer_id", "site_id",
//! "low_impact_auto", "updated_by", "updated_at"}`; a site row overrides
//! the customer row, and without either low-impact commands need
//! approval. `POST /api/remediation/classify` answers `{"tier", "rule":
//! {"source": "builtin", "id"} | {"source": "configured", "id"},
//! "reason"}` without storing the command. Changes are audited
//! (`remediation_rule.create`, `.delete`, `remediation_policy.update`).
//!
//! # Proxmox VM mappings
//!
//! The Proxmox VE VM of a server, snapshotted before change-tier
//! commands (ADR-0044), with the same access rules as revocation. The
//! view is `{"server_id", "mapping": null | {"cluster", "node", "vmid",
//! "updated_by", "updated_at"}, "snapshots": {"available", "reason"}}`;
//! `available` needs `[proxmox]` enabled, a mapping and its cluster
//! configured, else `reason` is `disabled`, `not_mapped` or
//! `unknown_cluster` and rollback is limited. `PUT` takes `{"cluster",
//! "node", "vmid"}`: a configured cluster name, the node's host name and
//! a VM ID from 100 to 999999999, else 400; a VM already mapped to
//! another server is 409. Changes are audited (`server.proxmox.set` with
//! the `previous` mapping, `server.proxmox.clear`).
//!
//! # Metric queries
//!
//! The query endpoints take `server_id`, `metric` (`monitorflow_` and a
//! sensor metric name), optional `instance` and `agg` (`avg`, `min`,
//! `max`, `sum`, `count`), then `time` for an instant query or `start`,
//! `end` (RFC 3339, default the last hour), `step` (seconds) and optional
//! `rollup` (`avg`, `max`, `min`, `last`) for a range. With `rollup`, each
//! point summarizes the samples of its series over the step before it
//! (`max_over_time(…[<step>s])`), before `agg`, so long ranges keep
//! spikes; without it, a point is the last sample before it. The API
//! builds the PromQL itself, scoped to that server's `peer`
//! and `site` (ADR-0018); arbitrary PromQL is not accepted. Ranges are at
//! most 31 days and 11,000 points per series, answers at most 500 series;
//! beyond that, 400. A server outside the user's sites is 404. Backend
//! failures are 502 `metrics backend unavailable`, timeouts 504, queries
//! the backend refuses 400.
//!
//! ```json
//! {"query": "monitorflow_cpu_usage_ratio{peer=\"agent-1\",site=\"paris\"}",
//!  "series": [{"labels": {"__name__": "...", "instance": "total"},
//!              "points": [[1760000000, 0.25]]}]}
//! ```
//!
//! # `GET /api/servers`
//!
//! ```json
//! {"servers": [{
//!   "id": "uuid", "peer_id": "agent-1", "hostname": "web-1",
//!   "site_id": "uuid", "site_name": "paris",
//!   "customer_id": "uuid", "customer_name": "default", "relay_id": null,
//!   "os_family": "debian", "os_version": "12", "agent_version": "0.1.0",
//!   "created_at": "2026-10-18T12:00:00Z", "revoked_at": null,
//!   "status": "online",
//!   "connected_at": "2026-10-18T12:00:00Z",
//!   "last_heartbeat": "2026-10-18T12:05:00Z",
//!   "via_relay": null,
//!   "open_incidents": 0,
//!   "cert_not_after": "2027-01-16T12:00:00Z"
//! }], "next_cursor": null}
//! ```
//!
//! Pages hold `limit` servers (1 to 1000, default 500); `next_cursor` is
//! `null` on the last page, else the `cursor` to pass for the next one. A
//! bad `limit` or `cursor` is 400.
//!
//! `status` is `online` while the agent has an open stream, else `offline`.
//! Servers come from the database only; a connected agent without a row is
//! not listed. See [`ServerView`] for each field. `cert_not_after` is the
//! latest expiry among the agent's unrevoked certificates, `null` if none.
//!
//! # `GET /api/servers/{id}`
//!
//! The same fields as a listing row, plus the certificate the CA issued
//! last to the agent (revoked or not), or `null` if it never issued one:
//!
//! ```json
//! {"id": "uuid", "peer_id": "agent-1", "...": "as in the listing",
//!  "certificate": {"serial": "3f2a…", "not_before": "2026-10-18T12:00:00Z",
//!                  "not_after": "2027-01-16T12:00:00Z",
//!                  "issued_at": "2026-10-18T12:00:00Z", "revoked": false,
//!                  "revoked_at": null, "revocation_reason": null}}
//! ```
//!
//! A server outside the user's sites, or an ID that is not a UUID, is 404.

pub mod access;
pub mod agent_files;
pub mod approvals;
pub(crate) mod audit;
pub mod auth;
pub mod auto_update;
pub mod campaigns;
pub mod commands;
pub mod customers;
mod error;
mod health;
pub mod incidents;
pub mod metrics;
pub mod mirrors;
pub mod packages;
pub mod password;
pub mod proxmox;
pub mod public_address;
pub mod relays;
pub mod remediation;
pub mod remediation_events;
pub mod remediation_incidents;
pub mod revoke;
pub mod roles;
pub mod sensors;
pub mod server_update;
pub mod servers;
pub mod session;
pub mod settings;
pub mod setup;
pub mod sites;
pub mod sources;
pub mod thresholds;
pub mod tokens;
pub mod users;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{delete, get, post, put};
use axum::Router;
use monitorflow_common::config::ServerConfig;
use sqlx::PgPool;
use tokio::net::TcpListener;

use crate::agent_releases;
use crate::campaign::Runner;
use crate::commands::Commands;
use crate::crl::CrlPublisher;
use crate::grpc::Registry;
use crate::mirror::Mirrors;
use crate::relay_commands::RelayCommands;
use crate::vm_query::VmQuery;

pub use auth::{CurrentUser, Me};
pub use error::ApiError;
pub use roles::{Role, RoleGrant};
pub use servers::{CertificateView, ServerDetail, ServerList, ServerView};
pub use tokens::EnrollmentSettings;

/// Largest accepted request body.
const BODY_LIMIT: usize = 64 * 1024;

/// Session and cookie settings, from [`ServerConfig`].
#[derive(Debug, Clone)]
pub struct ApiSettings {
    pub cookie_secure: bool,
    pub session_lifetime: Duration,
    pub session_idle_timeout: Duration,
    /// Base URL of the mirrors in agents' sources (ADR-0035); `None`
    /// refuses source configuration.
    pub mirror_public_url: Option<String>,
    /// Whether `[proxmox]` is on, and its cluster names (ADR-0044).
    pub proxmox_enabled: bool,
    pub proxmox_clusters: Vec<String>,
    /// Where agents download releases from, and the server's cache of
    /// them (ADR-0053).
    pub agent_releases: agent_releases::Sources,
    /// Server updates through the updater container (ADR-0056).
    pub server_update: server_update::ServerUpdateSettings,
    /// What the configuration sets about install sources (ADR-0054).
    pub agent_install: settings::InstallConfig,
    /// `auto_update.enabled` from the configuration, which locks the
    /// setting in the web interface (ADR-0055).
    pub auto_update_enabled: Option<bool>,
}

impl ApiSettings {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            cookie_secure: config.api_cookie_secure,
            session_lifetime: Duration::from_secs(config.api_session_lifetime),
            session_idle_timeout: Duration::from_secs(config.api_session_idle_timeout),
            mirror_public_url: config.mirror_public_url.clone(),
            agent_releases: agent_releases::Sources::from_config(config),
            server_update: server_update::ServerUpdateSettings::from_config(config),
            agent_install: settings::InstallConfig::from_config(config),
            auto_update_enabled: config.auto_update.enabled,
            proxmox_enabled: config.proxmox.enabled,
            proxmox_clusters: config
                .proxmox
                .clusters
                .iter()
                .map(|c| c.name.clone())
                .collect(),
        }
    }
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self::from_config(&ServerConfig::default())
    }
}

/// Shared by every handler; clones are cheap.
#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
    /// Live view of connected agents, shared with the gRPC service.
    pub registry: Registry,
    pub settings: Arc<ApiSettings>,
    /// Read client for the metric query proxy; `None` answers 502.
    pub metrics: Option<VmQuery>,
    /// CA pin and URLs for enrollment tokens; `None` refuses to create them.
    pub enrollment: Option<Arc<EnrollmentSettings>>,
    /// Revokes certificates and republishes the CRL; `None` refuses
    /// revocations.
    pub crl: Option<Arc<CrlPublisher>>,
    /// Sends newly queued commands at once; `None` leaves them to the
    /// dispatcher's next poll.
    pub commands: Option<Commands>,
    /// Sends relays their self-updates at once; `None` leaves them to the
    /// sender's next poll.
    pub relay_commands: Option<RelayCommands>,
    /// Runs mirror jobs; `None` refuses to start them.
    pub mirrors: Option<Mirrors>,
    /// Woken when a campaign starts or resumes; `None` leaves it to the
    /// runner's next poll.
    pub campaigns: Option<Runner>,
}

impl ApiState {
    pub fn new(pool: PgPool, registry: Registry, settings: ApiSettings) -> Self {
        Self {
            pool,
            registry,
            settings: Arc::new(settings),
            metrics: None,
            enrollment: None,
            crl: None,
            commands: None,
            relay_commands: None,
            mirrors: None,
            campaigns: None,
        }
    }

    pub fn with_relay_commands(mut self, commands: RelayCommands) -> Self {
        self.relay_commands = Some(commands);
        self
    }

    /// Wakes `runner` when a campaign starts or resumes.
    pub fn with_campaigns(mut self, runner: Runner) -> Self {
        self.campaigns = Some(runner);
        self
    }

    /// Wakes `commands` when a command is queued.
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Starts mirror syncs and snapshots with `mirrors`.
    pub fn with_mirrors(mut self, mirrors: Mirrors) -> Self {
        self.mirrors = Some(mirrors);
        self
    }

    /// Revokes peers' certificates through `crl`.
    pub fn with_crl(mut self, crl: Arc<CrlPublisher>) -> Self {
        self.crl = Some(crl);
        self
    }

    /// Creates enrollment tokens pinned to `settings.ca_fingerprint`.
    pub fn with_enrollment(mut self, settings: EnrollmentSettings) -> Self {
        self.enrollment = Some(Arc::new(settings));
        self
    }

    /// Serves metric queries from `metrics`.
    pub fn with_metrics(mut self, metrics: VmQuery) -> Self {
        self.metrics = Some(metrics);
        self
    }
}

/// Every API route, ready to serve.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health::live))
        .route("/api/health/ready", get(health::ready))
        .route("/agent/{version}/{file}", get(agent_files::file))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/me", get(auth::me))
        .route("/api/setup", get(setup::status).post(setup::complete))
        .route("/api/servers", get(servers::list))
        .route("/api/servers/{id}", get(servers::detail))
        .route("/api/sites", get(sites::list))
        .route(
            "/api/sites/{id}/agent-auto-update",
            put(sites::set_agent_auto_update),
        )
        .route("/api/auto-update", get(auto_update::status))
        .route(
            "/api/settings/auto-update",
            get(auto_update::get_settings).put(auto_update::set_settings),
        )
        .route(
            "/api/auto-update/rollouts/{kind}/{version}/resume",
            post(auto_update::resume),
        )
        .route("/api/relays", get(relays::list))
        .route("/api/relays/{id}", get(relays::detail))
        .route("/api/relays/{id}/revoke", post(revoke::relay))
        .route("/api/relays/{id}/update", post(relays::update))
        .route("/api/servers/{id}/revoke", post(revoke::server))
        .route("/api/tokens", get(tokens::list).post(tokens::create))
        .route("/api/tokens/{id}", delete(tokens::delete))
        .route(
            "/api/customers",
            get(customers::list).post(customers::create),
        )
        .route(
            "/api/customers/{id}/sites",
            post(customers::create_customer_site),
        )
        .route("/api/mirrors", get(mirrors::list))
        .route("/api/mirrors/{id}", get(mirrors::detail))
        .route("/api/mirrors/{id}/sync", post(mirrors::sync))
        .route("/api/mirrors/{id}/snapshots", post(mirrors::snapshot))
        .route("/api/incidents", get(incidents::list))
        .route("/api/incidents/{id}", get(incidents::detail))
        .route(
            "/api/remediation/incidents",
            get(remediation_incidents::list),
        )
        .route(
            "/api/remediation/incidents/{id}",
            get(remediation_incidents::detail),
        )
        .route("/api/remediation/approvals", get(approvals::list))
        .route(
            "/api/remediation/approvals/{id}/approve",
            post(approvals::approve),
        )
        .route(
            "/api/remediation/approvals/{id}/reject",
            post(approvals::reject),
        )
        .route("/api/remediation/events", get(remediation_events::list))
        .route("/api/servers/{id}/sensors", get(sensors::effective))
        .route("/api/sites/{id}/sensors", get(sensors::effective_site))
        .route("/api/servers/{id}/metrics", get(metrics::discover))
        .route("/api/metrics/query", get(metrics::query))
        .route("/api/metrics/query_range", get(metrics::query_range))
        .route(
            "/api/servers/{id}/sensor_configs",
            get(sensors::list_server).post(sensors::create_server),
        )
        .route(
            "/api/sites/{id}/sensor_configs",
            get(sensors::list_site).post(sensors::create_site),
        )
        .route(
            "/api/sensor_configs/{id}",
            put(sensors::update).delete(sensors::delete),
        )
        .route(
            "/api/servers/{id}/thresholds/effective",
            get(thresholds::effective),
        )
        .route(
            "/api/sites/{id}/thresholds/effective",
            get(thresholds::effective_site),
        )
        .route(
            "/api/servers/{id}/thresholds",
            get(thresholds::list_server).post(thresholds::create_server),
        )
        .route(
            "/api/sites/{id}/thresholds",
            get(thresholds::list_site).post(thresholds::create_site),
        )
        .route(
            "/api/thresholds/{id}",
            put(thresholds::update).delete(thresholds::delete),
        )
        .route("/api/servers/{id}/packages", get(packages::list))
        .route(
            "/api/servers/{id}/commands",
            get(commands::list).post(commands::create),
        )
        .route("/api/commands/{id}", get(commands::detail))
        .route("/api/agent-releases", get(commands::agent_releases))
        .route(
            "/api/server-update",
            get(server_update::get).post(server_update::start),
        )
        .route("/api/server-update/check", post(server_update::check))
        .route(
            "/api/servers/{id}/sources",
            post(sources::configure).delete(sources::revert),
        )
        .route(
            "/api/campaigns",
            get(campaigns::list).post(campaigns::create),
        )
        .route("/api/campaigns/{id}", get(campaigns::detail))
        .route("/api/campaigns/{id}/start", post(campaigns::start))
        .route("/api/campaigns/{id}/pause", post(campaigns::pause))
        .route("/api/campaigns/{id}/resume", post(campaigns::resume))
        .route("/api/campaigns/{id}/stop", post(campaigns::stop))
        .route(
            "/api/remediation/rules",
            get(remediation::list).post(remediation::create),
        )
        .route("/api/remediation/rules/{id}", delete(remediation::delete))
        .route(
            "/api/remediation/policies",
            get(remediation::list_policies).put(remediation::set_policy),
        )
        .route(
            "/api/settings/agent-install",
            get(settings::get_agent_install).put(settings::set_agent_install),
        )
        .route(
            "/api/settings/public-address",
            get(public_address::get).put(public_address::set),
        )
        .route("/api/remediation/classify", post(remediation::classify))
        .route(
            "/api/servers/{id}/proxmox",
            get(proxmox::get).put(proxmox::set).delete(proxmox::clear),
        )
        .fallback(not_found)
        .layer(middleware::from_fn(auth::csrf_guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Serves [`router`] on `listener` until `shutdown` resolves, then lets
/// in-flight requests finish.
pub async fn serve(
    listener: TcpListener,
    state: ApiState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if let Ok(addr) = listener.local_addr() {
        if !addr.ip().is_loopback() {
            tracing::warn!(%addr, "API listens on plain HTTP beyond loopback; keep it behind a TLS reverse proxy (ADR-0006)");
        }
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

#[cfg(test)]
mod tests;
