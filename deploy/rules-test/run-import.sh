#!/usr/bin/env bash
# The import half of the richer rules test (ADR-0153): a Zabbix of its own
# (a renamed copy of deploy/import-test/zabbix, on 127.0.0.1:18180, so a
# Zabbix another test runs is left alone) configured with richer triggers,
# then deploy/import-test/run-e2e.sh against it and the Prometheus rule
# files, which include richer-alerts.yml. Local Docker only.
#
# Usage: run-import.sh [zabbix|prometheus|all]
# Needs: what deploy/import-test/run-e2e.sh needs; for prometheus, its
# stack running (deploy/import-test/prometheus/setup.sh).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
IMPORT=$(cd "$HERE/../import-test" && pwd)
WHICH=${1:-all}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-rules-import}
ZBX=$WORK/zabbix
mkdir -p "$WORK"

if [[ $WHICH == all || $WHICH == zabbix ]]; then
    rm -rf "$ZBX"; mkdir -p "$ZBX"
    for f in compose.yml setup.sh; do
        sed -e 's/mf-import-zbx/mf-rules-zbx/g' -e 's/18080/18180/g' "$IMPORT/zabbix/$f" >"$ZBX/$f"
    done
    chmod +x "$ZBX/setup.sh"
    "$ZBX/setup.sh" "$ZBX/out"
fi

MF_E2E_ZBX_OUT=$ZBX/out MF_E2E_DIR=$WORK/e2e MF_E2E_AGENT_NAME=mf-rules-import-agent \
    MF_E2E_API_PORT=27944 MF_E2E_GRPC_PORT=27943 MF_E2E_ENROLL_PORT=27945 MF_E2E_MIRROR_PORT=27980 \
    "$IMPORT/run-e2e.sh" "$WHICH"

echo "Stop the Zabbix: docker compose -p mf-rules-zbx -f $ZBX/compose.yml down -v"
