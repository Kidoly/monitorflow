//! The license public keys built in (ADR-0100): `ee/license-keys.pem`, or
//! the file `MONITORFLOW_LICENSE_KEYS` names, for test builds that trust a
//! throwaway key. Parsed by `license::Verifier::built_in`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=MONITORFLOW_LICENSE_KEYS");
    let path = match std::env::var_os("MONITORFLOW_LICENSE_KEYS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../license-keys.pem"),
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let keys = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the license keys {}: {e}", path.display()))?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("license-keys.pem");
    std::fs::write(out, keys)?;
    Ok(())
}
