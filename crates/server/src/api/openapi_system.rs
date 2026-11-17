//! The public API's description of the central server itself: health,
//! updates of the server, backups and the certificate authority.
//! Merged into the document by [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::openapi::OpenApi as Document;
use utoipa::{Modify, OpenApi, ToSchema};

use super::openapi::{Common, ErrorBody};
use super::{backups, certificate_authority, health, server_update};

#[derive(OpenApi)]
#[openapi(
    paths(
        live, ready, update_status, check_update, start_update, list_backups, start_backup,
        download_backup, ca, start_ca_change, next_ca_step,
    ),
    components(schemas(BackupArchive)),
    modifiers(&Common, &Unauthenticated),
    tags(
        (name = "health", description = "Probes for load balancers and monitoring: whether the \
central server's process serves requests and whether its database answers. They need no key and \
say nothing beyond up or down."),
        (name = "server-update", description = "Updates of the central server itself, carried out \
by the updater service of the compose deployment: what runs, whether a newer signed release exists, \
and the request that installs one. The updater takes a backup first and rolls back by itself when \
the new version does not start. A global administrator's; with a key, access \"Administer\", reach \
\"Everything\" and the \"updates\" area."),
        (name = "backups", description = "Backups of the whole central server, taken by the \
updater: once a day, before each update and on request; the 7 latest are kept. A backup holds the \
database of every customer and the server's keys, including the certificate authority's, so only \
a global administrator lists, takes or downloads them; with a key, access \"Administer\", reach \
\"Everything\" and the \"administration\" area. Every download is in the audit log."),
        (name = "certificate-authority", description = "The certificate authority that signs the \
certificates of the central server, relays and agents for mTLS. These operations show when it \
expires and change it, planned or after a suspected compromise; they never hand out a key or a \
certificate. A global administrator's; with a key, access \"Administer\", reach \"Everything\" and \
the \"administration\" area."),
    )
)]
pub(super) struct SystemDoc;

/// Health needs no key: it has no rate limit and is never refused.
struct Unauthenticated;

impl Modify for Unauthenticated {
    fn modify(&self, doc: &mut Document) {
        for path in ["/api/v1/health", "/api/v1/health/ready"] {
            let Some(op) = doc.paths.paths.get_mut(path).and_then(|i| i.get.as_mut()) else {
                continue;
            };
            op.security = Some(vec![
                utoipa::openapi::security::SecurityRequirement::default(),
            ]);
            for status in ["400", "401", "403", "404", "429"] {
                op.responses.responses.remove(status);
            }
        }
    }
}

/// A backup file: a gzip-compressed tar archive.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
struct BackupArchive(Vec<u8>);

/// Check the server is up
///
/// Answers 200 while the central server's process serves requests. Needs
/// no key and counts against no rate limit; also served at `/api/health`.
#[utoipa::path(get, path = "/api/v1/health", tag = "health",
    responses((status = 200, description = "The process serves requests", body = health::Status, example = json!({ "status": "ok" }))))]
fn live() {}

/// Check the server is ready
///
/// Answers 200 when PostgreSQL answers a trivial query within 2 seconds,
/// 503 otherwise. Needs no key and counts against no rate limit; also
/// served at `/api/health/ready`.
#[utoipa::path(get, path = "/api/v1/health/ready", tag = "health",
    responses(
        (status = 200, description = "The database answers", body = health::Status, example = json!({ "status": "ready" })),
        (status = 503, body = health::Status, description = "The database does not answer",
            example = json!({ "status": "unavailable" })),
    ))]
fn ready() {}

/// Read the server's update status
///
/// The running version, what the updater service last reported, the last
/// update with its steps, and a newer release the daily check saw.
/// `enabled` is false when the deployment has no updater (a package
/// install, for one): updates are then done by hand. Changes nothing.
#[utoipa::path(get, path = "/api/v1/server-update", tag = "server-update",
    responses((status = 200, description = "The update status", body = server_update::UpdateView, example = json!({
        "running_version": "0.4.0",
        "enabled": true,
        "source": "https://releases.example.com/monitorflow",
        "updater": {
            "version": "0.4.0",
            "heartbeat_at": "2026-11-14T09:30:12Z",
            "alive": true,
            "running": {
                "version": "0.4.0",
                "server_image": "registry.example.com/monitorflow-server@sha256:5d41402abc4b2a76b9719d911017c592ae2fb4fa6bdd6a8e3f9d22c2f2e6e3a1",
                "web_image": "registry.example.com/monitorflow-web@sha256:7b52009b64fd0a2a49e6d8a939753077792b0554aa1b6c4d13e2c9ab0f5c1e8d",
                "updater_image": "registry.example.com/monitorflow-updater@sha256:e4da3b7fbbce2345d7772b0674a318d5c0a2b3f0e4b8a7d6c5e4f3a2b1c0d9e8"
            }
        },
        "update": {
            "id": "3f1c2a9e-8b7d-4c6e-9a51-2d0e7f4b6c18",
            "from": "0.3.2",
            "to": "0.4.0",
            "requested_by": "admin",
            "state": "succeeded",
            "steps": [
                { "at": "2026-11-12T20:00:03Z", "message": "Release 0.4.0 verified" },
                { "at": "2026-11-12T20:01:40Z", "message": "Backup taken" },
                { "at": "2026-11-12T20:03:05Z", "message": "0.4.0 is running" }
            ],
            "error": null,
            "backup": "monitorflow-backup-20261112T200005Z-update.tar.gz",
            "migrated": true,
            "started_at": "2026-11-12T20:00:01Z",
            "finished_at": "2026-11-12T20:03:05Z"
        },
        "requested": false,
        "available": null,
        "channel": "regular"
    }))))]
fn update_status() {}

/// Check for a newer release
///
/// Fetches the latest release of the server's `channel` from `source` and
/// verifies its signature, as the updater will before installing it, and
/// records what it saw as the daily check does. The `regular` channel is
/// the latest release; the `tested` channel, followed while a license key
/// is valid, is the release named by the signed `tested.json` next to it
/// (ADR-0147). Installs nothing; safe to repeat. Answers 200 even
/// when the source cannot be reached: `reachable` and `error` say so.
#[utoipa::path(post, path = "/api/v1/server-update/check", tag = "server-update",
    responses((status = 200, description = "What the source offers", body = server_update::CheckResult, example = json!({
        "source": "https://releases.example.com/monitorflow",
        "running_version": "0.4.0",
        "channel": "regular",
        "reachable": true,
        "error": null,
        "latest": {
            "version": "0.4.1",
            "newer": true,
            "release_notes_url": "https://releases.example.com/monitorflow/0.4.1/notes",
            "images": {
                "server": "registry.example.com/monitorflow-server@sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
                "web": "registry.example.com/monitorflow-web@sha256:60303ae22b998861bce3b28f33eec1be758a213c86c93c076dbe9f558c11c752",
                "updater": "registry.example.com/monitorflow-updater@sha256:fd61a03af4f77d870fc21e05e7e80678095c92d808cfb3b5c279ee04c74aca13",
                "mirror": null
            },
            "web_changes": true,
            "updater_changes": false,
            "agent_release_cached": false
        }
    }))))]
fn check_update() {}

/// Update the server
///
/// Asks the updater to install `version`: it verifies the release, takes a
/// backup, replaces the images and checks the new server answers, rolling
/// back to the previous images if it does not. The central server restarts
/// meanwhile, so requests may fail for a short while; follow the update with
/// `GET /api/v1/server-update`, whose `update.id` is the `id` answered here.
/// Audited as `server.update` with the versions from and to.
///
/// Errors: 400 `invalid` when `version` is not a release version or not
/// newer than the running one; 409 `conflict` when the deployment has no
/// updater, the updater is not running, or an update is already under
/// way. A second request while the first runs is refused with 409, so a
/// retry never installs twice.
#[utoipa::path(post, path = "/api/v1/server-update", tag = "server-update",
    request_body(content = server_update::StartRequest, example = json!({ "version": "0.4.1" })),
    responses(
        (status = 202, description = "Asked of the updater", body = server_update::Started,
            example = json!({ "id": "8c2f6d1e-4b3a-4f9e-a7c5-0d1b2e3f4a5b" })),
        (status = 409, body = ErrorBody, description = "No updater, updater not running, or an update under way: `conflict`"),
    ))]
fn start_update() {}

/// List backups
///
/// The last backup, running or finished, when the next automatic one is
/// due, and the files kept, newest first. `downloads` says whether the
/// files can be downloaded through the API; when `host_dir` is true they
/// sit in a host directory only root can read, and are copied from there.
/// Changes nothing.
#[utoipa::path(get, path = "/api/v1/backups", tag = "backups",
    responses((status = 200, description = "Backups and their state", body = backups::BackupsView, example = json!({
        "enabled": true,
        "downloads": true,
        "host_dir": false,
        "updater_alive": true,
        "requested": false,
        "keep": 7,
        "last": {
            "id": "daily-20261114",
            "kind": "daily",
            "requested_by": "",
            "state": "succeeded",
            "error": null,
            "warning": null,
            "file": "monitorflow-backup-20261114T030000Z-daily.tar.gz",
            "started_at": "2026-11-14T03:00:00Z",
            "finished_at": "2026-11-14T03:02:41Z"
        },
        "last_success_at": "2026-11-14T03:02:41Z",
        "next_at": "2026-11-15T03:00:00Z",
        "files": [{
            "name": "monitorflow-backup-20261114T030000Z-daily.tar.gz",
            "size": 48_213_771,
            "created_at": "2026-11-14T03:00:00Z",
            "kind": "daily"
        }]
    }))))]
fn list_backups() {}

/// Back up now
///
/// Asks the updater for a backup of the database and the server's data
/// directory. Answers at once; the backup shows as `last` in
/// `GET /api/v1/backups` once the updater takes it, and the oldest file
/// beyond 7 is removed when it ends. Audited as `server.backup`.
///
/// Errors: 409 `conflict` when the deployment has no updater, the updater
/// is not running, or a backup is already running or asked for. A second
/// request while one runs is refused, so a retry never takes two.
#[utoipa::path(post, path = "/api/v1/backups", tag = "backups",
    responses(
        (status = 202, description = "Asked of the updater", body = backups::Started,
            example = json!({ "id": "c7a1e2f3-9d84-4b6a-8e1f-2a3b4c5d6e7f" })),
        (status = 409, body = ErrorBody, description = "No updater, updater not running, or a backup under way: `conflict`"),
    ))]
fn start_backup() {}

/// Download a backup
///
/// The file itself, `application/gzip`, served as an attachment with
/// `Cache-Control: no-store`; `Range` requests are honoured. It is a tar
/// archive of `manifest.json` (format, time, server version, latest
/// migration), `database.dump` (`pg_dump` custom format of the whole
/// database: every customer's servers, sites, incidents, commands, users
/// with their password hashes, API key hashes and the audit log) and
/// `serverdata/`, the server's data directory: the certificate
/// authority's private key, the key that signs commands to agents, the
/// key that opens the stored SMTP passwords, webhook secrets and AI key,
/// and the mirror signing key. Whoever holds the file can read all of
/// that and act as the central server towards every agent and relay;
/// keep it as you would those keys.
///
/// Only a global administrator, or a key with access "Administer", reach
/// "Everything" and the "administration" area, may download one. Each
/// download is audited as `server.backup.download` with the file name.
///
/// Errors: 404 `not_found` for a name the updater did not write or a file
/// already rotated away; 409 `conflict` when the backups volume is not
/// mounted in the server or only root can read it on the host.
#[utoipa::path(get, path = "/api/v1/backups/{name}", tag = "backups",
    params(("name" = String, Path, description = "A `name` from `files`, like `monitorflow-backup-20261114T030000Z-daily.tar.gz`")),
    responses(
        (status = 200, description = "The backup archive", content_type = "application/gzip", body = BackupArchive),
        (status = 409, body = ErrorBody, description = "Backups cannot be read by the server: `conflict`"),
    ))]
fn download_backup() {}

fn ca_example() -> serde_json::Value {
    json!({
    "fingerprint": "3b1f6c0e9a7d2f4b8c5e1a0d9f7b3c6e2a4d8f0b1c3e5a7d9f2b4c6e8a0d1f3b",
    "created_at": "2025-03-01T10:00:00Z",
    "expires_at": "2035-03-01T10:00:00Z",
    "expires_soon": false,
    "auto_change_at": "2034-09-02T10:00:00Z",
    "rotation": {
        "stage": "introduced",
        "emergency": false,
        "started_at": "2026-11-14T09:00:00Z",
        "switched_at": null,
        "distrust_at": null,
        "other_fingerprint": "a9c4e2b7d1f0836e5c2a9b4d7f1e0c3a8b6d2f5e9c1a4b7d0e3f6a9c2b5d8e1f",
        "total": 12,
        "done": 11,
        "waiting": [{
            "role": "agent",
            "peer_id": "6f2d1c4b-8a3e-4d7f-9b0c-1e2a3b4c5d6e",
            "name": "web-1",
            "last_seen_at": "2026-11-14T08:55:10Z",
            "not_after": "2027-02-10T00:00:00Z",
            "moved": false,
            "trusts_all": false,
            "too_old": false,
            "cut_off": false
        }]
    },
    "restored_retired_at": null,
    "cut_off": []
    })
}

/// Read the certificate authority
///
/// Its fingerprint, when it expires and when a planned change starts by
/// itself (180 days before), the change under way with how many servers
/// and relays moved, and those cut off by an emergency change. Shows no
/// key and no certificate. 409 `conflict` when this server has no
/// certificate authority of its own.
#[utoipa::path(get, path = "/api/v1/certificate-authority", tag = "certificate-authority",
    responses(
        (status = 200, description = "The certificate authority", body = certificate_authority::CaView, example = ca_example),
        (status = 409, body = ErrorBody, description = "No certificate authority on this server: `conflict`"),
    ))]
fn ca() {}

/// Change the certificate authority
///
/// Makes a new certificate authority. Planned (`emergency` false), the
/// new one is first only trusted: servers and relays learn to trust it
/// over their next connections, then it issues once all have moved, then the old one is retired
/// and its key destroyed, each step taken by the server by itself. In an
/// emergency the new one issues at once and the old one stops being
/// trusted 24 hours later: servers and relays that have not connected by
/// then must be installed again. An emergency request during a planned
/// change switches it at once. Audited as `ca.rotation.start`; agents and
/// relays receive the new trust over their next connections, no command
/// is sent.
///
/// Errors: 409 `conflict` when a change is already under way, when more
/// than one central server instance is running, or when this server has
/// no certificate authority.
#[utoipa::path(post, path = "/api/v1/certificate-authority/rotation", tag = "certificate-authority",
    request_body(content = certificate_authority::StartRequest, example = json!({ "emergency": false })),
    responses(
        (status = 202, description = "Started; the certificate authority as it now stands", body = certificate_authority::CaView, example = ca_example),
        (status = 409, body = ErrorBody, description = "A change under way, several instances, or no CA: `conflict`"),
    ))]
fn start_ca_change() {}

/// Take the next step of the change now
///
/// Takes the next step without waiting for the servers and relays that
/// have not moved: makes the new certificate authority the one that
/// issues, or retires the old one and destroys its key. Those not moved
/// when the old one is retired must be installed again; `waiting` lists
/// them first. Audited as `ca.rotation.switch` or `ca.rotation.retire`
/// with how many had not moved. Not safe to repeat: a second call takes
/// the step after.
///
/// Errors: 409 `conflict` when no change is under way, more than one
/// central server instance runs, or this server has no certificate
/// authority.
#[utoipa::path(post, path = "/api/v1/certificate-authority/rotation/next", tag = "certificate-authority",
    responses(
        (status = 200, description = "Step taken; the certificate authority as it now stands", body = certificate_authority::CaView, example = ca_example),
        (status = 409, body = ErrorBody, description = "No change under way, several instances, or no CA: `conflict`"),
    ))]
fn next_ca_step() {}
