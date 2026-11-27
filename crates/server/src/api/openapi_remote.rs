//! Remote terminals and desktops (ADR-0190) in the OpenAPI document: merged into it by
//! [`super::openapi::build`]; the router in `mod.rs` serves the routes.

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::{json, Value};
use utoipa::OpenApi;

use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use super::remote_sessions;

#[derive(OpenApi)]
#[openapi(
    paths(start, list, detail, close, recording, frame, socket, get_settings, set_settings),
    components(schemas(ErrorBody, Png)),
    modifiers(&Common),
)]
pub struct Doc;

const SESSION: &str = "0b5c8a52-6c43-4a6e-8f1e-9d8a3f1c2b7e";

/// A PNG picture.
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
struct Png(Vec<u8>);

fn session() -> Value {
    json!({
        "id": SESSION, "server_id": ex::SERVER, "hostname": "web-1", "kind": "terminal",
        "requested_by": "alice", "created_at": "2026-11-23T09:30:00Z",
        "opened_at": "2026-11-23T09:30:02Z", "closed_at": "2026-11-23T09:41:15Z",
        "close_reason": "closed by alice", "duration_seconds": 673, "recorded": true,
        "bytes_in": 412, "bytes_out": 18734
    })
}

fn settings() -> Value {
    json!({"min_role": "administrator", "record": true, "idle_timeout_minutes": 15,
           "retention_days": 90})
}

#[utoipa::path(post, path = "/api/v1/servers/{id}/remote-sessions", tag = "servers",
    summary = "Start a remote terminal or desktop",
    description = "Starts a terminal on the server, through its agent's connection and its relay: no \
port to open and nothing to install. Answers the session and a one-time `token`, valid one minute: open \
the WebSocket at `socket` and send it first, with the terminal's size, as \
`{\"token\": \"…\", \"cols\": 120, \"rows\": 40}`. Only then does the central server send the agent a signed \
`remote_session` command, which opens a login shell as the agent's own user, or on Linux as the account the host's \
administrator chose (`MF_REMOTE_TERMINAL_USER`); the central server cannot choose it.\n\n\
The host must have allowed it: `command_kinds.remote_session` of the server is `accepted` where its \
administrator ran the installer with `MF_REMOTE_TERMINAL=1`, or `monitorflow-agent remote-access terminal \
on`. The central server cannot allow it by itself. While the session is open, people logged in on the \
server are told who opened it and whether it is recorded.\n\n\
With `{\"kind\": \"desktop\"}`, on a Windows server, it is the host's own Remote Desktop service instead: the \
agent connects to `127.0.0.1:3389`, and to nothing else, and the WebSocket speaks the RDCleanPath protocol of \
IronRDP's web client, the `token` as its `proxy_auth` (see the socket). The host must have allowed desktops \
apart: `command_kinds.remote_desktop` is `accepted` where its administrator installed with \
`REMOTE_DESKTOP=1` or ran `monitorflow-agent.exe remote-access desktop on`. The person signs in to Windows as \
usual; MonitorFlow neither asks for nor keeps Windows passwords. Where `record` is on, the central server \
records the desktop's screen from the RDP it carries (see the recording).\n\n\
**Who**: the role `GET /api/v1/settings/remote-access` names, or above, on the server's site. **Area**: \
`servers`. Audited as `remote_session.start`, then `remote_session.open` and `remote_session.close` with \
its duration. **Not safe to repeat**: each call starts another session.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    request_body(content = remote_sessions::StartRequest, example = json!({"kind": "terminal"})),
    responses(
        (status = 201, description = "The session, waiting for its WebSocket",
            body = remote_sessions::RemoteSessionStart,
            example = json!({
                "session": ex::merge(session(), json!({"opened_at": null, "closed_at": null,
                    "close_reason": null, "duration_seconds": null, "bytes_in": 0, "bytes_out": 0,
                    "created_at": "2026-11-23T09:30:00Z"})),
                "token": "4f9a0c3e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b2a1f",
                "token_expires_at": "2026-11-23T09:31:00Z",
                "socket": format!("/api/v1/remote-sessions/{SESSION}/socket")
            })),
        (status = 400, description = "Another `kind` than `terminal` or `desktop`: `invalid`",
            body = ErrorBody),
        (status = 403, description = "The role may not open terminals here: `forbidden`; or outside \
the key's areas: `api_key_scope`", body = ErrorBody),
        (status = 404, description = "No such server, a device, or outside the key's reach: \
`not_found`", body = ErrorBody),
        (status = 409, description = "The server is removed, its host did not allow this kind (the \
message gives the line that allows it), a desktop on a server that is not Windows, its agent is too old \
or not connected: `conflict`",
            body = ErrorBody),
    ))]
fn start() {}

#[utoipa::path(get, path = "/api/v1/servers/{id}/remote-sessions", tag = "servers",
    summary = "List a server's remote sessions",
    description = "The server's last 100 remote sessions, terminals and desktops, newest first: who \
opened them, when, for how long, why they ended and whether they were recorded. Open ones have `closed_at` null.\n\n\
**Who**: those who may open terminals there, and the site's administrators. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The server")),
    responses(
        (status = 200, description = "The sessions", body = Vec<remote_sessions::RemoteSession>,
            example = json!([session()])),
        (status = 403, description = "The role may not see them: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such server, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn list() {}

#[utoipa::path(get, path = "/api/v1/remote-sessions/{id}", tag = "servers",
    summary = "Get a remote session",
    description = "One remote session, as in the server's list.\n\n\
**Who**: those who may open terminals on its server, and the site's administrators. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The session")),
    responses(
        (status = 200, description = "The session", body = remote_sessions::RemoteSession,
            example = json!(session())),
        (status = 403, description = "The role may not see it: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such session, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn detail() {}

#[utoipa::path(post, path = "/api/v1/remote-sessions/{id}/close", tag = "servers",
    summary = "Close a remote session",
    description = "Ends an open session, wherever it runs: the shell gets a hangup, or the desktop's \
connection closes, and the person in it sees who closed it. A session whose WebSocket never came closes at once, and its token stops \
working.\n\n\
**Who**: whoever opened it, or an administrator of its server's site. **Area**: `servers`. Audited as \
`remote_session.close_request`, then `remote_session.close`.",
    params(("id" = uuid::Uuid, Path, description = "The session")),
    responses(
        (status = 204, description = "Closing"),
        (status = 403, description = "Neither who opened it nor an administrator there: `forbidden`",
            body = ErrorBody),
        (status = 404, description = "No such session, or outside the key's reach: `not_found`",
            body = ErrorBody),
        (status = 409, description = "It is already closed: `conflict`", body = ErrorBody),
    ))]
fn close() {}

#[utoipa::path(get, path = "/api/v1/remote-sessions/{id}/recording", tag = "servers",
    summary = "Get a remote session's recording",
    description = "A terminal's: what it printed, in order, each chunk with its time after the session \
opened: written into a terminal of the session's size, it shows what the person saw. Keystrokes are not \
kept apart: what the terminal echoed is in it, and what it did not, such as a password, is not.\n\n\
A desktop's: the pictures of its screen in `frames`, `chunks` empty. The central server decodes the screen \
from the RDP it carries and keeps a picture at most once a second, of the part that changed, and of the \
whole screen at the start, when its size changes and once a minute while it changes. Get each with \
`GET /api/v1/remote-sessions/{id}/recording/frames/{seq}` and draw it at `x`, `y`: from a picture of the \
whole screen (`width` and `height` equal to `screen_width` and `screen_height`) on, they show what the \
screen showed. A desktop's recording stops at 256 MiB of pictures, and the session with it.\n\n\
**Who**: administrators of the server's site. **Area**: `servers`. Audited as \
`remote_session.recording_read`.",
    params(("id" = uuid::Uuid, Path, description = "The session")),
    responses(
        (status = 200, description = "The recording", body = remote_sessions::RemoteRecording,
            example = json!({"session": session(), "chunks": [
                {"at_ms": 120, "data": "YWxpY2VAd2ViLTE6fiQg"},
                {"at_ms": 4310, "data": "dXB0aW1lDQogMDk6MzA6MDYgdXAgMyBkYXlzDQo="}
            ], "frames": []})),
        (status = 403, description = "Not an administrator there: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such session, not recorded, or outside the key's reach: \
`not_found`", body = ErrorBody),
    ))]
fn recording() {}

#[utoipa::path(get, path = "/api/v1/remote-sessions/{id}/recording/frames/{seq}", tag = "servers",
    summary = "Get a picture of a desktop's recording",
    description = "One picture of a recorded desktop's screen, as a PNG: the part `x`, `y`, `width`, \
`height` its entry in the recording's `frames` gives.\n\n\
**Who**: administrators of the server's site. **Area**: `servers`. Reading the recording is audited once, \
as `remote_session.recording_read`, not each picture.",
    params(("id" = uuid::Uuid, Path, description = "The session"),
           ("seq" = i32, Path, description = "The picture's `seq`")),
    responses(
        (status = 200, description = "The picture", content_type = "image/png", body = Png),
        (status = 403, description = "Not an administrator there: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such session or picture, not recorded, or outside the key's \
reach: `not_found`", body = ErrorBody),
    ))]
fn frame() {}

#[utoipa::path(get, path = "/api/v1/remote-sessions/{id}/socket", tag = "servers",
    summary = "Open a remote session's WebSocket",
    description = "The session's terminal, as a WebSocket (`Upgrade: websocket`). No key or cookie is \
needed: the first message must be a text message with the session's one-time token and the terminal's \
size, `{\"token\": \"…\", \"cols\": 120, \"rows\": 40}`, within 10 seconds.\n\n\
Then, from the central server: text messages `{\"type\": \"waiting\"}` while the agent opens the terminal, \
`{\"type\": \"opened\"}` once it did, `{\"type\": \"closed\", \"reason\": \"…\"}` at the end; and binary \
messages with what the terminal prints. To it: binary messages with keystrokes, and text messages \
`{\"type\": \"resize\", \"cols\": 100, \"rows\": 30}` or `{\"type\": \"close\"}`. Messages are at most 64 KiB.\n\n\
The session closes after the idle time of the settings without keystrokes, when the shell exits, when \
the agent's connection drops, and after 12 hours.\n\n\
**A desktop's WebSocket** (`kind` `desktop`) is IronRDP's RDCleanPath protocol instead, in binary messages \
only: the first is a DER `RDCleanPathPdu` request whose `proxy_auth` is the token and whose \
`x224_connection_pdu` is the client's X.224 connection request; its `destination` is ignored. The central \
server answers with the RDCleanPath response (the service's X.224 confirm and its TLS certificates, for \
network level authentication) or an error, then carries RDP both ways, the TLS with the host's service done \
on the central server's side. The WebSocket closes with why the session ended as its reason. The idle time \
counts from the last bytes the browser sent.",
    params(("id" = uuid::Uuid, Path, description = "The session")),
    responses(
        (status = 101, description = "Switching to the WebSocket protocol"),
        (status = 400, description = "Not a WebSocket request"),
        (status = 404, description = "This central server does not run remote sessions: `not_found`",
            body = ErrorBody),
    ))]
fn socket() {}

#[utoipa::path(get, path = "/api/v1/settings/remote-access", tag = "settings",
    summary = "Get the remote access settings",
    description = "Who may open remote terminals and desktops, whether they are recorded, when an \
idle session closes and how long recordings are kept. Each host must also allow terminals and desktops \
itself, each apart, which no setting here can change.\n\n\
**Who**: global administrators. **Area**: `administration`.",
    responses(
        (status = 200, description = "The settings", body = remote_sessions::RemoteAccessSettings,
            example = json!(settings())),
        (status = 403, description = "Not a global administrator: `forbidden`", body = ErrorBody),
    ))]
fn get_settings() {}

#[utoipa::path(put, path = "/api/v1/settings/remote-access", tag = "settings",
    summary = "Change the remote access settings",
    description = "Replaces the settings. `min_role` is the lowest role, on a server's site, that may open \
a terminal or a desktop there: `operator`, `approver` or `administrator`; `null` lets nobody. `record` keeps \
what terminals print and the screen of desktops. New sessions follow the new values; open ones keep theirs.\n\n\
**Who**: global administrators. **Area**: `administration`. Audited as `settings.remote_access`.",
    request_body(content = remote_sessions::RemoteAccessSettings, example = json!(settings())),
    responses(
        (status = 200, description = "The settings", body = remote_sessions::RemoteAccessSettings,
            example = json!(settings())),
        (status = 400, description = "`viewer`, or a number out of its range: `invalid`",
            body = ErrorBody),
        (status = 403, description = "Not a global administrator: `forbidden`", body = ErrorBody),
    ))]
fn set_settings() {}
