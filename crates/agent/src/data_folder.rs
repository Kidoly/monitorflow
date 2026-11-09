//! Who owns `%ProgramData%\\MonitorFlow`, checked by the MSI package before
//! it writes the configuration there (ADR-0050). Any user may create a
//! folder in `%ProgramData%`: one who made ours before the install stays
//! its owner, may rewrite its access list whenever they like, and may have
//! left an `agent.toml` or scripts of their own in it.

use std::path::Path;

use monitorflow_sensors::{check_owner, check_writers};

/// The folders the package gives an access list of their own, below the
/// data folder.
const PROTECTED: [&str; 2] = ["agent", "scripts.d"];

/// Checks that only administrators and `agent`, the SID of the agent's
/// service, own `dir` and everything in it, that only they may write to
/// `dir` and the package's folders in it, and that it holds no link, whose
/// target would be checked in its place. The error names the first path
/// at fault. Blocking.
pub fn check(dir: &Path, agent: &str) -> Result<(), String> {
    check_writers(dir, agent)?;
    for name in PROTECTED {
        let folder = dir.join(name);
        if folder.is_dir() {
            check_writers(&folder, agent)?;
        }
    }
    owners(dir, agent)
}

fn owners(dir: &Path, agent: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: cannot list it: {e}", dir.display());
    for entry in std::fs::read_dir(dir).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let path = entry.path();
        // Of the entry itself: a link or junction is not followed.
        let kind = entry.file_type().map_err(fail)?;
        if kind.is_symlink() {
            return Err(format!(
                "{} is a link, which does not belong in the agent's folder",
                path.display()
            ));
        }
        check_owner(&path, agent)?;
        if kind.is_dir() {
            owners(&path, agent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SID nobody has, so the test's own account is not the agent.
    const NOBODY: &str = "S-1-0-0";

    fn own_sid(path: &Path) -> String {
        // "… is owned by S-1-5-…, not an administrator or the agent"
        let error = check_owner(path, NOBODY).unwrap_err();
        let sid = error.split("is owned by ").nth(1).unwrap();
        sid.split(',').next().unwrap().to_owned()
    }

    #[test]
    fn a_folder_someone_else_made_is_refused_with_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("MonitorFlow");
        std::fs::create_dir_all(data.join("scripts.d")).unwrap();
        std::fs::write(
            data.join("agent.toml"),
            "upstream_url = \"https://evil.example\"",
        )
        .unwrap();
        let me = own_sid(&data);

        // Ours: every entry belongs to the account passed as the agent.
        check(&data, &me).unwrap();
        // Made by an account that is neither an administrator nor the
        // agent, as a folder created before the install is.
        let error = check(&data, NOBODY).unwrap_err();
        assert!(error.contains("MonitorFlow is owned by"), "{error}");
        assert!(check(&data.join("missing"), &me).is_err());
    }

    #[test]
    fn links_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("MonitorFlow");
        std::fs::create_dir(&data).unwrap();
        let me = own_sid(&data);
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        // A junction needs no privilege, unlike a symbolic link.
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(data.join("agent"))
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        // The junction's target passes the writers check; the walk stops
        // at the junction itself.
        let error = check(&data, &me).unwrap_err();
        assert!(error.contains("is a link"), "{error}");
    }
}
