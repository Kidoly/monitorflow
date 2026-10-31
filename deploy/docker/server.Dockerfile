# syntax=docker/dockerfile:1
# MonitorFlow central server (ADR-0051). Needs BuildKit (the buildx plugin).
# Build from the repository root:
#   docker build -f deploy/docker/server.Dockerfile -t monitorflow-server .
# The Rust version follows rust-toolchain.toml (ADR-0005): bump both together.

FROM rust:1.99.0-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
# Use the image's toolchain rather than installing the editor components
# rust-toolchain.toml lists.
ENV RUSTUP_TOOLCHAIN=1.99.0
# A test build may claim another version (crates/common/src/lib.rs).
ARG MONITORFLOW_BUILD_VERSION=
# Test builds only: PEM public keys to trust for releases instead of
# deploy/release-keys.pem (ADR-0053), like a throwaway key that signs the
# releases deploy/install-test.sh imports.
ARG RELEASE_KEYS=""
WORKDIR /src
COPY . .
# Locked caches: compose builds the server and updater images at once,
# and two cargo processes must not unpack the same crates together.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    if [ -z "$MONITORFLOW_BUILD_VERSION" ]; then unset MONITORFLOW_BUILD_VERSION; fi \
    && if [ -n "$RELEASE_KEYS" ]; then \
        printf '%s\n' "$RELEASE_KEYS" >/tmp/release-keys.pem \
        && export MONITORFLOW_RELEASE_KEYS=/tmp/release-keys.pem; \
    fi \
    && cargo build --release --locked -p monitorflow-server \
    && cp target/release/monitorflow-server /usr/local/bin/

FROM debian:trixie-slim
# ca-certificates for the AI provider; openssl for db-certs. The group
# is fixed too: the updater lets it read backups (ADR-0058).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 monitorflow \
    && useradd --system --uid 10001 --gid 10001 --home-dir /var/lib/monitorflow/server \
        --shell /usr/sbin/nologin monitorflow \
    && install -d -o monitorflow -g monitorflow -m 0700 /var/lib/monitorflow/server \
    && install -d -o monitorflow -g monitorflow /var/lib/monitorflow/server/mirrors
COPY --from=build /usr/local/bin/monitorflow-server /usr/local/bin/
COPY --chmod=0755 deploy/docker/db-certs.sh /usr/local/bin/db-certs
# Defaults only: the compose file sets the rest through MONITORFLOW_* variables.
COPY deploy/config/server.example.toml /etc/monitorflow/server.toml
USER monitorflow
VOLUME /var/lib/monitorflow/server
EXPOSE 7443 7445
ENTRYPOINT ["monitorflow-server"]
