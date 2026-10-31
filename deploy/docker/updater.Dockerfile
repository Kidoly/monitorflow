# syntax=docker/dockerfile:1
# MonitorFlow updater (ADR-0056): installs server releases on request of
# the server. The only container with the Docker socket. Build from the
# repository root:
#   docker build -f deploy/docker/updater.Dockerfile -t monitorflow-updater .
# The Rust version follows rust-toolchain.toml (ADR-0005): bump both together.

FROM rust:1.99.0-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_TOOLCHAIN=1.99.0
# Locked caches: compose builds the server and updater images at once,
# and two cargo processes must not unpack the same crates together.
# A test build may claim another version (crates/common/src/lib.rs).
ARG MONITORFLOW_BUILD_VERSION=
# Test builds only: keys to trust instead of deploy/release-keys.pem, as
# in server.Dockerfile.
ARG RELEASE_KEYS=""
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    if [ -z "$MONITORFLOW_BUILD_VERSION" ]; then unset MONITORFLOW_BUILD_VERSION; fi \
    && if [ -n "$RELEASE_KEYS" ]; then \
        printf '%s\n' "$RELEASE_KEYS" >/tmp/release-keys.pem \
        && export MONITORFLOW_RELEASE_KEYS=/tmp/release-keys.pem; \
    fi \
    && cargo build --release --locked -p monitorflow-updater \
    && cp target/release/monitorflow-updater /usr/local/bin/

# The Docker CLI and its compose plugin: static binaries.
FROM docker:29.9.2-cli AS docker

FROM debian:trixie-slim
COPY --from=docker /usr/local/bin/docker /usr/local/bin/docker
COPY --from=docker /usr/local/libexec/docker/cli-plugins/docker-compose \
    /usr/local/libexec/docker/cli-plugins/docker-compose
COPY --from=build /usr/local/bin/monitorflow-updater /usr/local/bin/
# The mirrors volume is the server's user's whichever service creates it.
RUN install -d -o 10001 -g 10001 /serverdata/mirrors
# Root: the Docker socket is root's anyway, and the updater keeps the
# update volume's root owned by root (crates/updater/src/lib.rs).
ENTRYPOINT ["monitorflow-updater"]
