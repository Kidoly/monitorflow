//! A login shell on a pseudo-terminal, as the agent's own user (ADR-0178).
//!
//! The shell gets a clean environment: the agent's holds what systemd gave
//! it, such as the enrollment token on a first start, which a person in
//! the terminal must not read. Two threads move the bytes, as reads and
//! writes on a terminal block.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rustix::process::{Pid, Signal};
use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
use rustix::termios::{tcsetwinsize, Winsize};
use tokio::sync::mpsc;

use super::{Control, Spec, Terminal};
use monitorflow_common::remote::MAX_CHUNK;

/// Shells tried in order.
const SHELLS: [&str; 2] = ["/bin/bash", "/bin/sh"];
/// Time the shell gets to end after the hangup, before it is killed.
const HANGUP_GRACE: Duration = Duration::from_secs(2);

pub fn spawn(spec: &Spec) -> std::io::Result<Terminal> {
    let shell = SHELLS
        .iter()
        .find(|s| Path::new(s).exists())
        .ok_or_else(|| std::io::Error::other("no shell found"))?;
    let mut command = Command::new(shell);
    command.arg("-l");
    spawn_command(command, spec, None)
}

/// The helper that starts the shell as the account of
/// `remote.terminal_user`: root, through an exact sudo rule. It reads the
/// signed command first, on a pipe, and checks it itself (ADR-0108).
pub const HELPER: [&str; 3] = ["-n", "/usr/bin/monitorflow-agent", "remote-shell"];

/// A login shell of `remote.terminal_user`, through the root helper.
pub fn spawn_helper(spec: &Spec) -> std::io::Result<Terminal> {
    let mut command = Command::new("sudo");
    command.args(HELPER);
    spawn_command(command, spec, Some(&spec.request))
}

/// Runs `command` on a new pseudo-terminal of `spec`'s size. With
/// `request`, its stdin is a pipe that carries those bytes and closes; the
/// terminal is on its stdout and stderr, and the helper reads it from
/// there.
pub fn spawn_command(
    mut command: Command,
    spec: &Spec,
    request: Option<&[u8]>,
) -> std::io::Result<Terminal> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
        .open(OsStr::from_bytes(name.as_bytes()))?;
    tcsetwinsize(&master, winsize(spec.cols, spec.rows))?;

    let home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
    command
        .env_clear()
        .env("TERM", "xterm-256color")
        .env("HOME", &home)
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env(
            "LANG",
            std::env::var_os("LANG").unwrap_or_else(|| "C.UTF-8".into()),
        )
        .env("MONITORFLOW_REMOTE_SESSION", &spec.session_id)
        .env("MONITORFLOW_REMOTE_USER", &spec.requested_by)
        .current_dir(if Path::new(&home).is_dir() {
            Path::new(&home)
        } else {
            Path::new("/")
        })
        .stdin(match request {
            Some(_) => Stdio::piped(),
            None => Stdio::from(slave.try_clone()?),
        })
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    for name in ["USER", "LOGNAME", "SHELL"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // SAFETY: the closure runs in the child between fork and exec, and
    // calls only setsid and ioctl, which are async-signal-safe system
    // calls: it allocates nothing and takes no lock. It makes the shell
    // the leader of a new session whose controlling terminal is the
    // pseudo-terminal on its stdout, as a login does.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(1))?;
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(command);
    if let (Some(request), Some(mut stdin)) = (request, child.stdin.take()) {
        // A few hundred bytes: far below a pipe's buffer, so it never waits.
        stdin.write_all(request)?;
    }
    let master = File::from(master);

    let (output_tx, output) = mpsc::channel(16);
    let mut reader = master.try_clone()?;
    std::thread::Builder::new()
        .name("remote-pty-read".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_CHUNK];
            loop {
                match reader.read(&mut buffer) {
                    // EIO once the shell and everything on the terminal
                    // closed it.
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if output_tx.blocking_send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;
    let (input, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut writer = master.try_clone()?;
    std::thread::Builder::new()
        .name("remote-pty-write".into())
        .spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        })?;
    Ok(Terminal {
        output,
        input,
        control: Box::new(Pty {
            master,
            child: Some(child),
        }),
    })
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

struct Pty {
    master: File,
    child: Option<Child>,
}

impl Control for Pty {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        // The kernel tells the shell's foreground job with SIGWINCH.
        tcsetwinsize(self.master.as_fd(), winsize(cols, rows))?;
        Ok(())
    }

    fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // The shell leads its own session and process group: a hangup
        // reaches it and its jobs in the foreground, as when a terminal
        // closes.
        if let Some(pid) = Pid::from_raw(child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, Signal::HUP);
        }
        let _ = std::thread::Builder::new()
            .name("remote-pty-reap".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + HANGUP_GRACE;
                while std::time::Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.kill();
    }
}
