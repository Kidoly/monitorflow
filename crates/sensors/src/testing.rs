//! A scriptable [`FakeSensor`] for tests of the scheduler and its users.
//! Available in this crate's tests and, with the `testing` feature, to
//! other crates' tests:
//!
//! ```toml
//! [dev-dependencies]
//! monitorflow-sensors = { workspace = true, features = ["testing"] }
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::{Sample, Sensor, SensorError};

/// What one call to [`FakeSensor::collect`] does.
#[derive(Debug, Clone)]
pub enum Step {
    /// Return these samples.
    Samples(Vec<Sample>),
    Fail(SensorError),
    Panic,
}

/// Returns one sample of value `n` on the n-th call (from 0) unless a
/// [`Step`] was scripted for that call, optionally after a delay. Counts
/// calls and the most calls ever in progress at once.
#[derive(Debug, Default)]
pub struct FakeSensor {
    delay: Duration,
    script: Mutex<Vec<(usize, Step)>>,
    calls: AtomicUsize,
    running: AtomicUsize,
    max_running: AtomicUsize,
}

impl FakeSensor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sleep this long (on the tokio clock) before answering.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Do `step` on call `call` (from 0).
    pub fn on_call(self, call: usize, step: Step) -> Self {
        self.script.lock().unwrap().push((call, step));
        self
    }

    /// Do `step` on every call.
    pub fn always(self, step: Step) -> Self {
        self.on_call(usize::MAX, step)
    }

    /// Calls started so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Calls in progress now.
    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// Most calls in progress at the same time.
    pub fn max_running(&self) -> usize {
        self.max_running.load(Ordering::SeqCst)
    }

    fn step(&self, call: usize) -> Option<Step> {
        let script = self.script.lock().unwrap();
        script
            .iter()
            .find(|(n, _)| *n == call || *n == usize::MAX)
            .map(|(_, step)| step.clone())
    }
}

/// Decrements the running count even when the future is dropped.
struct Running<'a>(&'a AtomicUsize);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Sensor for FakeSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
        let _running = Running(&self.running);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        match self.step(call) {
            None => Ok(vec![Sample::now(call as f64)]),
            Some(Step::Samples(samples)) => Ok(samples),
            Some(Step::Fail(error)) => Err(error),
            Some(Step::Panic) => panic!("FakeSensor told to panic on call {call}"),
        }
    }
}

/// Writes an executable file through a child process, so this process
/// never holds a write descriptor to it. Another test forking at the same
/// moment would inherit such a descriptor until its `exec`, and running
/// the file meanwhile fails with `ETXTBSY`.
pub fn write_executable(path: &std::path::Path, body: &str, mode: u32) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$0\" && chmod \"$1\" \"$0\""])
        .arg(path)
        .arg(format!("{mode:o}"))
        .stdin(Stdio::piped())
        .spawn()
        .expect("cannot run /bin/sh");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(body.as_bytes())
        .expect("cannot write the file");
    drop(stdin);
    assert!(
        child.wait().expect("sh did not run").success(),
        "cannot write {}",
        path.display()
    );
}
