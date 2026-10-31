# syntax=docker/dockerfile:1
# MonitorFlow mirror tools (ADR-0067): aptly, reposync, createrepo_c and gpg
# for the server's package mirrors, behind a socket in the volume they
# share. Needs BuildKit (the buildx plugin). Build from the repository root:
#   docker build -f deploy/docker/mirror.Dockerfile -t monitorflow-mirror .
# The Rust version follows rust-toolchain.toml (ADR-0005): bump both together.

FROM rust:1.99.0-slim-trixie AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_TOOLCHAIN=1.99.0
WORKDIR /src
COPY . .
# The caches of server.Dockerfile, which builds the same crate.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p monitorflow-server --bin monitorflow-mirror-tools \
    && cp target/release/monitorflow-mirror-tools /usr/local/bin/

FROM debian:trixie-slim
# The archive keys of the catalog's apt distributions; dnf clients check
# the vendor signature on each package. The user is the server's: they
# share the mirrors volume.
RUN apt-get update \
    && apt-get install -y --no-install-recommends aptly ca-certificates createrepo-c \
        debian-archive-keyring dnf dnf-plugins-core gnupg ubuntu-keyring \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /usr/share/monitorflow \
    && cat /usr/share/keyrings/debian-archive-keyring.gpg \
        /usr/share/keyrings/ubuntu-archive-keyring.gpg >/usr/share/monitorflow/upstream-keys.gpg \
    && groupadd --system --gid 10001 monitorflow \
    && useradd --system --uid 10001 --gid 10001 --home-dir /tmp --shell /usr/sbin/nologin monitorflow \
    && install -d -o monitorflow -g monitorflow /var/lib/monitorflow/server/mirrors
COPY --from=build /usr/local/bin/monitorflow-mirror-tools /usr/local/bin/
COPY --chmod=0755 deploy/docker/mirror-start.sh /usr/local/bin/mirror-start
COPY --chmod=0755 deploy/docker/reposync.sh /usr/local/bin/reposync
USER monitorflow
ENV HOME=/tmp
ENTRYPOINT ["mirror-start"]
