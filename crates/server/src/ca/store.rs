//! Files under the CA directory: atomic writes and the key permission check.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use super::CaError;

/// Writes `contents` to a temporary file next to `path` with `mode`, syncs
/// it, renames it over `path` and syncs the directory, so a crash leaves
/// either the old file or the new one.
pub(crate) fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<(), CaError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp"));
    let tmp_err = |error| CaError::io(&tmp, error);
    // A leftover from a crash; create_new below must not follow it.
    match fs::remove_file(&tmp) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(tmp_err(error)),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(tmp_err)?;
    file.write_all(contents).map_err(tmp_err)?;
    file.sync_all().map_err(tmp_err)?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| CaError::io(path, e))?;
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| CaError::io(dir, e))
}

/// Refuses a key file that group or others can read or write.
pub(super) fn check_key_permissions(path: &Path) -> Result<(), CaError> {
    let meta = fs::metadata(path).map_err(|e| CaError::io(path, e))?;
    if !meta.is_file() {
        return Err(CaError::corrupt(path, "not a regular file"));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(CaError::KeyPermissions {
            path: path.to_owned(),
            mode,
        });
    }
    Ok(())
}
