//! CPU and memory of the cgroup that holds the server side of a run, and
//! of each cgroup in it: the server's scope and the stores' containers,
//! when `scripts/load-test.sh` puts them together in one slice limited
//! like a small machine (`LOADTEST_SERVER_CPUS`, `LOADTEST_SERVER_MEMORY`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The name of the whole slice in the report.
pub const WHOLE: &str = "Server side in all";

/// One reading of one cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub name: String,
    /// CPU time used since it started, in microseconds.
    pub cpu_usec: u64,
    pub memory_bytes: u64,
}

/// `usage_usec` of a `cpu.stat`.
pub fn parse_usage(cpu_stat: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()
}

/// A counter of a `memory.events`, like `oom_kill`.
pub fn parse_event(events: &str, name: &str) -> u64 {
    events
        .lines()
        .find_map(|l| {
            let (key, value) = l.split_once(' ')?;
            (key == name).then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// What people call the cgroup `dir_name` of the slice: `server` for the
/// server's scope, the container's name for a container's.
pub fn name_of(dir_name: &str, containers: &HashMap<String, String>) -> String {
    if dir_name.ends_with("-server.scope") {
        return "Server".to_owned();
    }
    if let Some(id) = dir_name
        .strip_prefix("docker-")
        .and_then(|d| d.strip_suffix(".scope"))
    {
        if let Some(name) = containers.get(id) {
            return name.clone();
        }
        return format!("container {}", &id[..id.len().min(12)]);
    }
    dir_name.to_owned()
}

/// Watches the slice `dir` and the cgroups in it.
pub struct Slice {
    dir: PathBuf,
    /// Container ID to its compose service, like `postgres`.
    containers: HashMap<String, String>,
}

impl Slice {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            containers: HashMap::new(),
        }
    }

    fn children(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    /// Names the containers not seen yet, from Docker.
    async fn learn(&mut self) {
        for dir in self.children() {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Some(id) = name
                .strip_prefix("docker-")
                .and_then(|n| n.strip_suffix(".scope"))
            else {
                continue;
            };
            if self.containers.contains_key(id) {
                continue;
            }
            let output = tokio::process::Command::new("docker")
                .args([
                    "inspect",
                    "--format",
                    "{{index .Config.Labels \"com.docker.compose.service\"}}",
                    id,
                ])
                .output()
                .await;
            if let Ok(output) = output {
                let service = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !service.is_empty() {
                    self.containers.insert(id.to_owned(), service);
                }
            }
        }
    }

    fn read_one(&self, dir: &Path, name: String) -> Option<Reading> {
        let cpu = parse_usage(&std::fs::read_to_string(dir.join("cpu.stat")).ok()?)?;
        let memory = std::fs::read_to_string(dir.join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        Some(Reading {
            name,
            cpu_usec: cpu,
            memory_bytes: memory,
        })
    }

    /// The slice in all, then each cgroup in it.
    pub async fn read(&mut self) -> Vec<Reading> {
        self.learn().await;
        let mut readings: Vec<Reading> = self
            .read_one(&self.dir, WHOLE.to_owned())
            .into_iter()
            .collect();
        for dir in self.children() {
            let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let name = name_of(dir_name, &self.containers);
            readings.extend(self.read_one(&dir, name));
        }
        readings
    }

    /// Processes the kernel killed for memory in the slice, its peak
    /// memory, and swap it used now.
    pub fn memory_summary(&self) -> (u64, u64, u64) {
        let read = |file: &str| std::fs::read_to_string(self.dir.join(file)).unwrap_or_default();
        let number = |file: &str| read(file).trim().parse().unwrap_or(0);
        (
            parse_event(&read("memory.events"), "oom_kill"),
            number("memory.peak"),
            number("memory.swap.current"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_memory_files_are_read() {
        let stat = "usage_usec 1234567\nuser_usec 1000000\nsystem_usec 234567\n";
        assert_eq!(parse_usage(stat), Some(1_234_567));
        assert_eq!(parse_usage("user_usec 3\n"), None);
        let events = "low 0\nhigh 0\nmax 12\noom 2\noom_kill 1\n";
        assert_eq!(parse_event(events, "oom_kill"), 1);
        assert_eq!(parse_event(events, "oom"), 2);
        assert_eq!(parse_event(events, "absent"), 0);
    }

    #[test]
    fn cgroups_are_named_for_people() {
        let containers = HashMap::from([("abc123def456789".to_owned(), "postgres".to_owned())]);
        assert_eq!(name_of("mfloadtest-server.scope", &containers), "Server");
        assert_eq!(
            name_of("docker-abc123def456789.scope", &containers),
            "postgres"
        );
        assert_eq!(
            name_of("docker-0123456789abcdef.scope", &containers),
            "container 0123456789ab"
        );
        assert_eq!(name_of("other.scope", &containers), "other.scope");
    }

    #[tokio::test]
    async fn a_slice_reads_itself_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &Path, cpu: u64, memory: u64| {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("cpu.stat"), format!("usage_usec {cpu}\n")).unwrap();
            std::fs::write(path.join("memory.current"), format!("{memory}\n")).unwrap();
        };
        write(dir.path(), 300, 3_000);
        write(&dir.path().join("mfloadtest-server.scope"), 100, 1_000);
        std::fs::write(dir.path().join("memory.events"), "oom_kill 2\n").unwrap();
        std::fs::write(dir.path().join("memory.peak"), "4096\n").unwrap();
        let mut slice = Slice::new(dir.path().to_owned());
        let readings = slice.read().await;
        assert_eq!(
            readings,
            [
                Reading {
                    name: WHOLE.into(),
                    cpu_usec: 300,
                    memory_bytes: 3_000
                },
                Reading {
                    name: "Server".into(),
                    cpu_usec: 100,
                    memory_bytes: 1_000
                },
            ]
        );
        assert_eq!(slice.memory_summary(), (2, 4096, 0));
    }
}
