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
