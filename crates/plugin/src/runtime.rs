//! Runs plugins in wasmi (ADR-0103). A module may import only the `mf`
//! functions below and must export `memory` and `mf_run`. Each run gets a
//! fresh instance with its own memory, fuel and limits, on a blocking
//! thread; [`Runner`] bounds how many run at once.
//!
//! | Import | Does |
//! | --- | --- |
//! | `input_len() -> i32` | Length of the input JSON |
//! | `input_read(ptr)` | Copies the input JSON to `ptr` |
//! | `output(ptr, len)` | Sets the result JSON; the last call wins |
//! | `log(level, ptr, len)` | A log line, 0 error to 3 debug |
//! | `http(ptr, len) -> i32` | Sends the request JSON at `ptr`; the length of the answer |
//! | `http_read(ptr)` | Copies the answer, `{"status", "headers", "body"}` or `{"error"}` |

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Semaphore;
use wasmi::{
    Caller, Config, EnforcedLimits, Engine, Extern, ExternType, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode,
};

use crate::http::{self, HostRule, Http};
use crate::limits;

const IMPORT_MODULE: &str = "mf";
const IMPORTS: [&str; 6] = [
    "input_len",
    "input_read",
    "output",
    "log",
    "http",
    "http_read",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("the module is over {} MiB", limits::MODULE_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("not a valid WebAssembly module: {0}")]
    Invalid(String),
    #[error("it imports {0}, which plugins cannot use")]
    Import(String),
    #[error("it does not export {0}")]
    Export(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("the plugin used more than its share of CPU")]
    OutOfFuel,
    #[error("the plugin did not finish in time")]
    TimedOut,
    #[error("the plugin stopped: {0}")]
    Trap(String),
    #[error("{0}")]
    Failed(String),
    #[error("the plugin's result is not valid: {0}")]
    Output(String),
    #[error("the plugin could not start: {0}")]
    Start(String),
}

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::default();
        config
            .consume_fuel(true)
            .enforced_limits(EnforcedLimits::strict())
            .wasm_multi_memory(false)
            .wasm_custom_page_sizes(false);
        Engine::new(&config)
    })
}

/// A module that passed [`Plugin::load`], ready to run any number of
/// times.
#[derive(Debug, Clone)]
pub struct Plugin {
    module: Module,
}

impl Plugin {
    /// Parses, validates and checks the imports and exports of `wasm`.
    pub fn load(wasm: &[u8]) -> Result<Self, LoadError> {
        if wasm.len() > limits::MODULE_BYTES {
            return Err(LoadError::TooLarge);
        }
        let module = Module::new(engine(), wasm).map_err(|e| LoadError::Invalid(e.to_string()))?;
        for import in module.imports() {
            let known = import.module() == IMPORT_MODULE
                && IMPORTS.contains(&import.name())
                && matches!(import.ty(), ExternType::Func(_));
            if !known {
                return Err(LoadError::Import(format!(
                    "{}.{}",
                    import.module(),
                    import.name()
                )));
            }
        }
        if !matches!(module.get_export("memory"), Some(ExternType::Memory(_))) {
            return Err(LoadError::Export("memory"));
        }
        let run = match module.get_export("mf_run") {
            Some(ExternType::Func(ty)) => {
                ty.params().is_empty() && ty.results() == [wasmi::ValType::I32]
            }
            _ => false,
        };
        if !run {
            return Err(LoadError::Export("mf_run() -> i32"));
        }
        Ok(Self { module })
    }

    /// Runs the plugin once on this thread, which may block in HTTP
    /// requests: call it from a blocking thread.
    pub fn run_blocking(&self, input: &Value, context: Context) -> Result<Outcome, RunError> {
        let input = serde_json::to_vec(input).map_err(|e| RunError::Start(e.to_string()))?;
        if input.len() > limits::INPUT_BYTES {
            return Err(RunError::Start("its settings are too large".into()));
        }
        let fuel = context.fuel;
        let state = State {
            limits: StoreLimitsBuilder::new()
                .memory_size(limits::MEMORY_BYTES)
                .table_elements(limits::TABLE_ELEMENTS)
                .instances(1)
                .memories(1)
                .tables(1)
                .build(),
            input,
            output: None,
            answer: Vec::new(),
            requests: 0,
            logs: Vec::new(),
            log_bytes: 0,
            context,
        };
        let mut store = Store::new(engine(), state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(fuel)
            .map_err(|e| RunError::Start(e.to_string()))?;
        let linker = linker();
        let result = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| trap(&e, &store))
            .and_then(|instance| {
                let run = instance
                    .get_typed_func::<(), i32>(&store, "mf_run")
                    .map_err(|e| RunError::Start(e.to_string()))?;
                run.call(&mut store, ()).map_err(|e| trap(&e, &store))
            });
        let state = store.into_data();
        let code = match result {
            Ok(code) => code,
            Err(error) => {
                return Err(error);
            }
        };
        if code != 0 {
            let last_error = state.logs.iter().rev().find(|(level, _)| *level == 0);
            return Err(RunError::Failed(match last_error {
                Some((_, line)) => line.clone(),
                None => format!("the plugin failed with code {code}"),
            }));
        }
        let output = match state.output {
            None => Value::Object(Default::default()),
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| RunError::Output(e.to_string()))?
            }
        };
        if !output.is_object() {
            return Err(RunError::Output("not a JSON object".into()));
        }
        Ok(Outcome {
            output,
            logs: state.logs,
        })
    }
}

fn trap(error: &wasmi::Error, store: &Store<State>) -> RunError {
    if error.as_trap_code() == Some(TrapCode::OutOfFuel) {
        return RunError::OutOfFuel;
    }
    if Instant::now() >= store.data().context.deadline {
        return RunError::TimedOut;
    }
    RunError::Trap(error.to_string())
}

/// What a run may reach.
pub struct Context {
    pub http: Arc<dyn Http>,
    /// Hosts it may call, from its manifest and settings.
    pub rules: Vec<HostRule>,
    /// No HTTP request starts after it, and none runs past it.
    pub deadline: Instant,
    /// Values replaced by `[REDACTED]` in its logs and errors.
    pub secrets: Vec<String>,
    /// [`limits::FUEL`], less in tests.
    pub fuel: u64,
}

/// A run's result JSON and log lines, `(level, text)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub output: Value,
    pub logs: Vec<(u32, String)>,
}

struct State {
    limits: StoreLimits,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
    answer: Vec<u8>,
    requests: usize,
    logs: Vec<(u32, String)>,
    log_bytes: usize,
    context: Context,
}

impl State {
    fn redact(&self, text: &str) -> String {
        let mut text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        for secret in self.context.secrets.iter().filter(|s| s.len() >= 4) {
            text = text.replace(secret.as_str(), "[REDACTED]");
        }
        text
    }

    fn http(&mut self, request: &[u8]) -> Value {
        self.requests += 1;
        if self.requests > limits::HTTP_REQUESTS {
            return serde_json::json!({"error": format!("at most {} requests per run", limits::HTTP_REQUESTS)});
        }
        let request = match serde_json::from_slice::<http::Request>(request) {
            Ok(request) => request,
            Err(e) => return serde_json::json!({"error": format!("bad request: {e}")}),
        };
        let checked = match http::check(request, &self.context.rules) {
            Ok(checked) => checked,
            Err(e) => return serde_json::json!({"error": e}),
        };
        let left = self
            .context
            .deadline
            .saturating_duration_since(Instant::now());
        if left < Duration::from_millis(50) {
            return serde_json::json!({"error": "out of time"});
        }
        match self
            .context
            .http
            .send(checked, left.min(limits::HTTP_TIMEOUT))
        {
            Ok(response) => serde_json::to_value(response).unwrap_or_default(),
            Err(e) => serde_json::json!({"error": self.redact(&e)}),
        }
    }
}

fn memory(caller: &Caller<'_, State>) -> Result<wasmi::Memory, wasmi::Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("no memory"))
}

fn read(
    caller: &Caller<'_, State>,
    ptr: i32,
    len: i32,
    max: usize,
) -> Result<Vec<u8>, wasmi::Error> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    if len > max {
        return Err(wasmi::Error::new(format!("more than {max} bytes")));
    }
    let memory = memory(caller)?;
    memory
        .data(caller)
        .get(ptr..ptr.saturating_add(len))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| wasmi::Error::new("out of bounds"))
}

fn write(caller: &mut Caller<'_, State>, ptr: i32, bytes: Vec<u8>) -> Result<(), wasmi::Error> {
    let ptr = ptr as u32 as usize;
    let memory = memory(caller)?;
    memory
        .data_mut(caller)
        .get_mut(ptr..ptr.saturating_add(bytes.len()))
        .ok_or_else(|| wasmi::Error::new("out of bounds"))?
        .copy_from_slice(&bytes);
    Ok(())
}

fn length(bytes: &[u8]) -> i32 {
    // Inputs and answers are bounded far below 2 GiB.
    bytes.len() as i32
}

fn linker() -> Linker<State> {
    let mut linker = Linker::new(engine());
    let m = IMPORT_MODULE;
    linker
        .func_wrap(m, "input_len", |caller: Caller<'_, State>| {
            length(&caller.data().input)
        })
        .and_then(|l| {
            l.func_wrap(
                m,
                "input_read",
                |mut caller: Caller<'_, State>, ptr: i32| {
                    let input = caller.data().input.clone();
                    write(&mut caller, ptr, input)
                },
            )
        })
        .and_then(|l| {
            l.func_wrap(
                m,
                "output",
                |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
                    let bytes = read(&caller, ptr, len, limits::OUTPUT_BYTES)?;
                    caller.data_mut().output = Some(bytes);
                    Ok(())
                },
            )
        })
        .and_then(|l| {
            l.func_wrap(
                m,
                "log",
                |mut caller: Caller<'_, State>, level: i32, ptr: i32, len: i32| {
                    let left = limits::LOG_BYTES.saturating_sub(caller.data().log_bytes);
                    let len = (len as u32 as usize).min(left);
                    if len == 0 {
                        return Ok(());
                    }
                    let bytes = read(&caller, ptr, len as i32, left)?;
                    let state = caller.data_mut();
                    state.log_bytes += len;
                    let line = state.redact(&String::from_utf8_lossy(&bytes));
                    state.logs.push((level.clamp(0, 3) as u32, line));
                    Ok(())
                },
            )
        })
        .and_then(|l| {
            l.func_wrap(
                m,
                "http",
                |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
                    let request = read(&caller, ptr, len, limits::HTTP_REQUEST_BYTES + 16 * 1024)?;
                    let state = caller.data_mut();
                    let answer = state.http(&request);
                    state.answer = serde_json::to_vec(&answer).unwrap_or_default();
                    Ok(length(&state.answer))
                },
            )
        })
        .and_then(|l| {
            l.func_wrap(m, "http_read", |mut caller: Caller<'_, State>, ptr: i32| {
                let answer = std::mem::take(&mut caller.data_mut().answer);
                write(&mut caller, ptr, answer)
            })
        })
        .expect("the mf imports are defined once each");
    linker
}

/// Runs plugins on blocking threads, at most
/// [`limits::CONCURRENT_RUNS`] at once.
#[derive(Clone)]
pub struct Runner {
    http: Arc<dyn Http>,
    permits: Arc<Semaphore>,
    fuel: u64,
}

impl Runner {
    pub fn new(http: Arc<dyn Http>) -> Self {
        Self {
            http,
            permits: Arc::new(Semaphore::new(limits::CONCURRENT_RUNS)),
            fuel: limits::FUEL,
        }
    }

    /// Gives each run `fuel` instead of [`limits::FUEL`]: tests run
    /// unoptimized builds, where the full share takes minutes.
    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel.min(limits::FUEL);
        self
    }

    /// Runs `plugin` on `input` within `timeout`, waiting for its turn
    /// first. `name` is for the logs, where its log lines go at debug
    /// level.
    pub async fn run(
        &self,
        name: &str,
        plugin: Arc<Plugin>,
        input: Value,
        rules: Vec<HostRule>,
        secrets: Vec<String>,
        timeout: Duration,
    ) -> Result<Value, RunError> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| RunError::Start(e.to_string()))?;
        let context = Context {
            http: self.http.clone(),
            rules,
            deadline: Instant::now() + timeout,
            secrets,
            fuel: self.fuel,
        };
        let task = tokio::task::spawn_blocking(move || {
            // Held until the run ends, even after a timeout below.
            let _permit = permit;
            plugin.run_blocking(&input, context)
        });
        // Fuel ends a run that computes; the deadline ends one waiting on
        // HTTP. The grace covers a request started just before it.
        let outcome = match tokio::time::timeout(timeout + Duration::from_secs(1), task).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(RunError::Trap(e.to_string())),
            Err(_) => Err(RunError::TimedOut),
        };
        match outcome {
            Ok(outcome) => {
                for (level, line) in &outcome.logs {
                    tracing::debug!(plugin = name, level, "{line}");
                }
                Ok(outcome.output)
            }
            Err(error) => {
                tracing::debug!(plugin = name, %error, "plugin run failed");
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests;
