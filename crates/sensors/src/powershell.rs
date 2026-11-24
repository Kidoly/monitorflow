//! How Windows PowerShell runs for the agent: the script sensor
//! (ADR-0048) and the `shell` command of AI remediation (ADR-0084) start
//! it the same way.
//!
//! Windows PowerShell 5.1 from the system directory, found with
//! `GetSystemDirectoryW` rather than `PATH`, with the system variables
//! only, `PATH` set to the system directories, from the root of the
//! system drive, without a window, inside a [`Job`] that kills everything
//! it started when dropped.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::win;
pub use crate::win::system_dir;

/// Variables passed from the agent's environment; nothing else is.
/// Without `PSModulePath`, Windows PowerShell rebuilds the module path
/// and searches every installed module for the commands a script uses,
/// on every run: 18 to 33 s for a `Write-Output` on the CI runner.
pub const ENV: &[&str] = &[
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
    "PSModulePath",
];

/// Options before the script or command: no banner, no profile, no
/// prompt, and no execution policy check, since the caller decided what
/// runs. A machine policy set by Group Policy still wins.
pub const OPTIONS: [&str; 5] = [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
];

/// `powershell.exe` of Windows PowerShell 5.1 under `system`.
pub fn exe(system: &Path) -> PathBuf {
    system.join(r"WindowsPowerShell\v1.0\powershell.exe")
}

/// `PATH` of what runs: the system directories only.
pub fn path(system: &Path) -> OsString {
    let windows = system.parent().unwrap_or(system);
    std::env::join_paths([
        system.to_path_buf(),
        windows.to_path_buf(),
        system.join("Wbem"),
        system.join(r"WindowsPowerShell\v1.0"),
    ])
    .unwrap_or_default()
}

/// `program`, set up as described in the module documentation: stdin
/// closed, stdout piped, stderr closed. Callers pipe stderr if they read
/// it, and put the child in a [`Job`] right after it starts.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> io::Result<tokio::process::Command> {
    let system = win::system_dir()?;
    let mut command = tokio::process::Command::new(program);
    command
        .env_clear()
        .envs(ENV.iter().filter_map(|k| Some((k, std::env::var_os(k)?))))
        .env("PATH", path(&system))
        .current_dir(system.ancestors().last().unwrap_or(&system))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    Ok(command)
}

/// A job object that kills its processes when dropped, the Windows
/// counterpart of a process group killed at a deadline.
///
/// A process is put in the job right after it starts, not created inside
/// it: Rust's `Command` does not expose `PROC_THREAD_ATTRIBUTE_JOB_LIST`,
/// and PowerShell takes far longer to start than the gap.
pub struct Job(HANDLE);

// SAFETY: a kernel handle, usable from any thread.
unsafe impl Send for Job {}
// SAFETY: the handle is only passed to thread-safe kernel calls.
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> io::Result<Self> {
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

    /// Puts `child` in the job. A child that already exited is not an
    /// error: nothing of it is left to kill.
    pub fn assign(&self, child: &mut tokio::process::Child) -> io::Result<()> {
        let Some(process) = child.raw_handle() else {
            return Ok(());
        };
        // SAFETY: two open handles.
        if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
            let error = io::Error::last_os_error();
            if child.try_wait().ok().flatten().is_none() {
                return Err(error);
            }
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

/// Most output [`run_script`] reads.
const MAX_SCRIPT_OUTPUT: u64 = 16 << 20;

/// `script` as `-EncodedCommand` takes it: base64 of its UTF-16LE.
fn encoded(script: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Runs a script built into the agent, like the Hyper-V sensor's
/// (ADR-0156) or the inventory's (ADR-0187), and returns what it printed
/// on stdout. `what` names it in errors.
pub async fn run_script(
    script: &str,
    deadline: tokio::time::Instant,
    what: &str,
) -> Result<String, String> {
    use tokio::io::AsyncReadExt;

    let system = system_dir().map_err(|e| e.to_string())?;
    let job = Job::new().map_err(|e| format!("job object: {e}"))?;
    let mut child = command(exe(&system))
        .and_then(|mut command| {
            command
                .args(OPTIONS)
                .arg("-EncodedCommand")
                .arg(encoded(script))
                .stdout(Stdio::piped())
                .spawn()
        })
        .map_err(|e| format!("starting powershell.exe: {e}"))?;
    job.assign(&mut child)
        .map_err(|e| format!("job object: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped").take(MAX_SCRIPT_OUTPUT);
    let mut text = String::new();
    let read = tokio::time::timeout_at(deadline, async {
        stdout.read_to_string(&mut text).await?;
        child.wait().await
    })
    .await;
    drop(job);
    match read {
        Ok(Ok(_)) => Ok(text),
        Ok(Err(error)) => Err(format!("reading the output of {what}: {error}")),
        Err(_) => Err(format!("{what} did not finish in time")),
    }
}

#[cfg(test)]
mod script_tests {
    #[test]
    fn base64_matches_known_values() {
        // "a" in UTF-16LE is 61 00.
        assert_eq!(super::encoded("a"), "YQA=");
        assert_eq!(super::encoded("ab"), "YQBiAA==");
    }
}
