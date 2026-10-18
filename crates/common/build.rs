fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../proto";
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                format!("{proto_root}/monitorflow/v1/agent.proto"),
                format!("{proto_root}/monitorflow/v1/enroll.proto"),
            ],
            &[proto_root],
        )?;
    println!("cargo:rerun-if-changed={proto_root}");
    Ok(())
}
