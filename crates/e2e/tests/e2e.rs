//! End-to-end test of the real `monitorflow-server` and `monitorflow-agent`
//! binaries, driven as an operator would: config files written from
//! scratch, the server's CLI subcommands, the agent enrolling with a token,
//! and the REST API. It catches wiring regressions in `main.rs`, config
//! loading and defaults that the in-process tests cannot see.
//!
//! The binaries are built by `cargo build -p monitorflow-server -p
//! monitorflow-agent --bins` from the test, with the cargo, target directory
//! and profile that built the test (see [`support::build_binaries`]). Cargo
//! only exposes `CARGO_BIN_EXE_*` to tests of the package that owns the
//! binary, and artifact dependencies are unstable; building from the test
//! keeps `cargo test -p monitorflow-e2e` working on its own and costs nothing
//! under `cargo test --workspace`, which has already built both binaries.
//!
//! Needs `DATABASE_URL` at build time (see `build.rs`); the test creates its
//! own database on that server and drops it at the end. On a host whose
//! distribution the agent refuses, the test prints why and passes without
//! running: the agent has no os-release override.

mod support;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use support::{build_binaries, free_addr, http, unique, wait_for, Database, Logs, Process};

/// Short enough to keep the test fast; the agent heartbeats every second.
const HEARTBEAT_TIMEOUT_SECS: u64 = 3;
const AGENT_ID: &str = "e2e-01";
const SITE: &str = "e2e";
const ADMIN: &str = "e2e-admin";

/// A command for one of the binaries that ignores any `MONITORFLOW_*`
/// variable of the developer's environment and logs without colors.
fn command(binary: &Path, config: &Path) -> Command {
    let mut command = Command::new(binary);
    command.arg("--config").arg(config).env("NO_COLOR", "1");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("MONITORFLOW_") {
            command.env_remove(key);
        }
    }
    command
}

/// The server's entry for `AGENT_ID` once `ready` accepts it.
fn wait_for_agent(
    api: std::net::SocketAddr,
    cookie: &str,
    what: &str,
    timeout: Duration,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    wait_for(what, timeout, || {
        let response = http(api, "GET", "/api/servers", Some(cookie), None).ok()?;
        assert_eq!(response.status, 200, "GET /api/servers: {}", response.body);
        let servers = response.json()["servers"].as_array()?.clone();
        servers
            .into_iter()
            .find(|s| s["peer_id"] == AGENT_ID)
            .filter(&ready)
    })
}

#[test]
#[cfg_attr(
    not(db_tests),
    ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
)]
fn enroll_agent_and_list_it() {
    let host = match monitorflow_agent::os::detect() {
        Ok(host) => host,
        Err(error) => {
            eprintln!("SKIPPED: the agent refuses this host ({error}); run on a debian or rhel family distribution");
            return;
        }
    };
    let binaries = build_binaries();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL at run time");

    // Dropped in reverse order: processes first, then logs, then database.
    let database = Database::create(&database_url);
    let dir = tempfile::tempdir().expect("temp dir");
    let logs = Logs::default();
    let password = format!("pw-{}", unique());

    let (grpc, enroll, api) = (free_addr(), free_addr(), free_addr());
    let server_dir = dir.path().join("server");
    let server_config = dir.path().join("server.toml");
    std::fs::write(
        &server_config,
        format!(
            r#"grpc_listen_addr = "{grpc}"
enroll_listen_addr = "{enroll}"
api_listen_addr = "{api}"
api_cookie_secure = false
database_url = "{db}"
data_dir = "{data}"
heartbeat_timeout = {HEARTBEAT_TIMEOUT_SECS}
server_names = ["127.0.0.1"]

[tls]
ca_cert = "{data}/tls/ca.pem"
cert = "{data}/tls/cert.pem"
key = "{data}/tls/key.pem"
"#,
            db = database.url,
            data = server_dir.display(),
        ),
    )
    .unwrap();

    let mut server = Process::spawn(
        "server",
        command(&binaries.server, &server_config),
        &dir.path().join("server.log"),
        &logs,
    );
    wait_for("server readiness", Duration::from_secs(30), || {
        server.assert_running();
        let response = http(api, "GET", "/api/health/ready", None, None).ok()?;
        (response.status == 200).then_some(())
    });
    let health = http(api, "GET", "/api/health", None, None).unwrap();
    assert_eq!(health.json()["status"], "ok");

    // `user add` reads the password from stdin when it is not a terminal.
    let mut add = command(&binaries.server, &server_config)
        .args(["user", "add", "--username", ADMIN, "--role", "admin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("starting user add");
    use std::io::Write;
    writeln!(add.stdin.take().unwrap(), "{password}").unwrap();
    let added = add.wait_with_output().unwrap();
    let user_log = dir.path().join("user-add.log");
    std::fs::write(&user_log, [added.stdout, added.stderr].concat()).unwrap();
    logs.add("user add", &user_log);
    assert!(added.status.success(), "user add failed: {}", added.status);

    let created = command(&binaries.server, &server_config)
        .args(["token", "create", "--role", "agent", "--id", AGENT_ID])
        .args(["--site", SITE, "--create-site"])
        .output()
        .expect("running token create");
    let token_log = dir.path().join("token-create.log");
    std::fs::write(&token_log, &created.stderr).unwrap();
    logs.add("token create (stderr)", &token_log);
    assert!(
        created.status.success(),
        "token create failed: {}",
        created.status
    );
    let token = String::from_utf8(created.stdout).unwrap().trim().to_owned();
    let secret = token
        .split('.')
        .nth(1)
        .expect("token is mfe1.<secret>.<ca>");
    assert!(
        token.starts_with("mfe1.") && secret.len() >= 40,
        "odd token"
    );

    let agent_dir = dir.path().join("agent");
    let agent_config = dir.path().join("agent.toml");
    // The heartbeat interval is not in the brief's minimal config, but the
    // default 30 s would trip the short server timeout above.
    let agent_toml = |token: Option<&str>| {
        let mut toml = format!(
            "upstream_url = \"https://{grpc}\"\nenroll_url = \"https://{enroll}\"\ndata_dir = \"{}\"\nheartbeat_interval = 1\n",
            agent_dir.display()
        );
        if let Some(token) = token {
            toml.push_str(&format!("enrollment_token = \"{token}\"\n"));
        }
        toml
    };
    std::fs::write(&agent_config, agent_toml(Some(&token))).unwrap();
    let mut agent = Process::spawn(
        "agent",
        command(&binaries.agent, &agent_config),
        &dir.path().join("agent-1.log"),
        &logs,
    );

    let login = http(
        api,
        "POST",
        "/api/auth/login",
        None,
        Some(&serde_json::json!({"username": ADMIN, "password": password}).to_string()),
    )
    .unwrap();
    assert_eq!(login.status, 200, "login: {}", login.body);
    let set_cookie = login.header("set-cookie").expect("session cookie");
    assert!(
        set_cookie.starts_with("mf_session=") && !set_cookie.contains("Secure"),
        "insecure-mode cookie expected, got {set_cookie}"
    );
    let cookie = set_cookie.split(';').next().unwrap().to_owned();

    let timeout = Duration::from_secs(20);
    let online = wait_for_agent(api, &cookie, "agent online", timeout, |s| {
        s["status"] == "online"
    });
    agent.assert_running();
    assert_eq!(online["hostname"], host.hostname.as_str());
    assert_eq!(online["os_family"], host.os_family.as_str());
    assert_eq!(online["site_name"], SITE);
    assert!(
        agent_dir.join("tls/cert.pem").is_file(),
        "identity not stored"
    );

    // A clean stop closes the stream; the heartbeat timeout bounds the rest.
    assert!(agent.terminate(Duration::from_secs(10)).success());
    let offline_within = Duration::from_secs(HEARTBEAT_TIMEOUT_SECS + 5);
    wait_for_agent(api, &cookie, "agent offline", offline_within, |s| {
        s["status"] == "offline"
    });

    // Without a token the agent can only come back with its stored identity.
    std::fs::write(&agent_config, agent_toml(None)).unwrap();
    let mut agent = Process::spawn(
        "agent (restart)",
        command(&binaries.agent, &agent_config),
        &dir.path().join("agent-2.log"),
        &logs,
    );
    let back = wait_for_agent(api, &cookie, "agent back online", timeout, |s| {
        s["status"] == "online"
    });
    assert_eq!(back["id"], online["id"], "the same server row is reused");
    assert_ne!(back["connected_at"], online["connected_at"]);

    assert!(server.terminate(Duration::from_secs(10)).success());
    assert!(agent.terminate(Duration::from_secs(10)).success());

    let all = logs.all();
    assert!(!all.contains(secret), "a log contains the token secret");
    assert!(!all.contains(&password), "a log contains the password");
}
