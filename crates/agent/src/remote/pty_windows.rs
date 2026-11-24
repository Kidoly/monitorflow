//! Windows PowerShell on a pseudo console (ConPTY), as the agent's service
//! account (ADR-0178). Two threads move the bytes over the console's
//! pipes, as reads and writes on them block.

use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::ptr::{null, null_mut};

use monitorflow_common::remote::MAX_CHUNK;
use tokio::sync::mpsc;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::{Control, Spec, Terminal};

pub fn spawn(spec: &Spec) -> std::io::Result<Terminal> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let line = format!(r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe -NoLogo");
    let console = PseudoConsole::new(spec.cols, spec.rows)?;
    let process = console.start(&line)?;
    let (output_tx, output) = mpsc::channel(16);
    let mut reader = console.output.try_clone()?;
    std::thread::Builder::new()
        .name("remote-pty-read".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_CHUNK];
            loop {
                match reader.read(&mut buffer) {
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
    let mut writer = console.input.try_clone()?;
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
        control: Box::new(Conpty {
            console: Some(console),
            process,
        }),
    })
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD {
        X: cols.min(i16::MAX as u16) as i16,
        Y: rows.min(i16::MAX as u16) as i16,
    }
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// A pseudo console with our ends of its pipes.
struct PseudoConsole {
    handle: HPCON,
    input: File,
    output: File,
}

impl PseudoConsole {
    fn new(cols: u16, rows: u16) -> std::io::Result<Self> {
        let mut input_read: HANDLE = null_mut();
        let mut input_write: HANDLE = null_mut();
        let mut output_read: HANDLE = null_mut();
        let mut output_write: HANDLE = null_mut();
        // SAFETY: each call gets valid pointers to handles of this frame;
        // every handle created is either closed here or owned by a File.
        unsafe {
            if CreatePipe(&mut input_read, &mut input_write, null(), 0) == 0 {
                return Err(last_error());
            }
            if CreatePipe(&mut output_read, &mut output_write, null(), 0) == 0 {
                let error = last_error();
                CloseHandle(input_read);
                CloseHandle(input_write);
                return Err(error);
            }
            let mut handle: HPCON = 0;
            let result =
                CreatePseudoConsole(coord(cols, rows), input_read, output_write, 0, &mut handle);
            // The console holds its own copies of its ends.
            CloseHandle(input_read);
            CloseHandle(output_write);
            let input = File::from_raw_handle(input_write as RawHandle);
            let output = File::from_raw_handle(output_read as RawHandle);
            if result < 0 {
                return Err(std::io::Error::from_raw_os_error(result));
            }
            Ok(Self {
                handle,
                input,
                output,
            })
        }
    }

    /// Starts `line` attached to the console.
    fn start(&self, line: &str) -> std::io::Result<Process> {
        let mut line: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
        // SAFETY: the attribute list lives in `list`, sized as Windows
        // asks, until it is deleted at the end; the console handle stays
        // valid while `self` lives; CreateProcessW gets a mutable, null-
        // terminated command line, as it requires.
        unsafe {
            let mut size = 0usize;
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
            let mut list = vec![0u8; size];
            let attributes = list.as_mut_ptr() as *mut c_void;
            if InitializeProcThreadAttributeList(attributes, 1, 0, &mut size) == 0 {
                return Err(last_error());
            }
            let updated = UpdateProcThreadAttribute(
                attributes,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                self.handle as *const c_void,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null(),
            );
            if updated == 0 {
                let error = last_error();
                DeleteProcThreadAttributeList(attributes);
                return Err(error);
            }
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            // No standard handles of the service: the shell must use the
            // console's.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.lpAttributeList = attributes;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                null(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            );
            let error = last_error();
            DeleteProcThreadAttributeList(attributes);
            drop(list);
            if created == 0 {
                return Err(error);
            }
            CloseHandle(info.hThread);
            Ok(Process(info.hProcess))
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreatePseudoConsole and is closed
        // once, here.
        unsafe { ClosePseudoConsole(self.handle) };
    }
}

/// The shell's process handle.
struct Process(HANDLE);

// SAFETY: a process handle is a kernel handle, usable from any thread.
unsafe impl Send for Process {}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateProcessW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

struct Conpty {
    console: Option<PseudoConsole>,
    process: Process,
}

impl Control for Conpty {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let Some(console) = &self.console else {
            return Ok(());
        };
        // SAFETY: the console handle is valid while `console` lives.
        let result = unsafe { ResizePseudoConsole(console.handle, coord(cols, rows)) };
        if result < 0 {
            return Err(std::io::Error::from_raw_os_error(result));
        }
        Ok(())
    }

    fn kill(&mut self) {
        // SAFETY: the process handle is valid while `self.process` lives.
        unsafe { TerminateProcess(self.process.0, 1) };
        // Closing the console ends what is still attached to it. It may
        // wait for its output to be read, which the reader thread does,
        // so it is closed on a thread of its own.
        if let Some(console) = self.console.take() {
            let _ = std::thread::Builder::new()
                .name("remote-pty-close".into())
                .spawn(move || drop(console));
        }
    }
}

impl Drop for Conpty {
    fn drop(&mut self) {
        self.kill();
    }
}
