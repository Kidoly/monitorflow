use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::http::{Checked, Response};

/// Answers every request with its URL and records it.
#[derive(Default)]
struct FakeHttp {
    seen: Mutex<Vec<String>>,
}

impl Http for FakeHttp {
    fn send(&self, request: Checked, _timeout: Duration) -> Result<Response, String> {
        self.seen.lock().unwrap().push(request.url.to_string());
        Ok(Response {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: format!("hello from {}", request.url),
            elapsed_ms: 12,
        })
    }
}

const IMPORTS_WAT: &str = r#"
    (import "mf" "input_len" (func $len (result i32)))
    (import "mf" "input_read" (func $read (param i32)))
    (import "mf" "output" (func $out (param i32 i32)))
    (import "mf" "log" (func $log (param i32 i32 i32)))
    (import "mf" "http" (func $http (param i32 i32) (result i32)))
    (import "mf" "http_read" (func $http_read (param i32)))
    (memory (export "memory") 1)
"#;

fn plugin(body: &str) -> Plugin {
    let wat = format!("(module {IMPORTS_WAT} {body})");
    Plugin::load(&wat::parse_str(wat).unwrap()).unwrap()
}

fn context(http: Arc<dyn Http>) -> Context {
    Context {
        http,
        rules: vec![HostRule::parse("api.example.com").unwrap()],
        deadline: Instant::now() + Duration::from_secs(5),
        secrets: vec!["hunter22".into()],
        fuel: 10_000_000,
    }
}

fn run(plugin: &Plugin, input: Value) -> Result<Outcome, RunError> {
    plugin.run_blocking(&input, context(Arc::new(FakeHttp::default())))
}

const ECHO: &str = r#"
    (func (export "mf_run") (result i32)
        (call $read (i32.const 0))
        (call $out (i32.const 0) (call $len))
        (i32.const 0))
"#;

#[test]
fn a_plugin_reads_its_input_and_returns_a_result() {
    let input = json!({"settings": {"url": "https://api.example.com"}});
    let outcome = run(&plugin(ECHO), input.clone()).unwrap();
    assert_eq!(outcome.output, input);
    assert!(outcome.logs.is_empty());
}

#[test]
fn http_goes_only_to_listed_hosts() {
    let request = |url: &str| {
        format!(
            r#"(data (i32.const 1024) "{{\"url\": \"{url}\"}}")
            (func (export "mf_run") (result i32) (local $n i32)
                (local.set $n (call $http (i32.const 1024) (i32.const {})))
                (call $http_read (i32.const 4096))
                (call $out (i32.const 4096) (local.get $n))
                (i32.const 0))"#,
            url.len() + 11
        )
    };
    let http = Arc::new(FakeHttp::default());
    let allowed = plugin(&request("https://api.example.com/v1"));
    let outcome = allowed
        .run_blocking(&json!({}), context(http.clone()))
        .unwrap();
    assert_eq!(outcome.output["status"], 200);
    assert_eq!(
        outcome.output["body"],
        "hello from https://api.example.com/v1"
    );
    let refused = plugin(&request("https://evil.example.net/"));
    let outcome = refused
        .run_blocking(&json!({}), context(http.clone()))
        .unwrap();
    assert_eq!(
        outcome.output["error"],
        "evil.example.net is not a host this plugin may connect to"
    );
    assert_eq!(*http.seen.lock().unwrap(), ["https://api.example.com/v1"]);
}

#[test]
fn requests_per_run_are_limited() {
    let body = r#"
        (data (i32.const 1024) "{\"url\": \"https://api.example.com/\"}")
        (func (export "mf_run") (result i32) (local $i i32) (local $n i32)
            (loop $again
                (local.set $n (call $http (i32.const 1024) (i32.const 35)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $again (i32.lt_u (local.get $i) (i32.const 9))))
            (call $http_read (i32.const 4096))
            (call $out (i32.const 4096) (local.get $n))
            (i32.const 0))"#;
    let http = Arc::new(FakeHttp::default());
    let outcome = plugin(body)
        .run_blocking(&json!({}), context(http.clone()))
        .unwrap();
    assert_eq!(outcome.output["error"], "at most 8 requests per run");
    assert_eq!(http.seen.lock().unwrap().len(), 8);
}

#[test]
fn a_failure_reports_its_last_error_without_secrets() {
    let body = r#"
        (data (i32.const 0) "token hunter22 refused")
        (func (export "mf_run") (result i32)
            (call $log (i32.const 0) (i32.const 0) (i32.const 22))
            (i32.const 1))"#;
    assert_eq!(
        run(&plugin(body), json!({})).unwrap_err(),
        RunError::Failed("token [REDACTED] refused".into())
    );
    let silent = r#"(func (export "mf_run") (result i32) (i32.const 3))"#;
    assert_eq!(
        run(&plugin(silent), json!({})).unwrap_err(),
        RunError::Failed("the plugin failed with code 3".into())
    );
}

#[test]
fn logs_are_capped() {
    let body = r#"
        (func (export "mf_run") (result i32) (local $i i32)
            (loop $again
                (call $log (i32.const 3) (i32.const 0) (i32.const 1000))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $again (i32.lt_u (local.get $i) (i32.const 10))))
            (i32.const 0))"#;
    let outcome = run(&plugin(body), json!({})).unwrap();
    let total: usize = outcome.logs.iter().map(|(_, l)| l.len()).sum();
    assert_eq!(total, limits::LOG_BYTES);
}

#[test]
fn cpu_memory_and_bounds_are_enforced() {
    let spin = r#"(func (export "mf_run") (result i32) (loop $l (br $l)) (i32.const 0))"#;
    assert_eq!(
        run(&plugin(spin), json!({})).unwrap_err(),
        RunError::OutOfFuel
    );
    // 33 MiB: more than the limit; grow answers -1 and the plugin says so.
    let grow = r#"(func (export "mf_run") (result i32) (memory.grow (i32.const 528)))"#;
    assert_eq!(
        run(&plugin(grow), json!({})).unwrap_err(),
        RunError::Failed("the plugin failed with code -1".into())
    );
    let outside = r#"(func (export "mf_run") (result i32)
        (call $out (i32.const 65530) (i32.const 100)) (i32.const 0))"#;
    assert!(matches!(
        run(&plugin(outside), json!({})),
        Err(RunError::Trap(_))
    ));
    let huge = r#"(func (export "mf_run") (result i32)
        (call $out (i32.const 0) (i32.const 65537)) (i32.const 0))"#;
    assert!(matches!(
        run(&plugin(huge), json!({})),
        Err(RunError::Trap(_))
    ));
    let big_memory = r#"(module (memory (export "memory") 600)
        (func (export "mf_run") (result i32) (i32.const 0)))"#;
    let big = Plugin::load(&wat::parse_str(big_memory).unwrap()).unwrap();
    assert!(run(&big, json!({})).is_err());
}

#[test]
fn results_must_be_json_objects() {
    let body = r#"(data (i32.const 0) "[1, 2]")
        (func (export "mf_run") (result i32) (call $out (i32.const 0) (i32.const 6)) (i32.const 0))"#;
    assert!(matches!(
        run(&plugin(body), json!({})),
        Err(RunError::Output(_))
    ));
    let nothing = r#"(func (export "mf_run") (result i32) (i32.const 0))"#;
    assert_eq!(run(&plugin(nothing), json!({})).unwrap().output, json!({}));
}

#[test]
fn modules_with_other_imports_or_without_the_exports_are_refused() {
    let load = |wat: &str| Plugin::load(&wat::parse_str(wat).unwrap()).map(drop);
    assert_eq!(
        load(
            r#"(module (import "wasi_snapshot_preview1" "proc_exit" (func (param i32)))
            (memory (export "memory") 1) (func (export "mf_run") (result i32) (i32.const 0)))"#
        ),
        Err(LoadError::Import("wasi_snapshot_preview1.proc_exit".into()))
    );
    assert_eq!(
        load(
            r#"(module (import "mf" "spawn" (func))
            (memory (export "memory") 1) (func (export "mf_run") (result i32) (i32.const 0)))"#
        ),
        Err(LoadError::Import("mf.spawn".into()))
    );
    assert_eq!(
        load(
            r#"(module (import "mf" "input_len" (global i32))
            (memory (export "memory") 1) (func (export "mf_run") (result i32) (i32.const 0)))"#
        )
        .unwrap_err(),
        LoadError::Import("mf.input_len".into())
    );
    assert_eq!(
        load(r#"(module (func (export "mf_run") (result i32) (i32.const 0)))"#),
        Err(LoadError::Export("memory"))
    );
    assert_eq!(
        load(r#"(module (memory (export "memory") 1) (func (export "mf_run")))"#),
        Err(LoadError::Export("mf_run() -> i32"))
    );
    assert!(matches!(
        Plugin::load(b"\0asm\x01\0\0\0garbage"),
        Err(LoadError::Invalid(_))
    ));
    assert_eq!(
        Plugin::load(&vec![0; limits::MODULE_BYTES + 1]).unwrap_err(),
        LoadError::TooLarge
    );
}

#[test]
fn a_wrong_import_signature_fails_at_start() {
    let wat = r#"(module (import "mf" "input_len" (func (param i64)))
        (memory (export "memory") 1) (func (export "mf_run") (result i32) (i32.const 0)))"#;
    let plugin = Plugin::load(&wat::parse_str(wat).unwrap()).unwrap();
    assert!(matches!(run(&plugin, json!({})), Err(RunError::Trap(_))));
}

#[tokio::test]
async fn the_runner_runs_on_a_blocking_thread_and_times_out() {
    struct SlowHttp;
    impl Http for SlowHttp {
        fn send(&self, _: Checked, timeout: Duration) -> Result<Response, String> {
            std::thread::sleep(timeout);
            Err("timed out".into())
        }
    }
    let runner = Runner::new(Arc::new(FakeHttp::default())).with_fuel(10_000_000);
    let echo = Arc::new(plugin(ECHO));
    let rules = vec![HostRule::parse("api.example.com").unwrap()];
    let out = runner
        .run(
            "echo",
            echo,
            json!({"a": 1}),
            rules.clone(),
            Vec::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(out, json!({"a": 1}));

    let body = r#"
        (data (i32.const 1024) "{\"url\": \"https://api.example.com/\"}")
        (func (export "mf_run") (result i32) (local $n i32)
            (local.set $n (call $http (i32.const 1024) (i32.const 35)))
            (call $http_read (i32.const 4096))
            (call $out (i32.const 4096) (local.get $n))
            (i32.const 0))"#;
    let slow = Runner::new(Arc::new(SlowHttp));
    let out = slow
        .run(
            "slow",
            Arc::new(plugin(body)),
            json!({}),
            rules,
            Vec::new(),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
    // The request was cut to the run's deadline.
    assert_eq!(out["error"], "timed out");
}
