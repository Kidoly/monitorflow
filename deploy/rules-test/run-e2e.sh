#!/usr/bin/env bash
# End-to-end test of richer alert rules (ADR-0153), against real software
# in local Docker containers only.
#
# 1. Starts a private MonitorFlow server from this checkout (its own
#    database on the PostgreSQL of DATABASE_URL, its own VictoriaMetrics and
#    VictoriaLogs, the API on loopback, gRPC and enrollment on the Docker
#    bridge only) and a mail catcher (Mailpit) as its email channel.
# 2. Runs a real agent in a container, web-01, with two small tmpfs
#    disks it reports: /fill, filled steadily, and /spike.
# 3. Checks through the API and the mail catcher that:
#    - the suggested disk forecast, added in one call, opens a forecast
#      incident on /fill before it is full, with an expected date that
#      matches how fast it fills;
#    - a window average over 3 minutes ignores a spike of /spike and
#      catches a sustained one, and the preview says the same;
#    - a check that stops sending opens "no data", and its return resolves
#      it;
#    - a restart of the server keeps every incident, without duplicates.
# 4. Leaves everything running for the browser check (browser.mjs) unless
#    MF_RULES_E2E_STOP=1.
#
# Usage: run-e2e.sh                      (about 25 minutes: a forecast learns
#                                        over a quarter of its hour first)
# Needs: jq, curl, docker, cargo, psql, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-rules-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_rules_e2e
API_PORT=${MF_E2E_API_PORT:-27844}
GRPC_PORT=27843
ENROLL_PORT=27845
MIRROR_PORT=27880
SMTP_PORT=27825
MAIL_API_PORT=27826
VM_PORT=27828
VL_PORT=27829
API=http://127.0.0.1:$API_PORT
MAIL=http://127.0.0.1:$MAIL_API_PORT
PASSWORD=e2e-rules-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
CONTAINERS=(mf-rules-mail mf-rules-vm mf-rules-vl mf-rules-web-01)
MIB=1048576

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $(date +%H:%M:%S) $*"; }

for port in $API_PORT $GRPC_PORT $ENROLL_PORT $SMTP_PORT $MAIL_API_PORT $VM_PORT $VL_PORT; do
    if ss -ltn | grep -q ":$port "; then
        docker ps --format '{{.Names}}' | grep -q '^mf-rules-' || fail "port $port is busy"
    fi
done

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent)
TARGET=${CARGO_TARGET_DIR:-target}
[[ $TARGET == /* ]] || TARGET=$REPO/$TARGET
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "a private MonitorFlow with its stores, and a mail catcher"
if [[ -f "$WORK/server.pid" ]] && kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null; then
    pid=$(cat "$WORK/server.pid"); kill "$pid"
    for _ in $(seq 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
fi
docker rm -f "${CONTAINERS[@]}" >/dev/null 2>&1 || true
rm -rf "$WORK"; mkdir -p "$WORK/server"
docker run -d --name mf-rules-mail -p "$BRIDGE:$SMTP_PORT:1025" -p "127.0.0.1:$MAIL_API_PORT:8025" \
    axllent/mailpit:v1.21 >/dev/null
docker run -d --name mf-rules-vm -p "127.0.0.1:$VM_PORT:8428" \
    victoriametrics/victoria-metrics:latest -retentionPeriod=1d >/dev/null
docker run -d --name mf-rules-vl -p "127.0.0.1:$VL_PORT:9428" victoriametrics/victoria-logs:latest >/dev/null

ADMIN_URL=${DB_URL%/*}
psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" -c "CREATE DATABASE $DB_NAME" >/dev/null
cat > "$WORK/server.toml" <<EOF
database_url = "$ADMIN_URL/$DB_NAME"
api_cookie_secure = false
data_dir = "$WORK/server"
server_names = ["localhost", "127.0.0.1", "$BRIDGE"]
api_listen_addr = "127.0.0.1:$API_PORT"
grpc_listen_addr = "$BRIDGE:$GRPC_PORT"
enroll_listen_addr = "$BRIDGE:$ENROLL_PORT"
mirror_listen_addr = "127.0.0.1:$MIRROR_PORT"
victoriametrics_url = "http://127.0.0.1:$VM_PORT"
victorialogs_url = "http://127.0.0.1:$VL_PORT"

[tls]
ca_cert = "$WORK/server/tls/ca.pem"
cert = "$WORK/server/tls/cert.pem"
key = "$WORK/server/tls/key.pem"
EOF
start_server() {
    NO_COLOR=1 setsid "$SERVER_BIN" --config "$WORK/server.toml" >>"$WORK/server.log" 2>&1 </dev/null &
    echo $! >"$WORK/server.pid"
    for _ in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
    curl -sf "$API/api/health" >/dev/null || { tail -20 "$WORK/server.log"; fail "the server did not start"; }
}
echo "$PASSWORD" | NO_COLOR=1 "$SERVER_BIN" --config "$WORK/server.toml" user add --username admin --role admin >/dev/null
start_server
for _ in $(seq 30); do curl -sf "$MAIL/api/v1/messages" >/dev/null && break; sleep 1; done
ok "server on $API, Mailpit on $MAIL"

JAR=$WORK/cookies
sign_in() {
    curl -sf -c "$JAR" -o /dev/null -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' \
        -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$API/api/auth/login" || fail "sign-in"
}
sign_in
get() { curl -sf -b "$JAR" "$API$1"; }
send() { printf '%s' "$3" | curl -s -b "$JAR" -X "$1" -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' --data-binary @- -w '\n%{http_code}' "$API$2"; }
expect() {
    local out status
    out=$(cat); status=${out##*$'\n'}
    [[ "$status" == "$1" ]] || { echo "${out%$'\n'*}" >&2; fail "expected HTTP $1, got $status"; }
    echo "${out%$'\n'*}"
}

SITE=$(get /api/sites | jq -r '.sites[0].id // empty')
[[ -n "$SITE" ]] || SITE=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"main"}' | expect 201 | jq -r .id)
send POST /api/notifications/channels "$(jq -n --arg h "$BRIDGE" --argjson p "$SMTP_PORT" \
    '{kind: "email", settings: {host: $h, port: $p, username: "", from: "monitorflow@example.com", to: ["ops@example.com"]}}')" \
    | expect 201 >/dev/null
ok "an email channel to Mailpit"

step "an agent, web-01, with two small disks"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
mkdir -p "$WORK/web-01"
cat >"$WORK/web-01.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
# The agent of this checkout, run by the host's own loader and libraries.
docker run -d --name mf-rules-web-01 --hostname web-01 --user "$(id -u):$(id -g)" \
    --tmpfs /fill:size=48m,mode=1777 --tmpfs /spike:size=32m,mode=1777 \
    -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/web-01.toml:/agent/agent.toml:ro" \
    -v "$WORK/web-01:/agent/data" -v /usr/lib:/hostlib:ro \
    debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
for _ in $(seq 90); do
    WEB=$(get "/api/servers?limit=500" | jq -r '.servers[] | select(.hostname == "web-01") | .id')
    [[ -n "$WEB" && $(get "/api/servers/$WEB" | jq -r .status) == online ]] && break
    sleep 2
done
[[ $(get "/api/servers/$WEB" | jq -r .status) == online ]] || { docker logs --tail 20 mf-rules-web-01; fail "web-01 is not online"; }
# The disk check reports tmpfs here, every 10 s, without the host's own
# file systems bound into the container.
send POST "/api/servers/$WEB/sensor_configs" '{"kind": "disk", "name": "disk", "interval_ms": 10000,
    "params": {"fstypes_include": "tmpfs", "mounts_exclude": "/etc,/agent,/dev,/sys,/proc,/run,/tmp"}}' | expect 201 >/dev/null
# A port check whose data will stop.
send POST "/api/servers/$WEB/sensor_configs" '{"kind": "port", "name": "port_7", "interval_ms": 10000,
    "params": {"host": "127.0.0.1", "port": "7"}}' | expect 201 >/dev/null
fill_used() {
    get "/api/metrics/query?server_id=$WEB&metric=monitorflow_disk_used_ratio&instance=/fill" | jq -r '.series[0].points[0][1] // empty'
}
for _ in $(seq 60); do [[ -n $(fill_used) ]] && break; sleep 3; done
[[ -n $(fill_used) ]] || fail "no /fill in the metrics store"
ok "web-01 online; /fill and /spike reported"

step "the suggested disk forecast, added in one call"
offer=$(get "/api/sites/$SITE/thresholds/effective" | jq -c '.recommended[] | select(.key == "disk_forecast") | .rule')
[[ -n "$offer" ]] || fail "no disk forecast suggested"
send POST "/api/sites/$SITE/thresholds" "$offer" | expect 201 >/dev/null
[[ $(get "/api/sites/$SITE/thresholds/effective" | jq '.recommended | length') == 0 ]] || fail "still suggested once added"
ok "the forecast is a site rule: $(jq -c '{warning, critical, window_s, target}' <<<"$offer")"
# It learns over a day, so judges after 6 hours: /fill gets the same rule
# learned over an hour, which judges after 15 minutes.
send POST "/api/servers/$WEB/thresholds" "$(jq -c '. + {instance: "/fill", window_s: 3600}' <<<"$offer")" | expect 201 >/dev/null
ok "and /fill the same, learned over an hour"
# 1 MiB a minute: full in about 47 minutes.
docker exec -d mf-rules-web-01 sh -c 'i=0; while :; do head -c 1048576 /dev/zero >/fill/f$i; i=$((i+1)); sleep 60; done'
FILL_STARTED=$(date +%s)
ok "/fill grows 1 MiB a minute"

step "a window average ignores a spike and catches a sustained load"
send POST "/api/servers/$WEB/thresholds" '{"metric": "monitorflow_disk_used_ratio", "instance": "/spike",
    "kind": "window", "function": "avg", "window_s": 180, "comparison": ">", "critical": 0.5}' | expect 201 >/dev/null
window_incident() {
    get "/api/incidents?limit=200&server_id=$WEB" | jq -c '.incidents[] | select(.kind == "window" and .instance == "/spike")'
}
docker exec mf-rules-web-01 sh -c 'head -c 29360128 /dev/zero >/spike/burst; sleep 15; rm /spike/burst'
sleep 200
[[ -z $(window_incident) ]] || fail "the spike opened a window incident"
ok "a 15 s spike to 87% leaves the 3-minute average below 50%: no incident"
docker exec mf-rules-web-01 sh -c 'head -c 29360128 /dev/zero >/spike/held'
SUSTAINED=$(date +%s)
for _ in $(seq 60); do [[ -n $(window_incident) ]] && break; sleep 5; done
incident=$(window_incident)
[[ -n "$incident" ]] || fail "the sustained load opened no window incident"
after=$(( $(date -d "$(jq -r .opened_at <<<"$incident")" +%s) - SUSTAINED ))
echo "window incident after ${after}s: $(jq -r .reason <<<"$incident")"
[[ $after -ge 60 && $after -le 180 ]] || fail "the average crossed after ${after}s, expected 1.5 min"
mailed() { curl -sf "$MAIL/api/v1/search?query=$(jq -rn --arg q "$1" '$q|@uri')" | jq '.messages_count'; }
for _ in $(seq 40); do [[ $(mailed "Average over 3 minutes") -gt 0 ]] && break; sleep 3; done
[[ $(mailed "Average over 3 minutes") -gt 0 ]] || fail "no mail about the average"
ok "a sustained 87% opens it within the 3 minutes, and the mail says so"
preview=$(send POST "/api/servers/$WEB/thresholds/preview" '{"days": 1, "rule": {"metric": "monitorflow_disk_used_ratio",
    "instance": "/spike", "kind": "window", "function": "avg", "window_s": 180, "comparison": ">", "critical": 0.5}}' | expect 200)
echo "$preview" >"$WORK/preview-window.json"
[[ $(jq '[.series[].incidents[]] | length' <<<"$preview") == 1 ]] \
    || { jq -c '.series[] | {instance, incidents}' <<<"$preview"; fail "the preview does not show the one incident"; }
ok "the preview replays the same: one incident, the spike ignored"

step "a check that stops sending opens no data"
send POST "/api/servers/$WEB/thresholds" '{"metric": "monitorflow_port_7_up", "instance": "", "kind": "nodata",
    "critical": 60}' | expect 201 >/dev/null
port=$(get "/api/servers/$WEB/sensor_configs" | jq -r '.entries[] | select(.name == "port_7") | .id')
sleep 20
send DELETE "/api/sensor_configs/$port" '' | expect 204 >/dev/null
STOPPED=$(date +%s)
nodata_incident() {
    get "/api/incidents?limit=200&server_id=$WEB" | jq -c --arg s "$1" '.incidents[] | select(.kind == "nodata" and .state == $s)'
}
for _ in $(seq 40); do [[ -n $(nodata_incident open) ]] && break; sleep 5; done
incident=$(nodata_incident open)
[[ -n "$incident" ]] || fail "no no-data incident"
after=$(( $(date +%s) - STOPPED ))
echo "no data after ~${after}s: $(jq -r .reason <<<"$incident")"
[[ $after -le 150 ]] || fail "no data took ${after}s for a rule of 60 s"
for _ in $(seq 40); do [[ $(mailed "No data for") -gt 0 ]] && break; sleep 3; done
[[ $(mailed "No data for") -gt 0 ]] || fail "no mail about no data"
send POST "/api/servers/$WEB/sensor_configs" '{"kind": "port", "name": "port_7", "interval_ms": 10000,
    "params": {"host": "127.0.0.1", "port": "7"}}' | expect 201 >/dev/null
for _ in $(seq 40); do [[ -n $(nodata_incident resolved) ]] && break; sleep 3; done
[[ -n $(nodata_incident resolved) ]] || fail "the check came back and no data stayed open"
ok "no data opened within ${after}s, mailed, and resolved when the check came back"

step "the forecast opens before /fill is full, with a sensible date"
forecast_incident() {
    get "/api/incidents?limit=200&server_id=$WEB" | jq -c '.incidents[] | select(.kind == "forecast" and .instance == "/fill" and .state == "open")'
}
for _ in $(seq 240); do [[ -n $(forecast_incident) ]] && break; sleep 5; done
incident=$(forecast_incident)
[[ -n "$incident" ]] || fail "no forecast incident on /fill"
echo "$incident" >"$WORK/forecast.json"
now=$(date +%s)
used=$(fill_used)
expected=$(date -d "$(jq -r .expected_at <<<"$incident")" +%s)
# What is left, at 1 MiB a minute.
left=$(jq -n --argjson u "$used" '(1 - $u) * 48 * 60 | floor')
truth=$((now + left))
echo "forecast after $(( (now - FILL_STARTED) / 60 )) min: /fill $(jq -n --argjson u "$used" '$u * 100 | floor')% used," \
    "expected $(date -d "@$expected" +%H:%M), true pace $(date -d "@$truth" +%H:%M): $(jq -r .reason <<<"$incident")"
[[ $used != 1 && $(jq -n --argjson u "$used" '$u < 0.99') == true ]] || fail "/fill is already full"
diff=$(( expected > truth ? expected - truth : truth - expected ))
[[ $diff -le 600 ]] || fail "the expected date is $((diff / 60)) min off"
[[ $(jq -r .severity <<<"$incident") == critical ]] || fail "less than a day left should be critical"
for _ in $(seq 40); do [[ $(mailed "Expected to reach 100%") -gt 0 ]] && break; sleep 3; done
[[ $(mailed "Expected to reach 100%") -gt 0 ]] || fail "no mail about the forecast"
ok "forecast open while /fill has room, expected within $((diff / 60)) min of the true pace, mailed"

step "a restart keeps every incident, without duplicates"
before=$(get "/api/incidents?state=open&limit=200&server_id=$WEB" | jq -c '[.incidents[] | {kind, instance, id}] | sort_by(.kind, .instance)')
kill "$(cat "$WORK/server.pid")"
for _ in $(seq 30); do kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null || break; sleep 1; done
start_server
sign_in
sleep 60
after=$(get "/api/incidents?state=open&limit=200&server_id=$WEB" | jq -c '[.incidents[] | {kind, instance, id}] | sort_by(.kind, .instance)')
[[ "$before" == "$after" ]] || { echo "before: $before"; echo "after:  $after"; fail "the restart changed the open incidents"; }
psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT count(*) FROM incidents WHERE state = 'open'
    GROUP BY server_id, metric, instance, kind HAVING count(*) > 1" | grep -q . && fail "duplicate open incidents"
ok "same open incidents after a restart: $(jq -c 'map(.kind)' <<<"$after")"

step "audit"
actions=$(psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT string_agg(DISTINCT details->>'kind', ' ') FROM audit_log
    WHERE action = 'threshold.create'")
echo "thresholds created, by kind: $actions"
for kind in forecast window nodata; do [[ " $actions " == *" $kind "* ]] || fail "no audited $kind threshold"; done
ok "every rule added is in the audit log with its kind"

echo
echo "All passed. Server: $API (admin / $PASSWORD), Mailpit: $MAIL, log in $WORK/server.log."
if [[ ${MF_RULES_E2E_STOP:-0} == 1 ]]; then
    kill "$(cat "$WORK/server.pid")"
    docker rm -f "${CONTAINERS[@]}" >/dev/null
    psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" >/dev/null || true
else
    echo "Stop: kill \$(cat $WORK/server.pid); docker rm -f ${CONTAINERS[*]}"
fi
