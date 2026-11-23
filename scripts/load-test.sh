#!/usr/bin/env bash
# Load test of the server (docs/load-test.md): starts PostgreSQL,
# VictoriaMetrics and VictoriaLogs in a compose project of their own, runs
# the release server and relay against `monitorflow-loadtest`'s simulated
# agents, writes the report, and removes the containers.
#
#   scripts/load-test.sh                    # 1,000 agents, every phase
#   scripts/load-test.sh --agents 100 --steady 60
#   LOADTEST_SERVER_CPUS=10,11 LOADTEST_SERVER_MEMORY=4G scripts/load-test.sh --relays 0
#                                           # the server side as a small machine
#
# LOADTEST_SERVER_CPUS (CPU list) and LOADTEST_SERVER_MEMORY (like 4G) put
# the server and the three stores in one systemd slice with those limits
# and no swap, as on a small VM, and keep the generator on the other CPUs.
# That needs sudo: in a terminal it asks once, elsewhere set SUDO_ASKPASS.
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
generator=()
limited=${LOADTEST_SERVER_CPUS:-}${LOADTEST_SERVER_MEMORY:-}
if [[ -n $limited ]]; then
    export LOADTEST_SLICE=${LOADTEST_SLICE:-mfloadtest.slice}
    sudo=(sudo)
    [[ -n ${SUDO_ASKPASS:-} ]] && sudo=(sudo -A)
    props=(MemorySwapMax=0)
    [[ -n ${LOADTEST_SERVER_CPUS:-} ]] && props+=("AllowedCPUs=$LOADTEST_SERVER_CPUS")
    [[ -n ${LOADTEST_SERVER_MEMORY:-} ]] && props+=("MemoryMax=$LOADTEST_SERVER_MEMORY")
    "${sudo[@]}" systemctl set-property --runtime "$LOADTEST_SLICE" "${props[@]}"
    export LOADTEST_STORE_MEMORY=${LOADTEST_SERVER_MEMORY:-0}
    compose+=(-f deploy/docker-compose.loadtest-limited.yml)
    export LOADTEST_CGROUP=/sys/fs/cgroup/$LOADTEST_SLICE
    uid=$(id -u)
    gid=$(id -g)
    export LOADTEST_SERVER_WRAPPER="${sudo[*]} systemd-run --scope --quiet --slice=$LOADTEST_SLICE --unit=mfloadtest-server --uid=$uid --gid=$gid -- env NO_COLOR=1"
    if [[ -n ${LOADTEST_SERVER_CPUS:-} ]]; then
        # The generator on every other CPU.
        others=$(python3 -c '
import os, sys
taken = set()
for part in sys.argv[1].split(","):
    a, _, b = part.partition("-")
    taken.update(range(int(a), int(b or a) + 1))
print(",".join(str(c) for c in sorted(os.sched_getaffinity(0) - taken)))
' "$LOADTEST_SERVER_CPUS")
        generator=(taskset -c "$others")
    fi
fi
cleanup() {
    "${compose[@]}" logs --no-color postgres >target/load-test-postgres.log 2>&1 || true
    "${compose[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
    if [[ -n $limited ]]; then
        "${sudo[@]}" systemctl stop "$LOADTEST_SLICE" >/dev/null 2>&1 || true
        "${sudo[@]}" systemctl revert "$LOADTEST_SLICE" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

if [[ $profile == release ]]; then
    cargo build --release -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=${CARGO_TARGET_DIR:-target}/release
else
    cargo build -p monitorflow-server -p monitorflow-relay -p monitorflow-loadtest --bins
    bin=${CARGO_TARGET_DIR:-target}/debug
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
"${generator[@]}" "$bin/monitorflow-loadtest" --work-dir "$work" "$@"
