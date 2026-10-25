//! Small Win32 helpers shared by the Windows sensors: UTF-16 strings, the
//! system directory and the check that only trusted accounts can change a
//! file or directory (ADR-0048).

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, GENERIC_ALL, GENERIC_WRITE, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetAce, GetTokenInformation, TokenUser, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `s` as a NUL-terminated UTF-16 string.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// The UTF-16 string in `buf`, up to its first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// A NUL-terminated UTF-16 string at `p`, owned by the caller.
///
/// # Safety
///
/// `p` is null or points to a NUL-terminated UTF-16 string.
pub unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller guarantees a terminating NUL.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// `C:\Windows\System32`, from the API rather than `%SystemRoot%`, which
/// the environment could change.
pub fn system_dir() -> io::Result<PathBuf> {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer and its length match.
    let len = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// `path` without the `\\?\` prefix of `canonicalize` when it is a plain
/// drive path, which some programs (PowerShell's `-File`) do not take.
pub fn plain(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Rights that let an account change what a file holds or who may change
/// it: writing, adding or deleting entries, changing the ACL or owner.
const WRITE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;
/// LocalSystem, BUILTIN\Administrators and TrustedInstaller, which own
/// system files.
const TRUSTED_SIDS: [&str; 3] = [
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];
/// `ACCESS_ALLOWED_ACE_TYPE`, the only allow type whose SID is read; deny
/// types are skipped, other allow types granting a write right refused.
const ACCESS_ALLOWED: u8 = 0;
/// `ACCESS_DENIED_ACE_TYPE` and its object and callback variants.
const ACCESS_DENIED: [u8; 4] = [1, 6, 10, 12];

/// Closes a handle when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

/// Frees memory the system allocated with `LocalAlloc` when dropped.
struct Local<T>(*mut T);

impl<T> Drop for Local<T> {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by the API that returned it, freed once.
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// A SID in its `S-1-…` form.
///
/// # Safety
///
/// `sid` points to a valid SID.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: `sid` is valid; the string is freed by `Local`.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = Local(text);
    // SAFETY: the API returns a NUL-terminated string.
    Ok(unsafe { from_wide_ptr(text.0) })
}

/// The SID of the account this process runs as.
pub fn process_sid() -> io::Result<String> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo handle of this process and an out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    // A TOKEN_USER and its SID; u64 for the alignment of the pointer.
    let mut buf = [0u64; 64];
    let mut used = 0;
    let size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffer and its size match.
    let ok = unsafe {
        GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), size, &mut used)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the buffer now starts with a TOKEN_USER whose SID points
    // inside it.
    unsafe { sid_string((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid) }
}

/// Checks that only trusted accounts (LocalSystem, Administrators,
/// TrustedInstaller and `agent`, the agent's own SID) own `path` or are
/// granted a write right on it by its DACL. Inherit-only entries do not
/// apply to `path` itself and are skipped; a missing DACL, which grants
/// everything to everyone, is refused.
pub fn check_writers(path: &Path, agent: &str) -> Result<(), String> {
    let trusted = |sid: &str| TRUSTED_SIDS.contains(&sid) || sid == agent;
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let name = wide(path);
    // SAFETY: a NUL-terminated name and out pointers; the descriptor is
    // freed by `Local`, and `owner` and `dacl` point inside it.
    let error = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = Local(descriptor);
    let fail = |e: io::Error| format!("{}: cannot read its permissions: {e}", path.display());
    if error != 0 {
        return Err(fail(io::Error::from_raw_os_error(error as i32)));
    }
    // SAFETY: the API returned a valid owner SID.
    let owner = unsafe { sid_string(owner) }.map_err(fail)?;
    if !trusted(&owner) {
        return Err(format!(
            "{} is owned by {owner}, not an administrator or the agent",
            path.display()
        ));
    }
    if dacl.is_null() {
        return Err(format!(
            "{} has no access list: anyone may write it",
            path.display()
        ));
    }
    // SAFETY: a valid ACL inside the descriptor.
    let count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace = ptr::null_mut();
        // SAFETY: `index` is below the ACL's count.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(fail(io::Error::last_os_error()));
        }
        // SAFETY: every ACE starts with an ACE_HEADER and then, for the
        // standard allow and deny types, a u32 access mask.
        let (header, mask) = unsafe {
            let header = *ace.cast::<ACE_HEADER>();
            let mask = ace.cast::<u8>().add(4).cast::<u32>().read_unaligned();
            (header, mask)
        };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
            || ACCESS_DENIED.contains(&header.AceType)
            || mask & WRITE_RIGHTS == 0
        {
            continue;
        }
        if header.AceType != ACCESS_ALLOWED {
            return Err(format!(
                "{} has an access entry of type {} granting write access, which the agent does not check",
                path.display(),
                header.AceType
            ));
        }
        // SAFETY: ACCESS_ALLOWED_ACE: header, mask, then the SID.
        let sid = unsafe { sid_string(ace.cast::<u8>().add(8).cast()) }.map_err(fail)?;
        if !trusted(&sid) {
            return Err(format!(
                "{} grants write access to {sid}, not an administrator or the agent",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings() {
        assert_eq!(wide("ab"), [97, 98, 0]);
        assert_eq!(from_wide(&[97, 98, 0, 99]), "ab");
        assert_eq!(from_wide(&[97]), "a");
        let text = wide("C:\\é");
        // SAFETY: NUL-terminated by `wide`.
        assert_eq!(unsafe { from_wide_ptr(text.as_ptr()) }, "C:\\é");
        // SAFETY: null is allowed.
        assert_eq!(unsafe { from_wide_ptr(ptr::null()) }, "");
    }

    #[test]
    fn plain_paths() {
        assert_eq!(plain(r"\\?\C:\a\b".into()), PathBuf::from(r"C:\a\b"));
        assert_eq!(plain(r"\\?\UNC\h\s".into()), PathBuf::from(r"\\?\UNC\h\s"));
        assert_eq!(plain(r"C:\a".into()), PathBuf::from(r"C:\a"));
        assert!(system_dir().unwrap().join("kernel32.dll").is_file());
    }

    #[test]
    fn writers_of_system_and_user_paths() {
        let agent = process_sid().unwrap();
        assert!(agent.starts_with("S-1-5-"), "{agent}");
        // Owned by TrustedInstaller, writable by administrators only.
        let system = system_dir().unwrap();
        check_writers(&system.join("kernel32.dll"), "S-1-0-0").unwrap();
        // A fresh temporary directory belongs to the test's own account.
        let dir = tempfile::tempdir().unwrap();
        check_writers(dir.path(), &agent).unwrap();
        let error = check_writers(dir.path(), "S-1-0-0").unwrap_err();
        assert!(
            error.contains("not an administrator or the agent"),
            "{error}"
        );
        // Everyone may write: refused.
        let status = std::process::Command::new("icacls")
            .arg(dir.path())
            .args(["/grant", "*S-1-1-0:(W)"])
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        let error = check_writers(dir.path(), &agent).unwrap_err();
        assert!(error.contains("grants write access to S-1-1-0"), "{error}");
        assert!(check_writers(&dir.path().join("missing"), &agent).is_err());
    }
}
