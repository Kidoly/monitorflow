# ADR-0190: Remote terminals and desktops through the agent's connection

Date: 2026-11-24
Status: accepted; terminals and Windows remote desktops are implemented (desktops on 2026-11-25)

## Context

Phase 20 adds what people otherwise keep a separate remote management tool for. The first need is to open a terminal on a server from its page (Linux and Windows), and a remote desktop on Windows, without opening a port or installing another tool. Every action that changes a server follows the rule of AI commands and major upgrades: the server must have agreed to it at install, so a compromised central server cannot give itself control of servers that did not (ADR-0098).

What exists already: each agent keeps one mTLS gRPC stream open to its relay or to the central server, relays pass envelopes on without reading them, commands are signed by the CA key and checked by the agent (ADR-0031), and the central server can run as several instances, the browser and the agent's stream each reaching any of them (ADR-0089).

A terminal is different from a command in three ways. It lasts, with bytes both ways for minutes or hours. What is typed is not classified by any rule: whoever types runs anything the shell's user may. And every keystroke crosses the relays, which the security model does not trust to inject commands (architecture §4): a relay that could add bytes to a session would have a shell.

## Decision

### Consent on the host, off by default

**`[remote] terminal`** in `agent.toml`, off by default, allows terminals. Only the host's administrator sets it:

- Linux: `MF_REMOTE_TERMINAL=1` with the installer, on a new or an installed agent; `0` takes it back. The installer runs `monitorflow-agent remote-access terminal on|off` as root.
- Both systems: `monitorflow-agent remote-access terminal on|off`, which edits `agent.toml` as `ai-commands` does (ADR-0084), and needs a restart of the agent.
- Windows: the MSI property `REMOTE_TERMINAL=1`, which runs that command at install and upgrade.

The file is `root:monitorflow 0640` on Linux and read-only for the agent's account on Windows (ADR-0050): neither the agent's user nor any command can write it. Unlike other keys there is no environment variable for it, so the file is the one place to look. The agent reports the kind `remote_session` as accepted or off in its `Hello` (ADR-0076), and the server's page shows the line to run where it is off.

**Why off.** AI commands are on by default in the Add servers page (ADR-0175) because each one is classified by the risk rules and the risky ones wait for an approver. A terminal has no rules: whoever opens it runs what they type. Turning it on is a choice about a host and the people who reach it, which its administrator makes once, with the line the Add servers page offers ("Allow a remote terminal", unticked). The remote desktop has its own consent, off by default too: it gives a whole Windows session (below).

### The account: the agent's user, or one chosen on the host

By default the shell runs as the agent's own user, with a clean environment: `monitorflow` on Linux (a login shell, `bash -l` or `sh -l`, with the agent's groups, so it reads the journal), the virtual account `NT SERVICE\MonitorFlowAgent` on Windows (Windows PowerShell on a pseudo console). That user's sudo rules allow only the root helpers, which act on signed commands and nothing typed (ADR-0108).

On Linux the host's administrator may choose another local account, **`[remote] terminal_user`**, with `MF_REMOTE_TERMINAL_USER=<account>` with the installer or `monitorflow-agent remote-access terminal-user <account>`; `none` goes back to the agent's user. It is never root unless named there, and the installer warns when it is. The agent then starts `sudo -n /usr/bin/monitorflow-agent remote-shell`, one more exact sudo line, with the signed `remote_session` command on a pipe as its stdin and the terminal as its stdout. That root helper checks the command as every helper does, against its own anchor, for this machine, unexpired and never used (ADR-0108), reads the account from `agent.toml`, which root owns, refuses unless terminals are allowed and an account is named, then puts the terminal on its stdin and becomes `runuser -l <account>`. The agent's user alone cannot get that shell: it needs a command the central server signed for this session. The central server cannot choose the account either. On Windows terminals run as the agent's account; another one would go through the update service, which runs as LocalSystem, and is left for later.

### The session: a signed command, then sealed frames

1. A person allowed to (below) asks to start a session. The server stores it with a one-time token valid one minute and answers it.
2. The browser opens a WebSocket on `/api/remote-sessions/{id}/socket`, on whichever instance it reaches, and sends the token first. That instance runs the session from then on: it makes an X25519 key for the session and queues the signed `remote_session` command, `RemoteSessionOpen`: the session's ID, `terminal`, its public key, the size, who opened it, the idle time and whether it is recorded. It lives two minutes.
3. The agent checks the command as any other (signature, its own ID, journal, expiry), refuses it without `remote.terminal`, starts the shell on a pseudo-terminal, and answers with a `RemoteFrame` `opened`: its own X25519 public key, its certificate, and an ECDSA signature with its certificate's key over the session ID and both public keys. The command's result says the terminal opened; the session then goes on in a task of its own, so the dispatcher runs other commands.
4. The server believes the answer only if its CA issued the certificate to this agent (`verify_issued`), the certificate is in `certificates` and not revoked, and the signature verifies. Both sides derive a key per direction with HKDF-SHA256 over the X25519 secret, and seal every frame (`RemoteData`: keystrokes, output, a new size, a close) with ChaCha20-Poly1305, numbered from 0 in each direction; the session's ID and the direction are authenticated with it. A frame out of order or that does not open ends the session.

The agent knows the server's key is the server's from the command's signature; the server knows the agent's key is the agent's from its certificate. A relay sees sealed bytes: it cannot read the session, add a keystroke, replay one, or stand in for the agent to show a fake shell and collect what is typed. It can drop frames, which ends the session, as it can drop anything. `RemoteFrame` is a new variant of the envelope: relays forward it unchanged, both ways, with no code of their own; a relay older than this decision drops it, and the session ends with "the agent did not answer".

At most 4 sessions run on one agent, each sends at most 256 KiB of output a second so samples and results keep flowing on the same stream, and each closes after 12 hours whatever happens.

### Between instances

The instance holding the WebSocket is often not the one holding the agent's stream. Frames between them go through the registry when one instance holds both, and otherwise as PostgreSQL notifications on `monitorflow_remote`, at most 4 KiB of data per frame so a notification stays under PostgreSQL's 8000 bytes. A notification carries a sealed frame: nothing a relay could not see already. The stream's instance passes a frame on only for an open session of that very agent.

The instance running a session moves `remote_sessions.seen_at` on every 20 seconds; any instance closes a session not seen for two minutes ("the central server instance running it stopped") and one whose browser never came with its token. The session also ends when the agent's stream drops: frames lost on the way cannot be sent again, so the agent closes its sessions when its stream closes, and the server when the agent has no stream for 30 seconds.

### Who, audit, recording, idle

**`remote_access_settings`**, one row, changed by global administrators in Administration › Server settings and `PUT /api/settings/remote-access`:

- **`min_role`**: the lowest role, on a server's site, that may open a terminal there; `administrator` by default; `approver` or `operator` widen it, `null` lets nobody. Viewers never may. Those who may open terminals and the site's administrators see the server's sessions; the site's administrators also read recordings and close anyone's session.
- **`record`**, on by default: what the terminal printed is kept in `remote_session_recordings`, written every 2 seconds, and replayed in the browser. Keystrokes are not kept apart: what the terminal echoed is in the transcript and what it did not (a password typed at a prompt) is not. Recordings are deleted after **`retention_days`**, 90 by default.
- **`idle_timeout_minutes`**, 15 by default: the agent closes a session without keystrokes for that long, and the server shortly after if it did not hear it.

Every session is a row of `remote_sessions` (who, server, when it was asked, opened and closed, why, bytes each way) and audit entries: `remote_session.start`, `.open`, `.close` with its duration and reason, `.close_request`, `.recording_read`, and `settings.remote_access`.

### On the server, while a session is open

People logged in on the host are told when a session opens, by whom and whether it is recorded, and when it closes: `wall` on Linux, `msg *` on Windows, both best effort (a terminal with `mesg n`, or a Windows without `msg.exe`, is not told). The agent's log says it too, and the shell's environment has `MONITORFLOW_REMOTE_USER`.

### In the browser

The server's page gets a Terminal tab where the agent accepts `remote_session` and the person may open one, with the past sessions below and their recordings. The terminal is xterm.js (`@xterm/xterm` 6.0.0 and `@xterm/addon-fit` 0.11.0, pinned), loaded only on that tab. Everything the page does is in `/api/v1` and its OpenAPI document; the WebSocket keeps its upgrade through `/api/v1`, and its one-time token, not a cookie or a key, authenticates it, so a page of another site cannot use a person's cookie to open one.

### The remote desktop (Windows)

Two ways were studied:

1. **The host's own RDP, tunnelled.** The agent opens a TCP connection to `127.0.0.1:3389`, and nothing else, and carries its bytes as sealed frames. A web RDP client in the browser speaks RDP; no port opens beyond loopback, nothing is installed on the host, and the person signs in to Windows as usual, with network level authentication. It needs Remote Desktop turned on, and gives a session of its own on Windows Server (Windows 10 and 11 allow one session at a time).
2. **Capture and input by the agent**: desktop duplication, video encoding, and injected keyboard and mouse. It shows the console session, as tools for assisting a user do, but the agent's service cannot see a user's desktop: it needs a helper started in that session by a LocalSystem service, the secure desktop of UAC and the sign-in screen, encoding, cursor, layouts and clipboard. It is several times the work of the first.

The first is the one built. The second waits for workstations, if they are asked for.

**The web client: IronRDP.** Three were weighed:

- **IronRDP's web client**, chosen: Devolutions' RDP stack in Rust compiled to WebAssembly, MIT or Apache-2.0. `@devolutions/iron-remote-desktop` 0.11.0 (a web component: canvas, scaling, keyboard, mouse, cursor) and `@devolutions/iron-remote-desktop-rdp` 0.7.0 (the RDP module, about 6 MB with its WebAssembly inlined), pinned and loaded only on the Remote desktop tab. It speaks RDP itself, CredSSP included, and needs only a proxy that speaks its RDCleanPath protocol, which the central server does with `ironrdp-rdcleanpath` 0.3.1, a small crate whose one dependency is `der`. Nothing else runs anywhere.
- **A proxy on the central server to an HTML5 client**: an RDP client on the server (FreeRDP, or IronRDP's own crates) decoding the screen and sending pictures to a page of our own. The server would decode and encode every frame of every session, and we would write the page's protocol.
- **Guacamole**: `guacd`, a daemon in C, and its Java web application or a client of our own speaking its protocol. A separate daemon to package in the images, the installers and the compose files, and a second remote access system with its own configuration. Not simpler.

**Consent.** **`[remote] desktop`** in `agent.toml`, off by default, separate from `[remote] terminal`: a desktop gives a whole Windows session. Only the host's administrator sets it, with the MSI property `REMOTE_DESKTOP=1` (`0` takes it back) or `monitorflow-agent.exe remote-access desktop on|off`, which edit the file as for terminals, then a restart of the agent. No environment variable sets it. "Allow a remote desktop" on the Add servers page, unticked, adds the property to the Windows command. The agent reports the command kind **`remote_desktop`** in its `Hello`: `accepted` or `off` on Windows, `unsupported` on Linux, whatever the file says.

**The command and the address.** A desktop is a session of the kind `desktop`, opened by a signed `remote_desktop` command whose payload is the same `RemoteSessionOpen`. The agent's handler of each command kind accepts only its own kind, so a terminal's consent never opens a desktop, nor the other way round. The command carries no address: the agent connects to `127.0.0.1:3389`, a constant, and the code that connects refuses any address that is not loopback (tests give it a fake service's port, never anything else). A central server that is taken over cannot use the agent to reach another port or another machine. The same keys, frames, limits and banner as terminals follow; the size fields are 0.

**The proxy.** The browser opens the session's WebSocket and sends an RDCleanPath request: the one-time token as `proxy_auth`, and the client's X.224 connection request; its `destination` and any preconnection blob are ignored or refused. The instance holding the WebSocket attaches the session with that token (a desktop's token never attaches a terminal), runs it as for terminals, and, through the sealed frames, sends the X.224 request to the service, reads its confirm, and does the TLS handshake with it, TLS 1.2 or 1.3 with `rustls`. It answers the browser with the confirm and the service's certificates, which IronRDP binds its network level authentication to, or with an RDCleanPath error (the service's refusal of the security asked for, an HTTP status otherwise). From then on the browser sends and receives RDP in the clear on its WebSocket, itself HTTPS, and the instance carries it in and out of the TLS with the service. The WebSocket closes with why the session ended as its reason, and the page reads the session's `close_reason` for it.

The service's certificate is not checked against an authority: Windows makes its own, and the tunnel already proves the host, since only the agent's certificate key opens the session's keys and the agent reaches only its own loopback. The handshake's signatures are checked. Windows Server 2016 or newer, or Windows 10 or newer, is needed for the TLS suites `rustls` offers.

**Flow control.** A screen sends far more than a terminal, and a frame lost on the way ends a session. Desktops send up to 2 MiB a second, and the agent keeps at most `DESKTOP_WINDOW`, 128 output frames (512 KiB), beyond what the server said it passed on to the browser: a new body of `RemoteData`, `delivered`, sent by the server every 32 frames. A slow browser slows the reading of the service's socket, and so the service, instead of filling the inboxes on the way.

**Windows passwords.** The person types an account of the server and its password in the page, which gives them to the WebAssembly client only. They never reach MonitorFlow's API or database, and are not kept in the browser. The client runs CredSSP with Windows through the proxy; the central server relays that exchange inside the TLS it terminates. As for terminals, a central server that is taken over can act inside a session it relays, and could try to guess a weak password from the NTLM exchange: that is why the consent is the host's.

**Same roles, audit and idle time.** No setting of its own: `remote_access_settings.min_role` decides who may open a desktop, as a terminal, and the site's administrators see the sessions and close anyone's. Each desktop is a row of `remote_sessions` with `kind` `desktop`, and the same audit entries with `kind` `desktop` in their details: `remote_session.start`, `.open`, `.close` with its duration, `.close_request`. The idle time is the same, counted from the last bytes the browser sent; the 12 hours too. People logged in on the host are told with `msg *` who opened it.

**Not recorded.** A desktop's row was always `recorded = false`, whatever `record` said (a check of the table held it): recording a screen is video, kept for later if asked. ADR-0192 records them since: the central server decodes the screen from the RDP it carries and keeps pictures of it.

**In the browser.** A Remote desktop tab on Windows servers, for the same people as the Terminal tab: the account, password and domain on one line, the desktop scaled to fit the page, Ctrl+Alt+Del and Close, then the server's past desktop sessions. The interface's script policy gains `'wasm-unsafe-eval'` (it allows WebAssembly to compile, not JavaScript's `eval`), in the proxy's header and in each page's `<meta>` (ADR-0124), and `connect-src` gains `data:`, as the module reads its own inlined WebAssembly with `fetch` on a `data:` URL, which reaches no network.

### Rejected

- **A port opened on the host, or SSH through the relay.** The roadmap says no port to open, and SSH needs keys or passwords on every host and a client in the browser.
- **Frames in the clear, trusting the mTLS of each link.** Each relay terminates TLS: a compromised one could type in any session of its site.
- **A session key inside the signed command.** The command is signed, not encrypted: relays read it.
- **One WebSocket instance per session chosen by a redirect.** Instances share one name; a browser cannot pick one (ADR-0089).
- **Authenticating the WebSocket with the session cookie.** Browsers send cookies on cross-site WebSockets; a one-time token from a request the CSRF guard protects is safe, and works the same for API keys.
- **Keeping the session inside the command's handler.** The dispatcher runs one command at a time: a terminal would hold updates back for hours. The command only opens the session.
- **Another address or port for the desktop, given by the central server.** It would turn the agent into a way into any machine of the site. A service on another port is not supported.
- **The desktop's TLS in the browser, the central server passing bytes only.** IronRDP's web client does not do TLS itself, and the tunnel already authenticates the host; RDCleanPath is the protocol it speaks.
- **Roles and an idle time of their own for desktops.** The host's consent is already apart; one setting for who may reach a server is easier to keep right.

## Consequences

- Administrators open a terminal on a server from its page, through relays, where its host allowed it, without a port or a tool, and each session is audited and, by default, recorded.
- A central server that is taken over can open terminals where hosts allowed them, as the account each host chose: that is what the consent allows, and the Add servers page says so. It cannot where they did not, and cannot choose another account.
- A relay, compromised or not, sees only sealed frames.
- `servers.command_kinds` gains `remote_session`. Agents older than this decision show no Terminal tab; they update by themselves. Their relays need no change beyond being of a version that knows the envelope variant.
- A session waits for the agent's command queue: a terminal asked while a long command runs, an update for example, opens when it ends, or fails after two minutes.
- `servers.command_kinds` gains `remote_desktop`. Windows agents older than it show the tab with "cannot open remote desktops yet"; they update by themselves. Linux agents report it unsupported.
- Administrators reach a Windows server's desktop from its page without opening port 3389 anywhere, with a client of a few MB loaded only on that tab, and nothing to run on the central server besides itself.
- Still to come: the clipboard and file transfers of IronRDP's client, and Windows terminals as another account. Recording desktops and a test against a real Windows host in CI came with ADR-0192.
- Migrations: `20261220000000_remote_sessions` (the settings, the sessions and their recordings), `20270104000000_remote_desktop` (the kind `desktop`, never recorded).
