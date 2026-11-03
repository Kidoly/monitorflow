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

compose=(docker compose -p "$project" -f deploy/docker-compose.loadtest.yml)
cleanup() { "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

if [[ $profile == release ]]; then
    cargo build --release -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=target/release
else
    cargo build -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=target/debug
fi

"${compose[@]}" up -d --wait
export LOADTEST_DATABASE_URL="postgres://monitorflow:monitorflow@127.0.0.1:${LOADTEST_POSTGRES_PORT}/monitorflow"
export LOADTEST_VICTORIAMETRICS_URL="http://127.0.0.1:${LOADTEST_VICTORIAMETRICS_PORT}"
export LOADTEST_VICTORIALOGS_URL="http://127.0.0.1:${LOADTEST_VICTORIALOGS_PORT}"
"$bin/monitorflow-loadtest" "$@"
