#!/usr/bin/env bash
# End-to-end test of moving from another monitoring tool (ADR-0146),
# against real software in local Docker containers only.
#
# 1. Starts a private MonitorFlow server from this checkout: its own
#    database on the PostgreSQL of DATABASE_URL, its own data directory and
#    ports, the API on loopback only, gRPC and enrollment on the Docker
#    bridge only, so agent containers reach it and nothing else does.
# 2. Imports from the stacks that zabbix/setup.sh and prometheus/setup.sh
#    configured, from their export files and from their live APIs: checks
#    the preview, confirms, checks through the API that servers, checks,
#    thresholds, silences and channels exist and match, and that a second
#    import creates nothing.
# 3. Runs an agent in a container named after an imported host, enrolled
#    with an install key of the site, and checks it takes over that server
#    and its checks.
#
# Usage: run-e2e.sh [zabbix|prometheus|all]   (all by default)
# Needs: the stacks running (run zabbix/setup.sh and prometheus/setup.sh
# first), jq, curl, docker, cargo, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WHICH=${1:-all}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-import-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_import_e2e
API_PORT=${MF_E2E_API_PORT:-17444}
GRPC_PORT=${MF_E2E_GRPC_PORT:-17443}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-17445}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-17480}
API=http://127.0.0.1:$API_PORT
PASSWORD=e2e-import-password-2026
AGENT_NAME=${MF_E2E_AGENT_NAME:-mf-import-e2e-agent}
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

# Builds the server and agent of this checkout.
step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent)
# CARGO_TARGET_DIR may be absolute, or relative to the checkout.
TARGET=${CARGO_TARGET_DIR:-target}
[[ $TARGET == /* ]] || TARGET=$REPO/$TARGET
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent

# The Docker bridge's gateway: what containers reach the host at.
BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "a private MonitorFlow in $WORK"
if [[ -f "$WORK/server.pid" ]] && kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null; then
    kill "$(cat "$WORK/server.pid")"; sleep 2
fi
rm -rf "$WORK"; mkdir -p "$WORK/server"
ADMIN_URL=${DB_URL%/*}
psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME" -c "CREATE DATABASE $DB_NAME" >/dev/null
cat > "$WORK/server.toml" <<EOF
database_url = "$ADMIN_URL/$DB_NAME"
api_cookie_secure = false
data_dir = "$WORK/server"
server_names = ["localhost", "127.0.0.1", "$BRIDGE"]
api_listen_addr = "127.0.0.1:$API_PORT"
grpc_listen_addr = "$BRIDGE:$GRPC_PORT"
enroll_listen_addr = "$BRIDGE:$ENROLL_PORT"
mirror_listen_addr = "127.0.0.1:$MIRROR_PORT"

[tls]
ca_cert = "$WORK/server/tls/ca.pem"
cert = "$WORK/server/tls/cert.pem"
key = "$WORK/server/tls/key.pem"
EOF
echo "$PASSWORD" | NO_COLOR=1 "$SERVER_BIN" --config "$WORK/server.toml" user add --username admin --role admin >/dev/null
NO_COLOR=1 setsid "$SERVER_BIN" --config "$WORK/server.toml" >"$WORK/server.log" 2>&1 </dev/null &
echo $! >"$WORK/server.pid"
for _ in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
curl -sf "$API/api/health" >/dev/null || { tail -20 "$WORK/server.log"; fail "the server did not start"; }
ok "server up on $API"

JAR=$WORK/cookies
curl -sf -c "$JAR" -o /dev/null -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$API/api/auth/login" || fail "sign-in"
get() { curl -sf -b "$JAR" "$API$1"; }
# The body goes on stdin: an export does not fit on a command line.
send() { printf '%s' "$3" | curl -s -b "$JAR" -X "$1" -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' --data-binary @- -w '\n%{http_code}' "$API$2"; }
# The body of a send, failing unless its status is $2.
expect() {
    local out status
    out=$(cat); status=${out##*$'\n'}
    [[ "$status" == "$1" ]] || { echo "${out%$'\n'*}" >&2; fail "expected HTTP $1, got $status"; }
    echo "${out%$'\n'*}"
}

# The site a new server has; one is made if there is none.
SITE=$(get /api/sites | jq -r '.sites[0].id // empty')
[[ -n "$SITE" ]] || SITE=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"main"}' | expect 201 | jq -r .id)
ok "site $SITE"

# Previews `$1` (a JSON request), prints the counts, confirms, and checks a
# second preview of the same request creates nothing. Prints the run.
import_and_check() {
    local name=$1 request=$2 run id second
    run=$(send POST /api/imports "$request" | expect 201)
    id=$(jq -r .id <<<"$run")
    echo "$run" >"$WORK/$name.preview.json"
    echo "preview $name: $(jq -c .counts <<<"$run")"
    [[ $(jq '.counts.to_create' <<<"$run") -gt 0 ]] || fail "$name: nothing to create"
    [[ $(jq '[.items[] | select(.kind == "server")] | length' <<<"$run") -gt 0 ]] || fail "$name: no servers"
    # Nothing exists before the confirmation.
    local before; before=$(get /api/servers | jq '.servers | length')
    run=$(send POST "/api/imports/$id/confirm" '{"skip": []}' | expect 200)
    echo "$run" >"$WORK/$name.confirmed.json"
    echo "confirmed $name: $(jq -c '.result | {created, present, failed: (.failed | length)}' <<<"$run")"
    [[ $(jq '.result.failed | length' <<<"$run") == 0 ]] || { jq '.result.failed' <<<"$run"; fail "$name: items failed"; }
    local after; after=$(get /api/servers | jq '.servers | length')
    [[ $after -gt $before || $(jq '.result.created.server // 0' <<<"$run") == 0 ]] || fail "$name: no server appeared"
    second=$(send POST /api/imports "$request" | expect 201)
    [[ $(jq '.counts.to_create' <<<"$second") == 0 ]] \
        || { jq '[.items[] | select(.present == false and .create != null and .list != "not_movable") | .id]' <<<"$second"; fail "$name: a second import would create again"; }
    run=$(send POST "/api/imports/$(jq -r .id <<<"$second")/confirm" '{}' | expect 200)
    [[ $(jq '.result.created | length' <<<"$run") == 0 ]] || fail "$name: the second import created something"
    ok "$name: imported, and a second import creates nothing"
}

# Checks the server `$1` exists, waits for its agent, and has the check
# named `$2` and a threshold on the metric `$3` (each optional).
check_server() {
    local host=$1 check=${2:-} metric=${3:-} server id
    server=$(get "/api/servers?limit=500" | jq -c --arg h "$host" '.servers[] | select(.hostname == $h or .peer_id == $h)')
    [[ -n "$server" ]] || fail "no server $host"
    id=$(jq -r .id <<<"$server")
    [[ $(jq -r .state <<<"$server") == waiting || $(jq -r .awaiting_agent <<<"$server") == false ]] || fail "$host is not waiting"
    if [[ -n "$check" ]]; then
        get "/api/servers/$id/sensor_configs" | jq -e --arg c "$check" '.entries[] | select(.name | startswith($c))' >/dev/null \
            || fail "$host has no check $check*"
    fi
    if [[ -n "$metric" ]]; then
        get "/api/servers/$id/thresholds" | jq -e --arg m "$metric" '.entries[] | select(.metric == $m)' >/dev/null \
            || fail "$host has no threshold on $metric"
    fi
    ok "$host: waiting for its agent${check:+, check $check*}${metric:+, threshold $metric}"
}

files_request() { # source, then files
    local source=$1; shift
    jq -n --arg s "$source" --arg site "$SITE" '{source: $s, site_id: $site, files: []}' >"$WORK/req.json"
    for f in "$@"; do
        jq --arg n "$(basename "$f")" --rawfile c "$f" '.files += [{name: $n, content: $c}]' "$WORK/req.json" >"$WORK/req2.json"
        mv "$WORK/req2.json" "$WORK/req.json"
    done
    cat "$WORK/req.json"
}

if [[ $WHICH == all || $WHICH == zabbix ]]; then
    step "zabbix"
    ZBX_OUT=${MF_E2E_ZBX_OUT:-$HERE/zabbix/out}
    [[ -f $ZBX_OUT/token ]] || fail "run deploy/import-test/zabbix/setup.sh first"
    for format in yaml xml json; do
        files=("$ZBX_OUT"/*."$format")
        [[ -e ${files[0]} ]] || fail "no $format export in $ZBX_OUT"
        run=$(send POST /api/imports "$(files_request zabbix "${files[@]}")" | expect 201)
        echo "preview zabbix files ($format): $(jq -c .counts <<<"$run")"
        [[ $(jq '[.items[] | select(.kind == "server")] | length' <<<"$run") -ge 2 ]] || fail "zabbix $format: fewer than 2 servers"
    done
    import_and_check zabbix-files "$(files_request zabbix "$ZBX_OUT"/*.yaml)"
    ZBX_URL=$(cat "$ZBX_OUT/url"); ZBX_TOKEN=$(cat "$ZBX_OUT/token")
    import_and_check zabbix-api "$(jq -n --arg site "$SITE" --arg u "$ZBX_URL" --arg t "$ZBX_TOKEN" \
        '{source: "zabbix", site_id: $site, connection: {url: $u, token: $t}, time_zone: "Europe/Paris"}')"
    check_server web-01 port_80 monitorflow_cpu_usage_ratio
    # Web scenarios and HTTP agent items are website checks (ADR-0150).
    check_server web-01 http_ monitorflow_http_127_0_0_1_up
    check_server db-01 service_postgresql monitorflow_port_5432_up
    get /api/silences | jq -e '.silences | length > 0' >/dev/null || fail "zabbix: no silence from the maintenances"
    # Recurring maintenances are maintenance windows (ADR-0151), in the
    # zone the import was given, and tags give the checks.
    windows=$(get /api/maintenance-windows)
    echo "$windows" >"$WORK/zabbix-windows.json"
    jq -e '.windows[] | select(.reason == "Weekly patching" and .repeat == "weekly" and .weekdays == ["sunday"]
        and .start_time == "02:00" and .time_zone == "Europe/Paris")' <<<"$windows" >/dev/null \
        || fail "zabbix: no weekly window for Weekly patching"
    [[ $(jq '[.windows[] | select(.reason == "Database work" and .month_week == 2 and .months == [1, 7]
        and (.checks | map(.name) | index("port_5432")))] | length' <<<"$windows") == 2 ]] \
        || fail "zabbix: Database work is not the second Tuesday and Friday of port_5432"
    jq -e '.windows[] | select(.reason == "Nightly backups" and .repeat == "daily" and .start_time == "01:00")' <<<"$windows" >/dev/null \
        || fail "zabbix: no daily window for Nightly backups"
    ok "zabbix: recurring maintenances are maintenance windows"
    get /api/notifications/channels | jq -e '[.channels[] | select(.kind == "email" or .kind == "webhook")] | length > 0' >/dev/null \
        || fail "zabbix: no channel"
    ok "zabbix: silences and channels"
    # Richer triggers (ADR-0153): what each became, from the preview of the
    # export and the rows created.
    preview=$WORK/zabbix-files.preview.json
    item_of() { jq -c --arg n "$1" '[.items[] | select(.kind == "threshold" and (.origin.name | contains($n)))][0]' "$preview"; }
    for name in "Load high for 15 minutes" \
        "No processor data" "Busy and loaded" "PostgreSQL port check silent"; do
        [[ $(item_of "$name" | jq -r .list) == exact ]] || { item_of "$name"; fail "zabbix: $name is not exact"; }
    done
    [[ $(item_of "Load jumped" | jq -r .note.code) == zabbix_change ]] || { item_of "Load jumped"; fail "zabbix: Load jumped"; }
    # Two forecasts of one disk: the first moves exactly, the other says
    # which took the measure (one rule of each kind per measure).
    disk=$(for n in "Root file system full in 3 days" "Root file system above 95% within a day"; do item_of "$n" | jq -r '.list + " " + (.note.code // "")'; done | sort | tr '\n' '|')
    [[ $disk == "exact |not_movable kind_taken|" ]] || fail "zabbix: the two disk forecasts give $disk"
    case $(item_of "Processor this hour" | jq -r '.list + " " + .note.code') in
        "approximated zabbix_trend" | "not_movable kind_taken") ;;
        *) item_of "Processor this hour"; fail "zabbix: Processor this hour" ;;
    esac
    ok "zabbix: richer triggers are exact, change approximated, a second window of a measure says why it stays"
    kinds_of() {
        local id; id=$(get "/api/servers?limit=500" | jq -r --arg h "$1" '.servers[] | select(.peer_id == $h) | .id' | head -1)
        get "/api/servers/$id/thresholds" | jq -c '[.entries[] | {metric, instance, kind, function, window_s, target, n: (.conditions | length)}]'
    }
    web_kinds=$(kinds_of web-01); db_kinds=$(kinds_of db-01)
    echo "$web_kinds" >"$WORK/zabbix-web-01-thresholds.json"; echo "$db_kinds" >"$WORK/zabbix-db-01-thresholds.json"
    jq -e '.[] | select(.metric == "monitorflow_disk_used_ratio" and .instance == "/" and .kind == "forecast"
        and (.target == 1 or .target == 0.95))' <<<"$web_kinds" >/dev/null || fail "zabbix: no disk forecast on web-01"
    jq -e '.[] | select(.metric == "monitorflow_cpu_load1" and .kind == "window" and .function == "avg" and .window_s == 900)' \
        <<<"$web_kinds" >/dev/null || fail "zabbix: no load window on web-01"
    jq -e '.[] | select(.metric == "monitorflow_cpu_usage_ratio" and .kind == "nodata")' <<<"$web_kinds" >/dev/null \
        || fail "zabbix: no no-data rule on web-01"
    jq -e '.[] | select(.metric == "monitorflow_cpu_load5" and .kind == "value" and .n == 1)' \
        <<<"$db_kinds" >/dev/null || fail "zabbix: Busy and loaded is not a rule with a condition"
    jq -e '.[] | select(.metric == "monitorflow_port_5432_up" and .kind == "nodata")' <<<"$db_kinds" >/dev/null \
        || fail "zabbix: no no-data rule on the PostgreSQL port check"
    ok "zabbix: forecasts, windows, no data and conditions created as rules of their kind"
fi

if [[ $WHICH == all || $WHICH == prometheus ]]; then
    step "prometheus"
    PROM_DIR=$HERE/prometheus/config
    mapfile -t prom_files < <(find "$PROM_DIR" -type f \( -name '*.yml' -o -name '*.yaml' -o -name '*.json' -o -name '*.rules' \) | sort)
    import_and_check prometheus-files "$(files_request prometheus "${prom_files[@]}")"
    # Richer rules (ADR-0153), from richer-alerts.yml.
    preview=$WORK/prometheus-files.preview.json
    rule_item() { jq -c --arg n "$1" '[.items[] | select(.origin.name == $n)][0]' "$preview"; }
    for name in LoadHighFor15Minutes MemoryLowAllWindow LoadJumped NodeLoadMissing BusyAndLoaded; do
        [[ $(rule_item "$name" | jq -r .list) == exact ]] || { rule_item "$name"; fail "prometheus: $name is not exact"; }
    done
    [[ $(rule_item LoadTwiceLastWeek | jq -r .list) == exact ]] || { rule_item LoadTwiceLastWeek; fail "prometheus: LoadTwiceLastWeek"; }
    # Its condition reads free space as space used, as every disk rule does.
    [[ $(rule_item DiskFullIn4Hours | jq -r '.list + " " + .note.code') == "approximated disk_free_as_used" ]] \
        || { rule_item DiskFullIn4Hours; fail "prometheus: DiskFullIn4Hours"; }
    jq -e '.create.kind == "forecast" and .create.target == 1 and .create.critical == 14400 and (.create.conditions | length) == 1' \
        <<<"$(rule_item DiskFullIn4Hours)" >/dev/null || { rule_item DiskFullIn4Hours; fail "prometheus: DiskFullIn4Hours is not a forecast with a condition"; }
    jq -e '.create.kind == "compare" and .create.function == "ratio" and .create.offset_s == 604800 and .create.window_s == 3600' \
        <<<"$(rule_item LoadTwiceLastWeek)" >/dev/null || fail "prometheus: LoadTwiceLastWeek is not a comparison with last week"
    jq -e '.create.kind == "nodata" and .create.critical == 600' <<<"$(rule_item NodeLoadMissing)" >/dev/null \
        || fail "prometheus: NodeLoadMissing is not no data after 10 minutes"
    jq -e '.create.kind == "window" and .create.function == "max"' <<<"$(rule_item MemoryLowAllWindow)" >/dev/null \
        || fail "prometheus: MemoryLowAllWindow is not the highest available over the window"
    ok "prometheus: windows, a comparison with last week, a forecast, absent and and map to richer rules"
    import_and_check prometheus-api "$(jq -n --arg site "$SITE" '{source: "prometheus", site_id: $site,
        connection: {url: "http://127.0.0.1:19090", alertmanager_url: "http://127.0.0.1:19093"}}')"
    check_server web-01 "" monitorflow_cpu_usage_ratio
    # The blackbox http probe is a website check now (ADR-0150).
    found=""
    for id in $(get "/api/servers?limit=500" | jq -r '.servers[].id'); do
        if get "/api/servers/$id/sensor_configs" | jq -e '.entries[] | select(.kind == "http" and .params.url == "http://shop/")' >/dev/null; then
            found=$id; break
        fi
    done
    [[ -n "$found" ]] || fail "prometheus: no website check of http://shop/"
    get "/api/servers/$found/thresholds" | jq -e '[.entries[] | select(.metric == "monitorflow_http_shop_up" or .metric == "monitorflow_http_shop_response_seconds")] | length == 2' >/dev/null \
        || fail "prometheus: the website check of http://shop/ lacks its alerts"
    ok "prometheus: the blackbox http probe is a website check with its alerts"
    get /api/notifications/channels | jq -e '[.channels[] | select(.kind == "slack" or .kind == "teams" or .kind == "discord")] | length > 0' >/dev/null \
        || fail "prometheus: no chat channel"
    ok "prometheus: channels"
fi

step "an agent takes over web-01"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
CHECKS_BEFORE=$(get "/api/servers?limit=500" | jq -r '.servers[] | select(.peer_id == "web-01") | .id' | head -1)
[[ -n "$CHECKS_BEFORE" ]] || fail "no imported web-01"
WEB=$CHECKS_BEFORE
N_BEFORE=$(get "/api/servers/$WEB/sensor_configs" | jq '.entries | length')
mkdir -p "$WORK/agent"
cat >"$WORK/agent.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
docker rm -f $AGENT_NAME >/dev/null 2>&1 || true
# The agent of this checkout, run by the host's own loader and libraries:
# the container gives it the host name web-01 and nothing else.
docker run -d --name $AGENT_NAME --hostname web-01 --user "$(id -u):$(id -g)" \
    -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/agent.toml:/agent/agent.toml:ro" \
    -v "$WORK/agent:/agent/data" -v /usr/lib:/hostlib:ro \
    debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
for _ in $(seq 60); do
    state=$(get "/api/servers/$WEB" | jq -r '.awaiting_agent, .status' | tr '\n' ' ')
    [[ $state == "false online " ]] && break
    sleep 2
done
[[ $state == "false online " ]] || { docker logs --tail 30 $AGENT_NAME; fail "web-01 was not taken over ($state)"; }
N_AFTER=$(get "/api/servers/$WEB/sensor_configs" | jq '.entries | length')
[[ $N_AFTER == "$N_BEFORE" ]] || fail "web-01 had $N_BEFORE checks, now $N_AFTER"
[[ $(get "/api/servers?limit=500" | jq '[.servers[] | select(.hostname == "web-01")] | length') == 1 ]] || fail "a second web-01 appeared"
ok "the agent named web-01 took over the imported server and its $N_AFTER checks"

step "undo"
# The import that created web-01, whose agent has arrived since.
LAST=""
for id in $(get /api/imports | jq -r '.imports[] | select(.state == "confirmed") | .id'); do
    if get "/api/imports/$id" | jq -e '.items[] | select(.id == "server:web-01" and .present == false)' >/dev/null; then
        LAST=$id; break
    fi
done
[[ -n "$LAST" ]] || fail "no import created web-01"
run=$(send POST "/api/imports/$LAST/undo" '' | expect 200)
echo "undo: $(jq -c '.result | {removed, kept_servers}' <<<"$run")"
jq -e '.result.kept_servers | index("web-01")' <<<"$run" >/dev/null || fail "undo did not keep web-01"
ok "undo removed what it made and kept web-01"

echo
echo "All passed. Server: $API (admin / $PASSWORD), log in $WORK/server.log."
echo "Stop: kill \$(cat $WORK/server.pid); docker rm -f $AGENT_NAME"
