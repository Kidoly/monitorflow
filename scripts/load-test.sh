#!/usr/bin/env bash
# Load test of the server (docs/load-test.md): starts PostgreSQL,
# VictoriaMetrics and VictoriaLogs in a compose project of their own, runs
# the release server and relay against `monitorflow-loadtest`'s simulated
# agents, writes the report, and removes the containers.
#
#   scripts/load-test.sh                    # 1,000 agents, every phase
#   scripts/load-test.sh --agents 100 --steady 60
#
# Arguments go to monitorflow-loadtest (`--help` lists them). Ports and the
# project name come from LOADTEST_* variables, with free defaults.
set -euo pipefail

cd "$(dirname "$0")/.."
project=${LOADTEST_PROJECT:-monitorflow-loadtest}
export LOADTEST_POSTGRES_PORT=${LOADTEST_POSTGRES_PORT:-25432}
export LOADTEST_VICTORIAMETRICS_PORT=${LOADTEST_VICTORIAMETRICS_PORT:-28428}
export LOADTEST_VICTORIALOGS_PORT=${LOADTEST_VICTORIALOGS_PORT:-29428}
profile=${LOADTEST_PROFILE:-release}
work=$PWD/target/loadtest
export LOADTEST_STORAGE_TLS_DIR=$work/storage-tls

compose=(docker compose -p "$project" -f deploy/docker-compose.loadtest.yml)
cleanup() {
    "${compose[@]}" logs --no-color postgres >target/load-test-postgres.log 2>&1 || true
    "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

if [[ $profile == release ]]; then
    cargo build --release -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=target/release
else
    cargo build -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=target/debug
fi

export LOADTEST_DATABASE_URL="postgres://monitorflow:monitorflow@127.0.0.1:${LOADTEST_POSTGRES_PORT}/monitorflow"
export LOADTEST_VICTORIAMETRICS_URL="https://127.0.0.1:${LOADTEST_VICTORIAMETRICS_PORT}"
export LOADTEST_VICTORIALOGS_URL="https://127.0.0.1:${LOADTEST_VICTORIALOGS_PORT}"

# The server's CA, then the stores' certificate and password from it, before
# the stores start, as the production compose's storage-certs (ADR-0087).
rm -rf "$work"
mkdir -p "$work/server" "$work/storage-tls"
cat >"$work/storage-certs.toml" <<TOML
database_url = "$LOADTEST_DATABASE_URL"
data_dir = "$work/server"
victoriametrics_url = "$LOADTEST_VICTORIAMETRICS_URL"
victorialogs_url = "$LOADTEST_VICTORIALOGS_URL"
storage_tls_dir = "$LOADTEST_STORAGE_TLS_DIR"
TOML
"$bin/monitorflow-server" --config "$work/storage-certs.toml" storage-certs

"${compose[@]}" up -d --wait
"$bin/monitorflow-loadtest" --work-dir "$work" "$@"
