//! Code shared by the agent, relay and server: generated gRPC types,
//! TLS helpers, configuration loading and common error types.

pub mod client;
pub mod command;
pub mod config;
pub mod grpc_url;
pub mod logging;
pub mod mirror;
pub mod redact;
pub mod release;
#[cfg(all(unix, feature = "server-update"))]
pub mod server_update;
pub mod tls;
pub mod via;
#[cfg(windows)]
pub mod winacl;

/// Generated Protobuf and gRPC code for the `monitorflow.v1` package.
pub mod proto {
    // Generated code; tonic's client returns `tonic::Status` in a large `Err`.
    #[allow(clippy::result_large_err)]
    pub mod v1 {
        tonic::include_proto!("monitorflow.v1");
    }
}

/// Version of this build: the workspace version, or
/// `MONITORFLOW_BUILD_VERSION` when set at build time, which the
/// self-update test uses to build an older and a newer agent from the same
/// sources (ADR-0053).
/// An empty value counts as unset, as a Docker build argument left empty.
pub const VERSION: &str = match option_env!("MONITORFLOW_BUILD_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};

/// Protocol version spoken by this build. Bump the minor on additive changes.
pub const PROTOCOL_VERSION: &str = "1.1";

/// Oldest agent or relay release the server accepts on its streams
/// (ADR-0055). Raised only by a release whose notes say so, and never past
/// a version automatic updates could not reach.
pub const MIN_PEER_VERSION: &str = "0.1.0";

#[cfg(test)]
mod tests {
    use super::proto::v1::Hello;

    #[test]
    fn hello_round_trips_through_prost() {
        use prost::Message;
        let hello = Hello {
            agent_id: "agent-1".into(),
            protocol_version: super::PROTOCOL_VERSION.into(),
            ..Default::default()
        };
        let bytes = hello.encode_to_vec();
        let decoded = Hello::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded, hello);
    }
}
