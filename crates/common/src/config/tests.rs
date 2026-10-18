use super::*;

const AGENT_MIN: &str = r#"upstream_url = "https://relay:7443""#;
const SERVER_MIN: &str = r#"database_url = "postgres://mf:hunter2@db:5432/mf""#;

fn path() -> &'static Path {
    Path::new("/etc/monitorflow/test.toml")
}

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn parse<T: LoadConfig>(text: &str) -> Result<T, ConfigError> {
    parse_with_env(text, path(), &Env::new())
}

fn err<T: LoadConfig + fmt::Debug>(text: &str, vars: &[(&str, &str)]) -> String {
    parse_with_env::<T>(text, path(), &env(vars))
        .unwrap_err()
        .to_string()
}

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/config")
        .join(name)
}

#[test]
fn example_files_parse() {
    let env = Env::new();
    let agent: AgentConfig = load_with_env(&example("agent.example.toml"), &env).unwrap();
    assert_eq!(agent.upstream_url, "https://relay.example.com:7443");
    let relay: RelayConfig = load_with_env(&example("relay.example.toml"), &env).unwrap();
    assert_eq!(relay.upstream_url, "https://monitorflow.example.com:7443");
    let server: ServerConfig = load_with_env(&example("server.example.toml"), &env).unwrap();
    assert!(server.database_url.expose().starts_with("postgres://"));
}

#[test]
fn defaults_apply() {
    let agent: AgentConfig = parse(AGENT_MIN).unwrap();
    assert_eq!(agent.data_dir, Path::new("/var/lib/monitorflow/agent"));
    assert_eq!(agent.heartbeat_interval, 30);
    assert_eq!(agent.enrollment_token, None);
    assert_eq!(agent.log, LogConfig::default());
    assert_eq!(agent.log.level, LogLevel::Info);
    assert_eq!(agent.log.format, LogFormat::Text);
    assert_eq!(agent.tls.ca_cert, Path::new("/etc/monitorflow/tls/ca.pem"));

    let relay: RelayConfig = parse(AGENT_MIN).unwrap();
    assert_eq!(relay.listen_addr, "0.0.0.0:7443".parse().unwrap());
    assert_eq!(relay.data_dir, Path::new("/var/lib/monitorflow/relay"));

    let server: ServerConfig = parse(SERVER_MIN).unwrap();
    assert_eq!(server.grpc_listen_addr.port(), 7443);
    assert_eq!(server.api_listen_addr.port(), 7444);
    assert_eq!(server.enroll_listen_addr, "0.0.0.0:7445".parse().unwrap());
    assert_eq!(server.victoriametrics_url, "http://localhost:8428");
    assert_eq!(server.data_dir, Path::new("/var/lib/monitorflow/server"));
    assert_eq!(server.heartbeat_timeout, 3 * agent.heartbeat_interval);
}

#[test]
fn server_names_default_and_override() {
    let server: ServerConfig = parse(SERVER_MIN).unwrap();
    assert_eq!(server.server_names, ["localhost", "127.0.0.1"]);
    let vars = env(&[("MONITORFLOW_SERVER_NAMES", "mf.example.com, 10.0.0.5")]);
    let server: ServerConfig = parse_with_env(SERVER_MIN, path(), &vars).unwrap();
    assert_eq!(server.server_names, ["mf.example.com", "10.0.0.5"]);
    let text = format!("server_names = []\n{SERVER_MIN}");
    let err = parse::<ServerConfig>(&text).unwrap_err().to_string();
    assert!(err.contains("server_names"), "{err}");
}

#[test]
fn enroll_listen_addr_has_an_env_override() {
    let vars = env(&[("MONITORFLOW_ENROLL_LISTEN_ADDR", "127.0.0.1:9445")]);
    let server: ServerConfig = parse_with_env(SERVER_MIN, path(), &vars).unwrap();
    assert_eq!(server.enroll_listen_addr, "127.0.0.1:9445".parse().unwrap());
}

#[test]
fn env_beats_file() {
    let text = format!("{AGENT_MIN}\nheartbeat_interval = 10\n[log]\nlevel = \"warn\"\n");
    let vars = env(&[
        ("MONITORFLOW_HEARTBEAT_INTERVAL", "5"),
        ("MONITORFLOW_LOG_LEVEL", "debug"),
        ("MONITORFLOW_ENROLLMENT_TOKEN", "tok"),
        ("MONITORFLOW_TLS_CERT", "/tmp/cert.pem"),
    ]);
    let agent: AgentConfig = parse_with_env(&text, path(), &vars).unwrap();
    assert_eq!(agent.heartbeat_interval, 5);
    assert_eq!(agent.log.level, LogLevel::Debug);
    assert_eq!(agent.enrollment_token, Some(Secret::new("tok")));
    assert_eq!(agent.tls.cert, Path::new("/tmp/cert.pem"));
}

#[test]
fn env_fills_required_key_and_empty_env_is_ignored() {
    let vars = env(&[
        ("MONITORFLOW_DATABASE_URL", "postgres://db/mf"),
        ("MONITORFLOW_LOG_FORMAT", ""),
    ]);
    let server: ServerConfig = parse_with_env("", path(), &vars).unwrap();
    assert_eq!(server.database_url.expose(), "postgres://db/mf");
    assert_eq!(server.log.format, LogFormat::Text);
}

#[test]
fn bad_env_value_names_the_variable() {
    let msg = err::<AgentConfig>(AGENT_MIN, &[("MONITORFLOW_HEARTBEAT_INTERVAL", "soon")]);
    assert!(msg.contains("MONITORFLOW_HEARTBEAT_INTERVAL"), "{msg}");
    assert!(msg.contains("`heartbeat_interval`"), "{msg}");
}

#[test]
fn unknown_key_is_rejected() {
    let msg = err::<AgentConfig>(&format!("{AGENT_MIN}\nheartbeat_intervall = 5"), &[]);
    assert!(msg.contains("test.toml"), "{msg}");
    assert!(msg.contains("line 2"), "{msg}");
    assert!(msg.contains("unknown field `heartbeat_intervall`"), "{msg}");

    let msg = err::<AgentConfig>(&format!("{AGENT_MIN}\n[log]\nlevle = \"info\""), &[]);
    assert!(msg.contains("key `log.levle`"), "{msg}");
}

#[test]
fn upstream_must_be_present_and_https() {
    let msg = err::<AgentConfig>("", &[]);
    assert!(msg.contains("key `upstream_url`: is required"), "{msg}");
    assert!(msg.contains("MONITORFLOW_UPSTREAM_URL"), "{msg}");

    for url in [
        "http://relay:7443",
        "relay:7443",
        "https://",
        "not a url",
        "https://relay:7443/prefix",
    ] {
        let msg = err::<RelayConfig>(&format!("upstream_url = \"{url}\""), &[]);
        assert!(
            msg.contains("key `upstream_url`: must be an https URL"),
            "{msg}"
        );
    }
}

#[test]
fn database_url_must_be_postgres_without_leaking_it() {
    let msg = err::<ServerConfig>(r#"database_url = "mysql://mf:hunter2@db/mf""#, &[]);
    assert!(
        msg.contains("key `database_url`: must start with postgres://"),
        "{msg}"
    );
    assert!(!msg.contains("hunter2"), "{msg}");

    let msg = err::<ServerConfig>("", &[]);
    assert!(msg.contains("key `database_url`: is required"), "{msg}");
}

#[test]
fn storage_urls_accept_http_but_not_other_schemes() {
    let text = format!("{SERVER_MIN}\nvictorialogs_url = \"https://logs:9428\"");
    assert!(parse::<ServerConfig>(&text).is_ok());
    let msg = err::<ServerConfig>(
        &format!("{SERVER_MIN}\nvictoriametrics_url = \"ftp://vm\""),
        &[],
    );
    assert!(
        msg.contains("key `victoriametrics_url`: must be an http or https URL"),
        "{msg}"
    );
}

#[test]
fn intervals_and_sizes_must_be_positive() {
    let msg = err::<AgentConfig>(&format!("{AGENT_MIN}\nheartbeat_interval = 0"), &[]);
    assert!(
        msg.contains("key `heartbeat_interval`: must be greater than zero"),
        "{msg}"
    );
    let msg = err::<RelayConfig>(&format!("{AGENT_MIN}\nbuffer_max_bytes = 0"), &[]);
    assert!(
        msg.contains("key `buffer_max_bytes`: must be greater than zero"),
        "{msg}"
    );
    let msg = err::<ServerConfig>(SERVER_MIN, &[("MONITORFLOW_HEARTBEAT_TIMEOUT", "0")]);
    assert!(
        msg.contains("key `heartbeat_timeout`: must be greater than zero"),
        "{msg}"
    );
}

#[test]
fn heartbeat_timeout_can_be_overridden() {
    let text = format!("{SERVER_MIN}\nheartbeat_timeout = 60");
    let server: ServerConfig = parse(&text).unwrap();
    assert_eq!(server.heartbeat_timeout, 60);
    let vars = env(&[("MONITORFLOW_HEARTBEAT_TIMEOUT", "45")]);
    let server: ServerConfig = parse_with_env(&text, path(), &vars).unwrap();
    assert_eq!(server.heartbeat_timeout, 45);
}

#[test]
fn addresses_must_be_socket_addresses() {
    let msg = err::<ServerConfig>(
        &format!("{SERVER_MIN}\napi_listen_addr = \"localhost\""),
        &[],
    );
    assert!(msg.contains("line 2, key `api_listen_addr`"), "{msg}");
    assert!(msg.contains("socket address"), "{msg}");

    let msg = err::<RelayConfig>(AGENT_MIN, &[("MONITORFLOW_LISTEN_ADDR", "7443")]);
    assert!(msg.contains("MONITORFLOW_LISTEN_ADDR"), "{msg}");
}

#[test]
fn log_level_and_format_are_checked() {
    let msg = err::<AgentConfig>(&format!("{AGENT_MIN}\n[log]\nformat = \"xml\""), &[]);
    assert!(msg.contains("key `log.format`"), "{msg}");
    let msg = err::<AgentConfig>(AGENT_MIN, &[("MONITORFLOW_LOG_LEVEL", "loud")]);
    assert!(msg.contains("unknown log level `loud`"), "{msg}");
}

#[test]
fn missing_file_names_the_path() {
    let missing = Path::new("/nonexistent/monitorflow.toml");
    let msg = load_with_env::<AgentConfig>(missing, &Env::new())
        .unwrap_err()
        .to_string();
    assert!(msg.contains("/nonexistent/monitorflow.toml"), "{msg}");
}

#[test]
fn debug_output_redacts_secrets() {
    let vars = env(&[("MONITORFLOW_ENROLLMENT_TOKEN", "s3cret-token")]);
    let agent: AgentConfig = parse_with_env(AGENT_MIN, path(), &vars).unwrap();
    let out = format!("{agent:?}");
    assert!(!out.contains("s3cret-token"), "{out}");
    assert!(out.contains("[redacted]"), "{out}");

    let server: ServerConfig = parse(SERVER_MIN).unwrap();
    let out = format!("{server:?}");
    assert!(!out.contains("hunter2"), "{out}");
    assert!(out.contains("postgres://mf:[redacted]@db:5432/mf"), "{out}");
}

#[test]
fn parse_errors_do_not_echo_secret_lines() {
    let text = "enrollment_token = \"s3cret-token\" oops\n";
    let msg = err::<AgentConfig>(text, &[]);
    assert!(!msg.contains("s3cret-token"), "{msg}");
    assert!(msg.contains("line 1"), "{msg}");
}
