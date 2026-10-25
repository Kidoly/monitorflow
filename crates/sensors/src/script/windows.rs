//! The script sensor on Windows. See ADR-0048.
//!
//! A script is a PowerShell script (`.ps1`), run by Windows PowerShell
//! from the system directory with `-NoProfile -NonInteractive
//! -ExecutionPolicy Bypass -File`, or a program (`.exe`, starting with
//! `MZ`), run directly. Batch files are refused: `cmd.exe` parses their
//! arguments again. The output contract is the one of ADR-0024.
//!
//! The script directory, the file and the file's own directory must be
//! owned by an administrator, LocalSystem, TrustedInstaller or the agent's
//! account, and their access lists may grant write access to these only.
//! Checked before every run.
//!
//! The script runs with a fixed environment (system variables and `PATH`),
//! in the root of the system drive, inside a job object that is closed,
//! killing everything the script started, at the deadline and after every
//! run.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use tokio::time::Instant;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use super::{parse_params, report, script_name, spawn_error, wait, DEFAULT_DIR, MARGIN};
use crate::registry::{BuildError, Local, Params};
use crate::win;
use crate::{Sample, Sensor, SensorError};

/// Variables passed from the agent's environment; nothing else is.
const ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATHEXT",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "COMPUTERNAME",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
];

/// How a checked script runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    PowerShell(PathBuf),
    Exe(PathBuf),
}

/// Checks that `name` is installed safely in `dir`, `agent` being the
/// agent's SID, and returns how to run it, symlinks resolved.
pub fn check(dir: &Path, name: &str, agent: &str) -> Result<Program, String> {
    script_name(name)?;
    check_dir(dir, agent)?;
    let path = std::fs::canonicalize(dir.join(name)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("script `{name}` is not installed in {}", dir.display()),
        _ => format!("script `{name}`: {e}"),
    })?;
    let path = win::plain(path);
    let meta = std::fs::metadata(&path).map_err(|e| format!("script `{name}`: {e}"))?;
    let refuse = |why: &str| Err(format!("script `{name}` ({}) {why}", path.display()));
    if !meta.is_file() {
        return refuse("is not a regular file");
    }
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let program = match extension.as_deref() {
        Some("ps1") => Program::PowerShell(path.clone()),
        Some("exe") => Program::Exe(path.clone()),
        _ => return refuse("is neither a PowerShell script (.ps1) nor a program (.exe)"),
    };
    win::check_writers(&path, agent).map_err(|e| format!("script `{name}`: {e}"))?;
    if let Some(parent) = path.parent() {
        check_dir(parent, agent)?;
    }
    if let Program::Exe(_) = program {
        let mut magic = [0u8; 2];
        let read = std::fs::File::open(&path)
            .and_then(|mut f| io::Read::read(&mut f, &mut magic))
            .map_err(|e| format!("script `{name}`: {e}"))?;
        if magic[..read] != *b"MZ" {
            return refuse("is not a Windows program");
        }
    }
    Ok(program)
}

fn check_dir(dir: &Path, agent: &str) -> Result<(), String> {
    let meta =
        std::fs::metadata(dir).map_err(|e| format!("script directory {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "script directory {} is not a directory",
            dir.display()
        ));
    }
    win::check_writers(dir, agent).map_err(|e| format!("script directory {e}"))
}

/// A job object that kills its processes when dropped.
struct Job(HANDLE);

// SAFETY: a kernel handle, usable from any thread.
unsafe impl Send for Job {}

impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: no attributes and no name.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure and its size match the information class.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn assign(&self, process: HANDLE) -> io::Result<()> {
        // SAFETY: two open handles.
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value. Closing it
        // would be enough with KILL_ON_JOB_CLOSE; terminating first does
        // not wait for other handles to the job.
        unsafe {
            TerminateJobObject(self.0, 1);
            CloseHandle(self.0);
        }
    }
}

#[derive(Debug)]
pub struct ScriptSensor {
    script: String,
    args: Vec<String>,
    dir: PathBuf,
}

impl ScriptSensor {
    /// Runs `script` from `dir` with `args`, all already checked.
    pub fn new(script: String, args: Vec<String>, dir: PathBuf) -> Self {
        Self { script, args, dir }
    }

    pub(crate) fn from_params(
        params: &mut Params<'_>,
        local: Option<&Local>,
    ) -> Result<Self, BuildError> {
        let (script, args) = parse_params(params)?;
        let dir = local.map_or_else(|| DEFAULT_DIR.into(), |l| l.script_dir.clone());
        let sensor = Self::new(script, args, dir);
        if local.is_some() {
            let agent = win::process_sid().map_err(|e| params.refused(e.to_string()))?;
            check(&sensor.dir, &sensor.script, &agent).map_err(|reason| params.refused(reason))?;
        }
        Ok(sensor)
    }

    async fn run(
        &self,
        program: Program,
        cutoff: Instant,
    ) -> Result<Option<(ExitStatus, Vec<u8>)>, SensorError> {
        let system = win::system_dir()
            .map_err(|e| SensorError::Transient(format!("system directory: {e}")))?;
        let (exe, mut args): (PathBuf, Vec<OsString>) = match program {
            Program::PowerShell(path) => (
                system.join(r"WindowsPowerShell\v1.0\powershell.exe"),
                [
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ]
                .map(OsString::from)
                .into_iter()
                .chain([path.into_os_string()])
                .collect(),
            ),
            Program::Exe(path) => (path, Vec::new()),
        };
        args.extend(self.args.iter().map(OsString::from));
        let windows = system.parent().unwrap_or(&system);
        let path = std::env::join_paths([
            system.clone(),
            windows.to_path_buf(),
            system.join("Wbem"),
            system.join(r"WindowsPowerShell\v1.0"),
        ])
        .unwrap_or_default();
        let job = Job::new().map_err(|e| SensorError::Transient(format!("job object: {e}")))?;
        let mut child = tokio::process::Command::new(exe)
            .args(args)
            .env_clear()
            .envs(ENV.iter().filter_map(|k| Some((k, std::env::var_os(k)?))))
            .env("PATH", path)
            .current_dir(system.ancestors().last().unwrap_or(&system))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| spawn_error(&self.script, e))?;
        // Assigned right after the start: PowerShell takes far longer to
        // start than this, so nothing escapes the job in between.
        if let Some(handle) = child.raw_handle() {
            if let Err(e) = job.assign(handle) {
                if child.try_wait().ok().flatten().is_none() {
                    return Err(SensorError::Transient(format!(
                        "script `{}`: job object: {e}",
                        self.script
                    )));
                }
            }
        }
        let outcome = wait(&mut child, &self.script, cutoff).await;
        drop(job);
        outcome
    }
}

#[async_trait::async_trait]
impl Sensor for ScriptSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let started = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(started) / 2);
        // A few metadata and ACL reads of local files.
        let agent = win::process_sid()
            .map_err(|e| SensorError::Transient(format!("agent account: {e}")))?;
        let program = check(&self.dir, &self.script, &agent).map_err(SensorError::Permanent)?;
        let outcome = self.run(program, cutoff).await?;
        Ok(report(&self.script, started, outcome))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn installed(name: &str, body: &[u8]) -> (tempfile::TempDir, ScriptSensor) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), body).unwrap();
        let sensor = ScriptSensor::new(
            name.into(),
            vec!["a1".into(), "-w".into()],
            dir.path().into(),
        );
        (dir, sensor)
    }

    fn values(samples: &[Sample]) -> Vec<(String, f64)> {
        samples
            .iter()
            .filter(|s| s.sensor != "duration_seconds")
            .map(|s| (s.sensor.clone(), s.value))
            .collect()
    }

    fn in_secs(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    #[tokio::test]
    async fn powershell_status_and_perfdata() {
        let body = b"Write-Output \"WARNING - $($args -join ' ')\"\nWrite-Output 'free=12.5'\nWrite-Output \"env=$((Get-ChildItem env: | Measure-Object).Count)\"\nexit 1\n";
        let (_dir, sensor) = installed("check.ps1", body);
        let samples = sensor.collect(in_secs(60)).await.unwrap();
        let got = values(&samples);
        assert_eq!(got[0], ("status".into(), 1.0), "{samples:?}");
        assert_eq!(got[1], ("free".into(), 12.5));
        // The fixed environment, plus what PowerShell sets itself.
        assert!(
            got[2].0 == "env" && got[2].1 <= (ENV.len() + 6) as f64,
            "{got:?}"
        );
        assert!(samples.iter().all(|s| s.labels["script"] == "check.ps1"));
    }

    #[tokio::test]
    async fn timeouts_are_unknown_and_kill_the_script() {
        let (_dir, sensor) = installed("slow.ps1", b"Start-Sleep -Seconds 30\nexit 0\n");
        let started = std::time::Instant::now();
        let samples = sensor.collect(in_secs(3)).await.unwrap();
        assert_eq!(values(&samples), [("status".into(), 3.0)]);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn programs_run_directly() {
        let dir = tempfile::tempdir().unwrap();
        // `whoami.exe` exits 0 and prints the account name.
        let system = win::system_dir().unwrap();
        std::fs::copy(system.join("whoami.exe"), dir.path().join("who.exe")).unwrap();
        let sensor = ScriptSensor::new("who.exe".into(), Vec::new(), dir.path().into());
        let samples = sensor.collect(in_secs(30)).await.unwrap();
        assert_eq!(values(&samples), [("status".into(), 0.0)]);
    }

    #[test]
    fn checks_refuse_unsafe_scripts() {
        let agent = win::process_sid().unwrap();
        let (dir, _) = installed("ok.ps1", b"exit 0\n");
        let program = check(dir.path(), "ok.ps1", &agent).unwrap();
        assert!(
            matches!(program, Program::PowerShell(p) if p.ends_with("ok.ps1") && !p.to_string_lossy().starts_with(r"\\?\"))
        );
        std::fs::write(dir.path().join("run.cmd"), b"@echo off\n").unwrap();
        std::fs::write(dir.path().join("fake.exe"), b"#!/bin/sh\n").unwrap();
        for (name, why) in [
            ("missing.ps1", "is not installed"),
            ("run.cmd", "neither a PowerShell script"),
            ("fake.exe", "is not a Windows program"),
            ("..\\x.ps1", "is not a script name"),
        ] {
            let error = check(dir.path(), name, &agent).unwrap_err();
            assert!(error.contains(why), "{name}: {error}");
        }
        // Writable by everyone: refused, at build and before a run.
        let status = std::process::Command::new("icacls")
            .arg(dir.path().join("ok.ps1"))
            .args(["/grant", "*S-1-1-0:(M)"])
            .status()
            .unwrap();
        assert!(status.success());
        let error = check(dir.path(), "ok.ps1", &agent).unwrap_err();
        assert!(error.contains("grants write access to S-1-1-0"), "{error}");
        let local = Local {
            script_dir: dir.path().into(),
            ..Local::default()
        };
        let params = [("script".to_string(), "ok.ps1".to_string())].into();
        let error = crate::registry::build_local("script", &params, &local)
            .err()
            .unwrap();
        assert!(matches!(error, BuildError::Refused { .. }), "{error}");
    }
}
