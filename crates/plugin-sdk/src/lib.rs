//! Writing MonitorFlow plugins in Rust (ADR-0103, `docs/plugins.md`).
//!
//! Build for `wasm32-unknown-unknown` as a `cdylib`, then sign the module
//! with `monitorflow-plugin pack`. A check:
//!
//! ```ignore
//! use monitorflow_plugin_sdk::{check, CheckInput, Sample};
//!
//! fn run(input: CheckInput) -> Result<Vec<Sample>, String> {
//!     let url = input.setting("url")?;
//!     let response = monitorflow_plugin_sdk::Request::get(url).send()?;
//!     Ok(vec![Sample::new("status", f64::from(response.status))])
//! }
//!
//! check!(run);
//! ```
//!
//! The plugin can reach nothing but what these functions give it: its
//! input, a result, log lines and HTTP requests to the hosts its manifest
//! lists, made by MonitorFlow on its behalf.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
pub use serde_json;
use serde_json::Value;

/// The host's functions, ABI version 1. Outside WebAssembly, for unit
/// tests of a plugin's own code, they answer nothing.
mod sys {
    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "mf")]
    extern "C" {
        pub fn input_len() -> i32;
        pub fn input_read(ptr: *mut u8);
        pub fn output(ptr: *const u8, len: i32);
        pub fn log(level: i32, ptr: *const u8, len: i32);
        pub fn http(ptr: *const u8, len: i32) -> i32;
        pub fn http_read(ptr: *mut u8);
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod native {
        pub unsafe fn input_len() -> i32 {
            0
        }
        pub unsafe fn input_read(_: *mut u8) {}
        pub unsafe fn output(_: *const u8, _: i32) {}
        pub unsafe fn log(_: i32, _: *const u8, _: i32) {}
        pub unsafe fn http(_: *const u8, _: i32) -> i32 {
            0
        }
        pub unsafe fn http_read(_: *mut u8) {}
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub use native::*;
}

// SAFETY for every call below: the host reads exactly `len` bytes from a
// live buffer we own, and writes exactly the length it announced (from
// `input_len` or `http`) into a buffer of that length.

fn read_input() -> Vec<u8> {
    let len = unsafe { sys::input_len() }.max(0) as usize;
    let mut buffer = vec![0u8; len];
    if len > 0 {
        unsafe { sys::input_read(buffer.as_mut_ptr()) };
    }
    buffer
}

fn write_output(value: &Value) {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    unsafe { sys::output(bytes.as_ptr(), bytes.len() as i32) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

/// A line in the host's log; lines over 4 KiB per run are dropped.
pub fn log(level: Level, line: &str) {
    unsafe { sys::log(level as i32, line.as_ptr(), line.len() as i32) };
}

/// An HTTP request, sent by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Self {
            method: method.to_owned(),
            url: url.into(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }

    pub fn post(url: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            ..Self::new("POST", url)
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Sends the request. The error says why in words: a host that is not
    /// allowed, a timeout, a refused connection.
    pub fn send(&self) -> Result<Response, String> {
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        let len = unsafe { sys::http(bytes.as_ptr(), bytes.len() as i32) }.max(0) as usize;
        let mut answer = vec![0u8; len];
        if len > 0 {
            unsafe { sys::http_read(answer.as_mut_ptr()) };
        }
        parse_answer(&answer)
    }
}

fn parse_answer(answer: &[u8]) -> Result<Response, String> {
    let value: Value = serde_json::from_slice(answer).map_err(|_| "no answer".to_owned())?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.to_owned());
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// How long the request took, measured by the host.
    #[serde(default)]
    pub elapsed_ms: u64,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Result<Value, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("the answer is not JSON: {e}"))
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// What a check gets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CheckInput {
    /// Its settings, defaults filled in.
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    /// What the check watches, as configured; may be empty.
    #[serde(default)]
    pub instance: String,
}

impl CheckInput {
    /// A setting that must have a value.
    pub fn setting(&self, key: &str) -> Result<&str, String> {
        self.settings
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("the {key} setting is missing"))
    }
}

/// One measurement of a check. `metric` is a suffix of the check's name:
/// `seconds` of a check named `api` is the metric `api_seconds`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    pub metric: String,
    pub instance: String,
    pub value: f64,
    pub labels: BTreeMap<String, String>,
}

impl Sample {
    pub fn new(metric: &str, value: f64) -> Self {
        Self {
            metric: metric.to_owned(),
            instance: String::new(),
            value,
            labels: BTreeMap::new(),
        }
    }

    pub fn instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = instance.into();
        self
    }

    pub fn label(mut self, name: &str, value: impl Into<String>) -> Self {
        self.labels.insert(name.to_owned(), value.into());
        self
    }
}

/// What a channel gets: its settings, its secret settings and the
/// notification.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ChannelInput {
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    pub message: Message,
}

impl ChannelInput {
    /// A setting or a secret setting that must have a value.
    pub fn setting(&self, key: &str) -> Result<&str, String> {
        self.settings
            .get(key)
            .or_else(|| self.secrets.get(key))
            .map(String::as_str)
            .ok_or_else(|| format!("the {key} setting is missing"))
    }
}

/// A notification, as plain text: never markup.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Message {
    /// `incident.opened`, `incident.resolved`, `approval.waiting`,
    /// `update.failed`, `test` and the others of the webhook channel.
    pub event: String,
    /// The notification in one line.
    pub text: String,
    /// The page of the web interface that shows it.
    pub url: String,
    /// For notifications that tell several things at once, one line each.
    #[serde(default)]
    pub lines: Vec<String>,
    /// The webhook channel's JSON body, for every detail.
    #[serde(default)]
    pub payload: Value,
}

#[doc(hidden)]
pub fn __run_check(run: fn(CheckInput) -> Result<Vec<Sample>, String>) -> i32 {
    finish(
        serde_json::from_slice(&read_input())
            .map_err(|e| format!("bad input: {e}"))
            .and_then(run)
            .map(|samples| serde_json::json!({ "samples": samples })),
    )
}

#[doc(hidden)]
pub fn __run_channel(run: fn(ChannelInput) -> Result<(), String>) -> i32 {
    finish(
        serde_json::from_slice(&read_input())
            .map_err(|e| format!("bad input: {e}"))
            .and_then(run)
            .map(|()| serde_json::json!({})),
    )
}

fn finish(result: Result<Value, String>) -> i32 {
    match result {
        Ok(output) => {
            write_output(&output);
            0
        }
        Err(error) => {
            log(Level::Error, &error);
            1
        }
    }
}

/// Makes `run`, a `fn(CheckInput) -> Result<Vec<Sample>, String>`, the
/// plugin's entry point.
#[macro_export]
macro_rules! check {
    ($run:path) => {
        #[no_mangle]
        pub extern "C" fn mf_run() -> i32 {
            $crate::__run_check($run)
        }
    };
}

/// Makes `run`, a `fn(ChannelInput) -> Result<(), String>`, the plugin's
/// entry point.
#[macro_export]
macro_rules! channel {
    ($run:path) => {
        #[no_mangle]
        pub extern "C" fn mf_run() -> i32 {
            $crate::__run_channel($run)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_responses_or_errors() {
        let ok = br#"{"status": 200, "headers": [["Content-Type", "application/json"]], "body": "{\"a\": 1}"}"#;
        let response = parse_answer(ok).unwrap();
        assert!(response.is_success());
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.json().unwrap()["a"], 1);
        assert_eq!(
            parse_answer(br#"{"error": "timed out"}"#).unwrap_err(),
            "timed out"
        );
        assert_eq!(parse_answer(b"").unwrap_err(), "no answer");
    }

    #[test]
    fn inputs_and_samples_have_the_host_shape() {
        let input: ChannelInput = serde_json::from_value(serde_json::json!({
            "settings": {"topic": "ops"},
            "secrets": {"token": "t"},
            "message": {"event": "test", "text": "Test", "url": "https://mf/x"}
        }))
        .unwrap();
        assert_eq!(input.setting("token").unwrap(), "t");
        assert!(input.setting("other").is_err());
        let sample = Sample::new("seconds", 0.5)
            .instance("api")
            .label("code", "200");
        assert_eq!(
            serde_json::to_value(sample).unwrap(),
            serde_json::json!({"metric": "seconds", "instance": "api", "value": 0.5, "labels": {"code": "200"}})
        );
    }
}
