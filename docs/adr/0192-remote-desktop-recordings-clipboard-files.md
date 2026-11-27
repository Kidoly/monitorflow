# ADR-0192: Remote desktops, the rest: recordings, clipboard and files

Date: 2026-11-27
Status: accepted; recordings and the test against a real Windows host implemented. Completes ADR-0190's "Still to come" for desktops.

## Context

ADR-0190 opened Windows desktops from a server's page: the host's own RDP service on `127.0.0.1:3389`, tunnelled through the agent's sealed frames, IronRDP's web client in the browser, the central server as its RDCleanPath proxy. It left for later recording desktops ("recording a screen is video"), the clipboard and the file transfers of the web client, and a test against a real Windows host.

The proxy already does the TLS with the host's service: past it, the central server carries RDP in the clear both ways, inside the browser's WebSocket (itself HTTPS) on one side and the sealed frames on the other. What a recording or a rule about the clipboard needs is there, on the central server, which nothing a person in the session controls can bypass. A recording made in the browser would be made by the person it records.

## Decision

### The proxy reads the RDP it carries

The proxy cuts each direction into whole PDUs (CredSSP messages, the Early User Authorization Result, TPKT and fast-path PDUs) and passes on whole PDUs only. Bytes that are not RDP end the session. It reads what the connection sequence says, with IronRDP's own PDU crates (`ironrdp-pdu` 0.11, the version of the web client's): the static virtual channels the client asks for in its MCS Connect Initial and their IDs in the service's Connect Response, the I/O channel, the bulk compression the Client Info PDU asks for, and whether the client asks for the graphics pipeline. The Client Info PDU, which may carry the person's password, is read for its flags only, and nothing read is kept or logged.

### Recording a desktop: pictures of what changed

**Same settings, roles, audit and replay as terminals.** `remote_access_settings.record` now covers desktops: a desktop's row is `recorded` as a terminal's is, the check that kept desktops unrecorded is dropped, and the people told on the host hear "it is recorded" (ADR-0190). Administrators of the server's site replay recordings, `remote_session.recording_read` is audited, and `retention_days` deletes them. Since `record` is on by default, desktops opened after the upgrade are recorded where terminals were.

**The central server decodes the screen.** For a recorded desktop, the proxy feeds the service's fast-path PDUs and the Send Data Indications of the I/O channel to a decoder running IronRDP's own (`ironrdp-session` 0.13, `ironrdp-graphics`, `ironrdp-bulk`): the Demand Active gives the desktop's size, at the start and after each reactivation; bitmaps, RemoteFX surface commands and the pointer are drawn into an image as the web client draws them; the bulk decompressor's history is shared by the fast and slow paths as on the client. The decoder runs off the async runtime (`spawn_blocking`), fed by a bounded queue: a slow decoder slows the service's socket, as a slow browser does (ADR-0190's window).

**The format: PNG pictures of the part that changed.** Each picture is a row of `remote_desktop_frames`: the session, its number, its time after the session opened, the rectangle it covers and the screen's size, and a PNG without alpha. At most one picture a second, of the bounding box of what changed since the last one; a picture of the whole screen at the start, when the size changes, when the change covers half the screen or more, and at least once a minute while the screen changes, so a replay can start from a whole picture at most a minute of changes before. A screen that does not change adds nothing.

- **Why PNG.** The point of a desktop recording is what an administrator did: windows, menus, text typed into consoles. Lossless pictures keep text legible; a desktop's flat areas compress well, and the parts are small. JPEG would blur text, and its encoder crates carry an attribution clause; a video codec needs a native library on the central server and a player in the browser for what browsers already draw.
- **Why not the RDP stream itself.** Replaying RDP needs an RDP decoder in the browser that can start without a connection, which IronRDP's web client is not; the central server would then decode at each replay instead of once, and the raw stream of a busy screen is larger than its pictures.
- **Why not the browser's canvas.** A recording made by the client is made by the person it records.

**Bounded.** A recording stops at 256 MiB of pictures, and the session ends with "the recording reached its limit of 256 MiB; open a new session to go on": a recorded session must be recorded. Retention deletes recordings after `retention_days`, as for terminals. At most one picture a second keeps a busy hour of a full HD screen to a few hundred MB at worst, and an idle one to nothing.

**What the recording cannot read ends the session.** A client asking for the graphics pipeline (EGFX, in its Connect Initial) is refused when the desktop is recorded: its screen would come in a form the decoder does not draw, and the recording would be blank. IronRDP's web client does not ask for it. A PDU the decoder refuses ends the session with why, as does compressed data the client did not ask for. The pointer is drawn into the pictures where the client last put it; nothing typed is kept apart, a password field shows as Windows draws it.

**Replay.** `GET /api/v1/remote-sessions/{id}/recording` answers `frames`, each picture's place and size, beside the terminals' `chunks`; `GET /api/v1/remote-sessions/{id}/recording/frames/{seq}` answers one PNG, for the same people, its reading audited once with the recording. The page draws them on a canvas the width of the page, from the last whole picture before the position, with play, pause, a position and a speed (1×, 2×, 4×, 8×) on one line.

### A real Windows host in CI

The tests of ADR-0190 use a fake RDP service. A CI job, `windows-remote-desktop`, runs against a real one: GitHub's `windows-latest` runner, whose own Remote Desktop `deploy/windows/rdp-test.ps1` turns on, with network level authentication required as on a server's default, and a local account it makes for the test, in Remote Desktop Users only, its random password masked in the log and the account removed after. No self-hosted runner, nothing reachable from outside: the test stays on the runner's loopback.

`crates/agent/tests/desktop_rdp.rs` opens a desktop as the agent does, with the agent's own `RemoteSessionHandler` and its connection to `127.0.0.1:3389`, sealed frames and window included, and plays the central server's part with IronRDP's client crates (`ironrdp-connector` 0.12, `ironrdp-tokio`, `ironrdp-session`, test dependencies only): X.224, TLS, CredSSP with the local account, the connection sequence, then pictures until at least 1% of the screen is drawn. The central server does not build on Windows (ADR-0047), so its proxy is not in that test; `crates/agent/tests/remote.rs` has the same client through the proxy, with recording on, for a real RDP service on Linux (an xrdp container, run by hand), ignored by default.

### Rejected

- **Recording in the browser** (the canvas, or a `MediaRecorder` upload): made by the person recorded, who can stop it or send something else.
- **Keeping the RDP stream for replay**: needs an RDP decoder in the browser that starts without a connection, decodes at every replay, and is larger than pictures for a busy screen.
- **Video (H.264, VP9, WebM)**: a native encoder on the central server, and blurred text.
- **A setting of its own for recording desktops**: one `record` for both kinds, as ADR-0190 keeps one setting for who may reach a server.
- **A self-hosted Windows runner, or a Windows VM in a Linux job**: a machine to keep, or nested virtualization the hosted runners do not offer; the hosted Windows runner has Remote Desktop, off.

## Consequences

- A recorded desktop costs the central server a decoder and a PNG encoder per session while its screen changes, and its database a few hundred KB to a few hundred MB per hour, deleted after `retention_days`.
- A desktop's recording is as trustworthy as a terminal's: the central server makes it from what it carries to the browser, and a session it cannot record ends.
- Migration `20270106000000_remote_desktop_recordings`: `remote_desktop_frames`, and desktops may be recorded.
