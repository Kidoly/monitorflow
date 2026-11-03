//! The compose file follows the release (ADR-0087). The updater image
//! carries the files of `monitorflow-server-compose.tar.gz`. When those of
//! the deploy directory differ, as after an update from the web interface
//! to a release whose compose file changed, the updater saves them, puts
//! its own in place, and the caller applies them, or puts the saved ones
//! back. Blocking file I/O on small files.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Where the updater image holds the release's files.
pub const BUNDLE_DIR: &str = "/usr/share/monitorflow/deploy";
/// What `deploy/release-files.sh` bundles, relative to the deploy
/// directory; `deploy/docker/updater.Dockerfile` copies the same.
pub const FILES: [&str; 4] = [
    "docker-compose.yml",
    "docker/pg_hba.conf",
    "restore.sh",
    ".env.example",
];
/// A file only a checkout has: a checkout follows git, not the updater.
const CHECKOUT_MARK: &str = "docker/server.Dockerfile";
/// The saved files, in the updater's volume. Present means a change is
/// under way.
const PREVIOUS_DIR: &str = "compose-previous";

/// What [`plan`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The deploy directory holds the release's files.
    Same,
    /// A git checkout, or a compose file of another name.
    NotOurs,
    /// An image without the files, like a local build of an older updater.
    NoBundle,
    Install,
}

/// Compares the bundle with the deploy directory. `compose_files` are the
/// files the project runs, as this container sees them.
pub fn plan(bundle: &Path, deploy: &Path, compose_files: &[PathBuf]) -> io::Result<Plan> {
    if !bundle.join(FILES[0]).is_file() {
        return Ok(Plan::NoBundle);
    }
    let ours = deploy.join(FILES[0]);
    if deploy.join(CHECKOUT_MARK).exists() || !compose_files.contains(&ours) {
        return Ok(Plan::NotOurs);
    }
    for file in FILES {
        let wanted = fs::read(bundle.join(file))?;
        match fs::read(deploy.join(file)) {
            Ok(current) if current == wanted => {}
            Ok(_) => return Ok(Plan::Install),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Plan::Install),
            Err(e) => return Err(e),
        }
    }
    Ok(Plan::Same)
}

/// Whether a change was interrupted: saved files are still there.
pub fn pending(state: &Path) -> bool {
    state.join(PREVIOUS_DIR).is_dir()
}

/// Copies the deploy directory's files into the updater's volume, then
/// marks the change as under way by renaming the copy into place.
pub fn save_previous(deploy: &Path, state: &Path) -> io::Result<()> {
    let tmp = state.join(format!("{PREVIOUS_DIR}.tmp"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    for file in FILES {
        let from = deploy.join(file);
        if from.is_file() {
            copy_atomic(&from, &tmp.join(file))?;
        }
    }
    fs::create_dir_all(&tmp)?;
    fs::rename(&tmp, state.join(PREVIOUS_DIR))
}

/// Puts the bundle's files in the deploy directory, each atomically.
/// Identical files stay as they are: a container that bind-mounts one,
/// like PostgreSQL its `pg_hba.conf`, keeps the same file.
pub fn install(bundle: &Path, deploy: &Path) -> io::Result<()> {
    for file in FILES {
        let (from, to) = (bundle.join(file), deploy.join(file));
        if fs::read(&to).ok() != Some(fs::read(&from)?) {
            copy_atomic(&from, &to)?;
        }
    }
    Ok(())
}

/// Puts the saved files back. A file that was not there before stays: no
/// compose file reads it.
pub fn put_back(deploy: &Path, state: &Path) -> io::Result<()> {
    let previous = state.join(PREVIOUS_DIR);
    for file in FILES {
        let saved = previous.join(file);
        if saved.is_file() {
            copy_atomic(&saved, &deploy.join(file))?;
        }
    }
    Ok(())
}

/// Ends the change: the saved files go.
pub fn finish(state: &Path) -> io::Result<()> {
    match fs::remove_dir_all(state.join(PREVIOUS_DIR)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Copies `from` to `to` through a temporary file and a rename, with the
/// mode of `from`, so a reader sees one file or the other.
fn copy_atomic(from: &Path, to: &Path) -> io::Result<()> {
    let dir = to.parent().expect("a file in a directory");
    fs::create_dir_all(dir)?;
    let name = to.file_name().expect("a file name").to_string_lossy();
    let tmp = dir.join(format!(".{name}.monitorflow-tmp"));
    fs::copy(from, &tmp)?;
    let mode = fs::metadata(from)?.permissions().mode() & 0o777;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, to)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    struct Dirs {
        _root: TempDir,
        bundle: PathBuf,
        deploy: PathBuf,
        state: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = TempDir::new().unwrap();
        let [bundle, deploy, state] = ["bundle", "deploy", "state"].map(|d| root.path().join(d));
        for (dir, version) in [(&bundle, "new"), (&deploy, "old")] {
            for file in FILES {
                let path = dir.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, format!("{file} {version}\n")).unwrap();
            }
        }
        fs::set_permissions(bundle.join("restore.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(&state).unwrap();
        Dirs {
            _root: root,
            bundle,
            deploy,
            state,
        }
    }

    fn compose(d: &Dirs) -> Vec<PathBuf> {
        vec![d.deploy.join("docker-compose.yml")]
    }

    #[test]
    fn installs_saves_and_puts_back() {
        let d = dirs();
        assert_eq!(
            plan(&d.bundle, &d.deploy, &compose(&d)).unwrap(),
            Plan::Install
        );
        save_previous(&d.deploy, &d.state).unwrap();
        assert!(pending(&d.state));
        install(&d.bundle, &d.deploy).unwrap();
        assert_eq!(
            plan(&d.bundle, &d.deploy, &compose(&d)).unwrap(),
            Plan::Same
        );
        let mode = fs::metadata(d.deploy.join("restore.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);

        put_back(&d.deploy, &d.state).unwrap();
        let compose_file = fs::read_to_string(d.deploy.join("docker-compose.yml")).unwrap();
        assert_eq!(compose_file, "docker-compose.yml old\n");
        finish(&d.state).unwrap();
        assert!(!pending(&d.state));
        finish(&d.state).unwrap();
    }

    #[test]
    fn a_missing_file_is_installed() {
        let d = dirs();
        install(&d.bundle, &d.deploy).unwrap();
        fs::remove_file(d.deploy.join("restore.sh")).unwrap();
        assert_eq!(
            plan(&d.bundle, &d.deploy, &compose(&d)).unwrap(),
            Plan::Install
        );
        save_previous(&d.deploy, &d.state).unwrap();
        install(&d.bundle, &d.deploy).unwrap();
        put_back(&d.deploy, &d.state).unwrap();
        assert!(d.deploy.join("restore.sh").is_file());
    }

    #[test]
    fn leaves_checkouts_other_files_and_old_images_alone() {
        let d = dirs();
        assert_eq!(
            plan(&d.bundle, &d.deploy, &[d.deploy.join("compose.yaml")]).unwrap(),
            Plan::NotOurs
        );
        fs::write(d.deploy.join(CHECKOUT_MARK), "FROM x\n").unwrap();
        assert_eq!(
            plan(&d.bundle, &d.deploy, &compose(&d)).unwrap(),
            Plan::NotOurs
        );
        let empty = d.state.join("empty");
        assert_eq!(
            plan(&empty, &d.deploy, &compose(&d)).unwrap(),
            Plan::NoBundle
        );
    }
}
