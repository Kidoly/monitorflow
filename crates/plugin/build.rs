use std::path::PathBuf;

/// The catalogue keys built in (ADR-0103): `deploy/plugin-keys.pem`, or the
/// file `MONITORFLOW_PLUGIN_KEYS` names, for builds that trust another.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=MONITORFLOW_PLUGIN_KEYS");
    let path = match std::env::var_os("MONITORFLOW_PLUGIN_KEYS") {
        Some(path) => PathBuf::from(path),
        None => {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../deploy/plugin-keys.pem")
        }
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let keys = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the plugin keys {}: {e}", path.display()))?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("plugin-keys.pem");
    std::fs::write(out, keys)?;
    Ok(())
}
