//! Helpers for the end-to-end test: building the binaries, child processes
//! that die with the test, a throwaway database and a tiny HTTP client.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Paths of the workspace binaries under test.
pub struct Binaries {
    pub server: PathBuf,
    pub agent: PathBuf,
    pub relay: PathBuf,
}

/// Builds `monitorflow-server`, `monitorflow-agent` and `monitorflow-relay`
/// with the cargo, target
/// directory and profile that built this test, and returns the executables
/// cargo reports. A no-op when `cargo test --workspace` already built them.
pub fn build_binaries() -> Binaries {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    // `CARGO_TARGET_TMPDIR` is `<target dir>/tmp`, whatever `--target-dir`
    // or `CARGO_TARGET_DIR` said.
    let target_dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("target tmp dir has a parent");
    // This test runs from `<target dir>[/<triple>]/<profile dir>/deps/`.
    let exe = std::env::current_exe().expect("test executable path");
    let profile = match exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
    {
        Some(dir) if dir == "debug" => "dev".to_owned(),
        Some(dir) => dir.to_string_lossy().into_owned(),
        None => "dev".to_owned(),
    };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "build",
            "--bins",
            "--message-format=json-render-diagnostics",
        ])
        .args(["-p", "monitorflow-server", "-p", "monitorflow-agent"])
        .args(["-p", "monitorflow-relay"])
        .args(["--profile", &profile])
        .arg("--manifest-path")
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", target_dir)
        .stderr(Stdio::inherit())
        .output()
        .expect("running cargo build");
    assert!(
        output.status.success(),
        "cargo build failed: {}",
        output.status
    );
    let (mut server, mut agent, mut relay) = (None, None, None);
    for line in output.stdout.split(|&b| b == b'\n') {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let Some(path) = message["executable"].as_str() else {
            continue;
        };
        match message["target"]["name"].as_str() {
            Some("monitorflow-server") => server = Some(PathBuf::from(path)),
            Some("monitorflow-agent") => agent = Some(PathBuf::from(path)),
            Some("monitorflow-relay") => relay = Some(PathBuf::from(path)),
            _ => {}
        }
    }
    Binaries {
        server: server.expect("cargo reported no monitorflow-server executable"),
        agent: agent.expect("cargo reported no monitorflow-agent executable"),
        relay: relay.expect("cargo reported no monitorflow-relay executable"),
    }
}

/// A loopback address with a port that was free a moment ago.
pub fn free_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("binding an ephemeral port")
}

/// A string unlikely to collide between runs, for names and secrets.
pub fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}_{nanos}", std::process::id())
}

/// Log files of every process started by the test, printed if it fails.
#[derive(Default)]
pub struct Logs(Mutex<Vec<(String, PathBuf)>>);

impl Logs {
    pub fn add(&self, name: &str, path: &Path) {
        self.0
            .lock()
            .unwrap()
            .push((name.to_owned(), path.to_owned()));
    }

    /// Every log, concatenated.
    pub fn all(&self) -> String {
        let mut out = String::new();
        for (name, path) in self.0.lock().unwrap().iter() {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            out.push_str(&format!(
                "===== {name} ({}) =====\n{text}\n",
                path.display()
            ));
        }
        out
    }
}

impl Drop for Logs {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.all());
        }
    }
}

/// A child process killed when dropped, so a failing test leaves nothing
/// running. Stdout and stderr go to one log file.
pub struct Process {
    name: String,
    child: Child,
}

impl Process {
    pub fn spawn(name: &str, mut command: Command, log: &Path, logs: &Logs) -> Self {
        let out = File::create(log).expect("creating log file");
        let err = out.try_clone().expect("cloning log file");
        logs.add(name, log);
        let child = command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .unwrap_or_else(|e| panic!("starting {name}: {e}"));
        Self {
            name: name.to_owned(),
            child,
        }
    }

    /// Panics if the process has already exited.
    pub fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("polling child") {
            panic!("{} exited unexpectedly: {status}", self.name);
        }
    }

    /// Sends SIGTERM and waits for the exit.
    pub fn terminate(&mut self, timeout: Duration) -> ExitStatus {
        self.assert_running();
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .expect("running kill");
        assert!(status.success(), "kill -TERM {} failed", self.name);
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("polling child") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "{} still running {timeout:?} after SIGTERM",
                self.name
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A database created for this run on the server of `DATABASE_URL`, dropped
/// with this value, test failure included.
pub struct Database {
    admin_url: String,
    name: String,
    pub url: String,
}

impl Database {
    pub fn create(admin_url: &str) -> Self {
        let name = format!("mf_e2e_{}", unique());
        let url = with_database(admin_url, &name);
        run_sql(admin_url, &format!("CREATE DATABASE \"{name}\""))
            .unwrap_or_else(|e| panic!("creating database {name}: {e}"));
        Self {
            admin_url: admin_url.to_owned(),
            name,
            url,
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let sql = format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name);
        if let Err(e) = run_sql(&self.admin_url, &sql) {
            eprintln!("dropping database {}: {e}", self.name);
        }
    }
}

fn run_sql(url: &str, sql: &str) -> Result<(), sqlx::Error> {
    use sqlx::{Connection, Executor};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async {
        let mut conn = sqlx::PgConnection::connect(url).await?;
        conn.execute(sql).await?;
        conn.close().await
    })
}

/// `url` with its database name replaced by `name`, query string kept.
fn with_database(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, format!("?{query}")),
        None => (url, String::new()),
    };
    let authority = base.find("://").map_or(0, |i| i + 3);
    let path = base[authority..]
        .find('/')
        .map_or(base.len(), |i| authority + i);
    format!("{}/{name}{query}", &base[..path])
}

/// One HTTP/1.1 exchange, enough for the JSON API.
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("invalid JSON ({e}): {}", self.body))
    }
}

/// Sends a request with the CSRF header the API requires on POST and an
/// optional cookie, on a fresh connection.
pub fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> io::Result<Response> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nX-MonitorFlow-CSRF: 1\r\n"
    );
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    let body = body.unwrap_or_default();
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(request.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other(format!("bad status line {line:?}")))?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let mut response = Response {
        status,
        headers,
        body: String::new(),
    };
    let chunked = response
        .header("transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"));
    if chunked {
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim(), 16).map_err(io::Error::other)?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk)?;
            if size == 0 {
                break;
            }
            response
                .body
                .push_str(&String::from_utf8_lossy(&chunk[..size]));
        }
    } else {
        reader.read_to_string(&mut response.body)?;
    }
    Ok(response)
}

/// Calls `check` every 200 ms until it returns `Some`, or panics after
/// `timeout` with `what`.
pub fn wait_for<T>(what: &str, timeout: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn with_database_replaces_the_name_only() {
    assert_eq!(
        with_database("postgres://u:p@h:5432/monitorflow", "x"),
        "postgres://u:p@h:5432/x"
    );
    assert_eq!(
        with_database("postgres://u:p@h/db?sslmode=disable", "x"),
        "postgres://u:p@h/x?sslmode=disable"
    );
    assert_eq!(with_database("postgres://h", "x"), "postgres://h/x");
}
