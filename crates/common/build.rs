use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                format!("{proto_root}/monitorflow/v1/agent.proto"),
                format!("{proto_root}/monitorflow/v1/enroll.proto"),
                format!("{proto_root}/monitorflow/v1/certificate.proto"),
                format!("{proto_root}/monitorflow/v1/packages.proto"),
                format!("{proto_root}/monitorflow/v1/sources.proto"),
            ],
            &[proto_root],
        )?;
    println!("cargo:rerun-if-changed={proto_root}");
    release_keys()
}

/// The release signing keys built in (ADR-0053): `deploy/release-keys.pem`,
/// or the file `MONITORFLOW_RELEASE_KEYS` names, for test builds that
/// trust a throwaway key. Parsed by `release::trusted_keys`.
fn release_keys() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=MONITORFLOW_RELEASE_KEYS");
    let path = match std::env::var_os("MONITORFLOW_RELEASE_KEYS") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?)
            .join("../../deploy/release-keys.pem"),
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let keys = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read the release keys {}: {e}", path.display()))?;
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("release-keys.pem");
    std::fs::write(out, keys)?;
    Ok(())
}
