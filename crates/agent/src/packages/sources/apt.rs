//! apt: one deb822 file with a stanza per repository, signed by keyrings
//! under `/etc/apt/keyrings/`.

use std::path::{Path, PathBuf};

use super::{list_dir, Error, Spec, HEADER};

pub(super) const SOURCES_LIST: &str = "/etc/apt/sources.list";
pub(super) const PARTS: &str = "/etc/apt/sources.list.d";
pub(super) const SOURCES: &str = "/etc/apt/sources.list.d/monitorflow.sources";
pub(super) const KEYRINGS: &str = "/etc/apt/keyrings";

pub(super) fn key_path(name: &str) -> PathBuf {
    Path::new(KEYRINGS).join(format!("monitorflow-{name}.asc"))
}

/// MonitorFlow's files: the keyrings, then the sources file.
pub(super) fn files(spec: &Spec) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = spec
        .keys
        .iter()
        .map(|(name, key)| (key_path(name), key.armored.clone().into_bytes()))
        .collect();
    let signed_by = spec
        .keys
        .iter()
        .map(|(name, _)| key_path(name).display().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut text = HEADER.to_owned();
    for repo in &spec.apt {
        text.push_str(&format!(
            "\nTypes: deb\nURIs: {}/apt/{}/{}/\nSuites: {}\nComponents: {}\n",
            spec.base,
            repo.mirror,
            repo.snapshot,
            repo.suite,
            repo.components.join(" "),
        ));
        if !repo.architectures.is_empty() {
            text.push_str(&format!(
                "Architectures: {}\n",
                repo.architectures.join(" ")
            ));
        }
        text.push_str(&format!("Signed-By: {signed_by}\n"));
    }
    files.push((PathBuf::from(SOURCES), text.into_bytes()));
    files
}

/// The files apt reads sources from, which `replace` sets aside.
pub(super) fn others(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut paths = vec![PathBuf::from(SOURCES_LIST)];
    paths.extend(list_dir(root, PARTS, &["list", "sources"])?);
    Ok(paths)
}
