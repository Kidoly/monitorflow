//! Unix file access for the `log` sensor: inodes identify files (ADR-0024).

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::FileId;

/// What the error message names when the agent may not read a file.
pub const ACCOUNT: &str = "the agent's user";

/// Opens a resolved path for reading. `O_NONBLOCK` in case a FIFO replaced
/// the file since the check; `O_NOFOLLOW` in case a symlink did.
pub fn open(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(path)
}

pub fn id(file: &File) -> io::Result<FileId> {
    let meta = file.metadata()?;
    Ok(FileId {
        volume: meta.dev(),
        index: meta.ino().into(),
    })
}

/// The file `path` names now.
pub fn path_id(path: &Path) -> io::Result<FileId> {
    let meta = std::fs::metadata(path)?;
    Ok(FileId {
        volume: meta.dev(),
        index: meta.ino().into(),
    })
}

/// Where the open file is, from `/proc` on Linux: a folder of the path
/// swapped for a link between the check and the open shows here, which
/// `O_NOFOLLOW` alone does not catch. `None` where the system cannot tell.
pub fn final_path(file: &File) -> io::Result<Option<PathBuf>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let link = format!("/proc/self/fd/{}", file.as_raw_fd());
        // Without /proc (a bare chroot), the path checked before the open
        // stands, as on other systems.
        Ok(std::fs::read_link(link).ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Ok(None)
    }
}

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(buf, offset)
}

/// Whether the file was deleted: holding it would keep its space used.
pub fn deleted(file: &File) -> io::Result<bool> {
    Ok(file.metadata()?.nlink() == 0)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_open_file_names_where_it_really_is() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("app.log"), "x").unwrap();
        // A folder of the path that became a link after the check.
        std::os::unix::fs::symlink(&real, dir.path().join("logs")).unwrap();
        let file = open(&dir.path().join("logs/app.log")).unwrap();
        let found = final_path(&file).unwrap().unwrap();
        assert_eq!(found, real.canonicalize().unwrap().join("app.log"));
    }
}
