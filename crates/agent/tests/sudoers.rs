//! `deploy/sudoers/monitorflow-agent` allows exactly the privileged command
//! lines the agent runs (ADR-0031, ADR-0032, ADR-0035, ADR-0036), with the program
//! paths of [`Tools::default`], no wildcard, and parses with `visudo -cf`
//! where `visudo` is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use monitorflow_agent::packages::Tools;

const SUDOERS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../deploy/sudoers/monitorflow-agent"
);

/// The commands of every `Cmnd_Alias`, unescaped.
fn allowed() -> Vec<String> {
    let text = std::fs::read_to_string(SUDOERS).unwrap();
    let joined = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("\\\n", " ");
    joined
        .lines()
        .filter_map(|l| l.strip_prefix("Cmnd_Alias "))
        .flat_map(|l| {
            let (_, commands) = l.split_once('=').unwrap();
            // Separators are unescaped commas; escaped ones do not occur.
            commands
                .split(", ")
                .map(|c| {
                    c.split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .replace("\\:", ":")
                        .replace("\\=", "=")
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_rules_are_the_agents_exact_command_lines() {
    let tools = Tools::default();
    let agent = tools.agent.display();
    let expected = [
        format!("{agent} packages update"),
        format!("{} -r +1", tools.shutdown.display()),
        format!("{agent} sources apply"),
        format!("{agent} sources revert"),
    ];
    assert_eq!(allowed(), expected);
    assert_eq!(tools.agent, Path::new("/usr/bin/monitorflow-agent"));

    let text = std::fs::read_to_string(SUDOERS).unwrap();
    let rules: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    assert!(
        !rules.iter().any(|l| l.contains(['*', '?', '['])),
        "a wildcard"
    );
    // The helper sets the package managers' environment itself.
    assert!(!rules.iter().any(|l| l.starts_with("Defaults")));
    assert!(rules
        .iter()
        .any(|l| l.starts_with("monitorflow ALL=(root) NOPASSWD: ")));
}

fn visudo() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/sbin".into(), "/sbin".into()])
        .map(|dir| dir.join("visudo"))
        .find(|p| p.is_file())
}

#[test]
fn visudo_accepts_the_file() {
    let Some(visudo) = visudo() else {
        eprintln!("visudo not installed; syntax not checked");
        return;
    };
    let output = Command::new(visudo)
        .args(["-c", "-f", SUDOERS])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
