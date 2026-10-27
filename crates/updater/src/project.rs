//! The compose project the updater belongs to (ADR-0056), found from the
//! labels compose puts on its own container. The project directory is
//! mounted at [`Project::mount`]; [`Project::link`] makes the host's path
//! of that directory lead to the mount inside this container, so compose
//! resolves relative paths to the same host paths as for the operator.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub const LABEL_PROJECT: &str = "com.docker.compose.project";
pub const LABEL_WORKING_DIR: &str = "com.docker.compose.project.working_dir";
pub const LABEL_CONFIG_FILES: &str = "com.docker.compose.project.config_files";
pub const LABEL_ENV_FILE: &str = "com.docker.compose.project.environment_file";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("this container has no compose label {0}: is the updater run by docker compose?")]
    MissingLabel(&'static str),
    #[error("{0} is not an absolute path under the project directory {1}")]
    Outside(String, String),
    #[error("cannot link {0} to the project mount: {1}")]
    Link(String, std::io::Error),
}

/// What the labels say, in host paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub working_dir: PathBuf,
    pub config_files: Vec<PathBuf>,
    pub env_file: PathBuf,
    /// Where the project directory is mounted in this container.
    pub mount: PathBuf,
}

impl Project {
    pub fn from_labels(
        labels: &HashMap<String, String>,
        mount: &Path,
    ) -> Result<Self, ProjectError> {
        let label = |name: &'static str| {
            labels
                .get(name)
                .filter(|v| !v.is_empty())
                .ok_or(ProjectError::MissingLabel(name))
        };
        let name = label(LABEL_PROJECT)?.clone();
        let working_dir = PathBuf::from(label(LABEL_WORKING_DIR)?);
        let inside = |path: &str| -> Result<PathBuf, ProjectError> {
            let path = PathBuf::from(path);
            let plain = path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
            if plain && path.starts_with(&working_dir) {
                Ok(path)
            } else {
                Err(ProjectError::Outside(
                    path.display().to_string(),
                    working_dir.display().to_string(),
                ))
            }
        };
        // The project directory itself must be a plain absolute path.
        inside(&working_dir.display().to_string())?;
        let config_files = label(LABEL_CONFIG_FILES)?
            .split(',')
            .map(inside)
            .collect::<Result<Vec<_>, _>>()?;
        // Several environment files are possible; the pins go in the first.
        let env_file = match labels.get(LABEL_ENV_FILE).filter(|v| !v.is_empty()) {
            Some(files) => inside(files.split(',').next().unwrap_or_default())?,
            None => working_dir.join(".env"),
        };
        Ok(Self {
            name,
            working_dir,
            config_files,
            env_file,
            mount: mount.to_owned(),
        })
    }

    /// `path`, a host path in the project, as seen in this container.
    pub fn local(&self, path: &Path) -> PathBuf {
        match path.strip_prefix(&self.working_dir) {
            Ok(rest) => self.mount.join(rest),
            Err(_) => path.to_owned(),
        }
    }

    /// Creates the host's path of the project directory in this container
    /// as a link to the mount, unless it is the mount itself or already
    /// that link.
    pub fn link(&self) -> Result<(), ProjectError> {
        if self.working_dir == self.mount {
            return Ok(());
        }
        let fail = |e| ProjectError::Link(self.working_dir.display().to_string(), e);
        if std::fs::read_link(&self.working_dir).is_ok_and(|target| target == self.mount) {
            return Ok(());
        }
        if let Some(parent) = self.working_dir.parent() {
            std::fs::create_dir_all(parent).map_err(fail)?;
        }
        std::os::unix::fs::symlink(&self.mount, &self.working_dir).map_err(fail)
    }

    /// `docker compose` options naming this project.
    pub fn compose_args(&self) -> Vec<String> {
        let mut args = vec![
            "compose".to_owned(),
            "--project-name".to_owned(),
            self.name.clone(),
            "--project-directory".to_owned(),
            self.working_dir.display().to_string(),
            "--env-file".to_owned(),
            self.env_file.display().to_string(),
        ];
        for file in &self.config_files {
            args.push("--file".to_owned());
            args.push(file.display().to_string());
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            (LABEL_PROJECT, "monitorflow"),
            (LABEL_WORKING_DIR, "/home/op/monitorflow/deploy"),
            (
                LABEL_CONFIG_FILES,
                "/home/op/monitorflow/deploy/docker-compose.yml,/home/op/monitorflow/deploy/docker-compose.override.yml",
            ),
        ]
    }

    #[test]
    fn reads_the_compose_labels() {
        let project = Project::from_labels(&labels(&base()), Path::new("/deploy")).unwrap();
        assert_eq!(
            project.env_file,
            Path::new("/home/op/monitorflow/deploy/.env")
        );
        assert_eq!(project.local(&project.env_file), Path::new("/deploy/.env"));
        let args = project.compose_args();
        assert_eq!(
            args[..7],
            [
                "compose",
                "--project-name",
                "monitorflow",
                "--project-directory",
                "/home/op/monitorflow/deploy",
                "--env-file",
                "/home/op/monitorflow/deploy/.env"
            ]
        );
        assert_eq!(args.iter().filter(|a| *a == "--file").count(), 2);

        let mut with_env = base();
        with_env.push((LABEL_ENV_FILE, "/home/op/monitorflow/deploy/prod.env"));
        let project = Project::from_labels(&labels(&with_env), Path::new("/deploy")).unwrap();
        assert_eq!(
            project.local(&project.env_file),
            Path::new("/deploy/prod.env")
        );
    }

    #[test]
    fn refuses_files_outside_the_project_and_missing_labels() {
        for (label, value) in [
            (LABEL_CONFIG_FILES, "/etc/other/compose.yml"),
            (LABEL_CONFIG_FILES, "/home/op/monitorflow/deploy/../x.yml"),
            (LABEL_ENV_FILE, "/dev/null"),
            (LABEL_WORKING_DIR, "deploy"),
        ] {
            let mut pairs = base();
            pairs.retain(|(k, _)| *k != label);
            pairs.push((label, value));
            assert!(
                Project::from_labels(&labels(&pairs), Path::new("/deploy")).is_err(),
                "{label}={value}"
            );
        }
        let mut pairs = base();
        pairs.remove(0);
        assert!(matches!(
            Project::from_labels(&labels(&pairs), Path::new("/deploy")),
            Err(ProjectError::MissingLabel(LABEL_PROJECT))
        ));
    }

    #[test]
    fn links_the_host_path_to_the_mount() {
        let root = tempfile::TempDir::new().unwrap();
        let mount = root.path().join("deploy");
        std::fs::create_dir(&mount).unwrap();
        std::fs::write(mount.join(".env"), "A=1\n").unwrap();
        let host = root.path().join("home/op/monitorflow/deploy");
        let mut pairs = base();
        pairs.retain(|(k, _)| *k == LABEL_PROJECT);
        let working_dir = host.display().to_string();
        let config = format!("{working_dir}/docker-compose.yml");
        pairs.push((LABEL_WORKING_DIR, &working_dir));
        pairs.push((LABEL_CONFIG_FILES, &config));
        let project = Project::from_labels(&labels(&pairs), &mount).unwrap();
        project.link().unwrap();
        project.link().unwrap();
        assert_eq!(std::fs::read_to_string(host.join(".env")).unwrap(), "A=1\n");
    }
}
