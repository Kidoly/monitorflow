//! The Windows defaults (ADR-0047); the other tests use Unix paths.

use super::*;

#[test]
fn agent_defaults_live_under_program_data() {
    let base = windows_base_dir();
    assert!(
        base.is_absolute() && base.ends_with("MonitorFlow"),
        "{base:?}"
    );
    let text = r#"upstream_url = "https://relay:7443""#;
    let agent: AgentConfig = parse_with_env(text, Path::new(r"C:\a.toml"), &Env::new()).unwrap();
    assert_eq!(agent.data_dir, base.join("agent"));
    assert_eq!(agent.tls_paths().key, base.join(r"agent\tls\key.pem"));
    assert_eq!(agent.sensors.script_dir, base.join("scripts.d"));
    assert!(agent.sensors.log_dirs.is_empty());
}

#[test]
fn windows_example_parses() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/config/agent.windows.example.toml");
    let agent: AgentConfig = load_with_env(&path, &Env::new()).unwrap();
    assert_eq!(agent.upstream_url, "https://relay.example.com:7443");
    assert_eq!(
        agent.data_dir,
        Path::new(r"C:\ProgramData\MonitorFlow\agent")
    );
    assert_eq!(
        agent.sensors.script_dir,
        Path::new(r"C:\ProgramData\MonitorFlow\scripts.d")
    );
}
