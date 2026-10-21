//! Code shared by the agent, relay and server: generated gRPC types,
//! TLS helpers, configuration loading and common error types.

pub mod client;
pub mod command;
pub mod config;
pub mod grpc_url;
pub mod logging;
pub mod redact;
pub mod tls;
pub mod via;

/// Generated Protobuf and gRPC code for the `monitorflow.v1` package.
pub mod proto {
    // Generated code; tonic's client returns `tonic::Status` in a large `Err`.
    #[allow(clippy::result_large_err)]
    pub mod v1 {
        tonic::include_proto!("monitorflow.v1");
    }
}

/// Protocol version spoken by this build. Bump the minor on additive changes.
pub const PROTOCOL_VERSION: &str = "1.1";

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
