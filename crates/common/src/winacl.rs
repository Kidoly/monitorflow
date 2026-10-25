//! Windows access control lists for files the agent keeps private, in
//! place of the Unix modes 0600 and 0700 (ADR-0047).
//!
//! A private file or directory gets a protected DACL, so nothing is
//! inherited from `%ProgramData%` (which lets every user read): full
//! control for `SYSTEM`, `Administrators` and the object's owner, which is
//! the service account that created it. Directories pass the same entries
//! on to what is created inside them.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

/// DACL of a private file, in SDDL: `SYSTEM`, `Administrators`, owner.
pub const PRIVATE_FILE: &str = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)";
/// DACL of a private directory; files and directories created inside
/// inherit the same entries.
pub const PRIVATE_DIR: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)";

/// Replaces the DACL of `path` with the one `sddl` describes, protected
/// from inheritance. Blocking.
pub fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let path = wide(path.as_os_str());
    let sddl = wide(sddl.as_ref());
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the
    // calls; `descriptor` is allocated by the first call and freed below
    // with LocalFree, as its documentation requires, on every path after
    // it succeeded. `dacl` points into `descriptor` and is only used
    // before the free.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = ptr::null_mut();
        let result = if GetSecurityDescriptorDacl(
            descriptor,
            &mut present,
            &mut dacl,
            &mut defaulted,
        ) == 0
        {
            Err(io::Error::last_os_error())
        } else {
            let status = SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            );
            match status {
                ERROR_SUCCESS => Ok(()),
                code => Err(io::Error::from_raw_os_error(code as i32)),
            }
        };
        LocalFree(descriptor);
        result
    }
}

/// The DACL of `path` in SDDL, like `D:P(A;;FA;;;SY)…`. Blocking.
pub fn dacl(path: &Path) -> io::Result<String> {
    let path = wide(path.as_os_str());
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut text: *mut u16 = ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `path` is NUL-terminated UTF-16 that outlives the calls;
    // `descriptor` and `text` are allocated by the system and freed with
    // LocalFree once their contents were copied; `len` is the length of
    // `text` in UTF-16 units, as the second call returns it.
    unsafe {
        let status = GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let result = if ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            &mut len,
        ) == 0
        {
            Err(io::Error::last_os_error())
        } else {
            let units = std::slice::from_raw_parts(text, len as usize);
            let sddl = String::from_utf16_lossy(units);
            LocalFree(text.cast());
            Ok(sddl.trim_end_matches('\0').to_owned())
        };
        LocalFree(descriptor);
        result
    }
}

/// Whether `sddl` is a protected DACL granting only `SYSTEM`,
/// `Administrators` and the owner, as [`set_dacl`] leaves a private file
/// or directory, whatever order or flags Windows writes it back with.
pub fn is_private(sddl: &str) -> bool {
    let Some(aces) = sddl.strip_prefix("D:P") else {
        return false;
    };
    let aces = aces.trim_start_matches(|c: char| c.is_ascii_uppercase());
    let mut count = 0;
    for ace in aces.split_terminator(')') {
        let Some(ace) = ace.strip_prefix('(') else {
            return false;
        };
        let trustee = ace.rsplit(';').next().unwrap_or_default();
        if !["SY", "BA", "OW"].contains(&trustee) {
            return false;
        }
        count += 1;
    }
    count > 0
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dacls_are_set_protected_and_inherited() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        set_dacl(&private, PRIVATE_DIR).unwrap();
        let text = dacl(&private).unwrap();
        assert!(is_private(&text), "{text}");

        let file = private.join("key.pem");
        std::fs::write(&file, "secret").unwrap();
        let inherited = dacl(&file).unwrap();
        assert!(
            !is_private(&inherited) && !inherited.contains(";BU)"),
            "{inherited}"
        );
        set_dacl(&file, PRIVATE_FILE).unwrap();
        let text = dacl(&file).unwrap();
        assert!(is_private(&text), "{text}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "secret");
    }

    #[test]
    fn only_protected_dacls_for_system_admins_and_owner_are_private() {
        assert!(is_private(PRIVATE_FILE) && is_private(PRIVATE_DIR));
        assert!(is_private("D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;OW)"));
        assert!(!is_private("D:(A;;FA;;;SY)"));
        assert!(!is_private("D:P(A;;FA;;;SY)(A;;FR;;;BU)"));
        assert!(!is_private("D:P(A;;FA;;;S-1-5-21-1-2-3-1001)"));
        assert!(!is_private("D:P"));
    }

    #[test]
    fn missing_paths_and_bad_sddl_fail() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(set_dacl(&missing, PRIVATE_FILE).is_err());
        assert!(dacl(&missing).is_err());
        assert!(set_dacl(dir.path(), "not sddl").is_err());
    }
}
