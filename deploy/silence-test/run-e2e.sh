#!/usr/bin/env bash
# End-to-end test of precise and recurring silences (ADR-0151), against
# real software in local Docker containers only.
#
# 1. Starts a private MonitorFlow server from this checkout (its own
#    database on the PostgreSQL of DATABASE_URL, the API on loopback, gRPC
#    and enrollment on the Docker bridge only), a mail catcher (Mailpit)
#    and a webhook receiver, both set up as notification channels.
# 2. Runs two real agents in containers, web-01 and db-01, enrolled with
#    an install key, and gives them port checks that fail.
# 3. Checks, through the API and the two receivers, that a check failing
#    outside any silence is sent; that a silence of one check holds only
#    that check's incident and tells it once at its end; that a running
#    maintenance window holds its server and says "in maintenance", and
#    tells what is still open when it goes; and that a window not running
#    holds nothing.
# 4. Leaves everything running for the browser check (browser.mjs) unless
#    MF_SILENCE_E2E_STOP=1.
#
# Usage: run-e2e.sh
# Needs: jq, curl, docker, cargo, psql, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-silence-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_silence_e2e
API_PORT=${MF_E2E_API_PORT:-27644}
GRPC_PORT=27643
ENROLL_PORT=27645
MIRROR_PORT=27680
SMTP_PORT=27625
MAIL_API_PORT=27626
HOOK_PORT=27627
API=http://127.0.0.1:$API_PORT
MAIL=http://127.0.0.1:$MAIL_API_PORT
PASSWORD=e2e-silence-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
ZONE=Europe/Paris

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent)
# CARGO_TARGET_DIR may be absolute, or relative to the checkout.
TARGET=${CARGO_TARGET_DIR:-target}
[[ $TARGET == /* ]] || TARGET=$REPO/$TARGET
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "a private MonitorFlow, a mail catcher and a webhook receiver"
if [[ -f "$WORK/server.pid" ]] && kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null; then
    pid=$(cat "$WORK/server.pid"); kill "$pid"
    for _ in $(seq 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
fi
docker rm -f mf-silence-mail mf-silence-hook mf-silence-web-01 mf-silence-db-01 >/dev/null 2>&1 || true
rm -rf "$WORK"; mkdir -p "$WORK/server" "$WORK/hook"
: >"$WORK/hook/received.jsonl"
docker run -d --name mf-silence-mail -p "$BRIDGE:$SMTP_PORT:1025" -p "127.0.0.1:$MAIL_API_PORT:8025" \
    axllent/mailpit:v1.21 >/dev/null
docker run -d --name mf-silence-hook --user "$(id -u):$(id -g)" -p "$BRIDGE:$HOOK_PORT:8080" \
    -v "$HERE/hook.py:/hook.py:ro" -v "$WORK/hook:/out" python:3.12-slim python3 /hook.py /out/received.jsonl >/dev/null

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
for _ in $(seq 30); do curl -sf "$MAIL/api/v1/messages" >/dev/null && break; sleep 1; done
ok "server on $API, Mailpit on $MAIL, webhook receiver on $BRIDGE:$HOOK_PORT"

JAR=$WORK/cookies
curl -sf -c "$JAR" -o /dev/null -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$API/api/auth/login" || fail "sign-in"
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
send POST /api/notifications/channels "$(jq -n --arg u "http://$BRIDGE:$HOOK_PORT/hook" '{kind: "webhook", settings: {url: $u}}')" \
    | expect 201 >/dev/null
ok "an email channel to Mailpit and a webhook channel"

step "two agents: web-01 and db-01"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
agent() {
    local name=$1
    mkdir -p "$WORK/$name"
    cat >"$WORK/$name.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
    # The agent of this checkout, run by the host's own loader and
    # libraries: the container gives it its host name and nothing else.
    docker run -d --name "mf-silence-$name" --hostname "$name" --user "$(id -u):$(id -g)" \
        -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/$name.toml:/agent/agent.toml:ro" \
        -v "$WORK/$name:/agent/data" -v /usr/lib:/hostlib:ro \
        debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
}
agent web-01
agent db-01
server_id() { get "/api/servers?limit=500" | jq -r --arg h "$1" '.servers[] | select(.hostname == $h) | .id'; }
for _ in $(seq 90); do
    WEB=$(server_id web-01); DB=$(server_id db-01)
    if [[ -n "$WEB" && -n "$DB" ]] \
        && [[ $(get "/api/servers/$WEB" | jq -r .status) == online && $(get "/api/servers/$DB" | jq -r .status) == online ]]; then
        break
    fi
    sleep 2
done
[[ $(get "/api/servers/$WEB" | jq -r .status) == online ]] || { docker logs --tail 20 mf-silence-web-01; fail "web-01 is not online"; }
[[ $(get "/api/servers/$DB" | jq -r .status) == online ]] || fail "db-01 is not online"
ok "web-01 and db-01 online"

# failing_check SERVER NAME: a port check of a closed port every 10 s, and
# a critical alert when it is down.
failing_check() {
    add_check "$1" "$2"
    alert_on "$1" "$2"
}
add_check() {
    send POST "/api/servers/$1/sensor_configs" "$(jq -n --arg n "$2" \
        '{kind: "port", name: $n, interval_ms: 10000, params: {host: "127.0.0.1", port: "9"}}')" | expect 201 >/dev/null
}
alert_on() {
    send POST "/api/servers/$1/thresholds" "$(jq -n --arg m "monitorflow_${2}_up" \
        '{metric: $m, comparison: "<", critical: 1, duration_s: 0}')" | expect 201 >/dev/null
}
# incident SERVER CHECK: its open incident, once it opened.
incident() {
    local found=""
    for _ in $(seq 45); do
        found=$(get "/api/incidents?state=open&limit=200" | jq -c --arg s "$1" --arg m "monitorflow_${2}_up" \
            '.incidents[] | select(.server_id == $s and .metric == $m)')
        [[ -n "$found" ]] && break
        sleep 2
    done
    [[ -n "$found" ]] || fail "no incident for $2"
    echo "$found"
}
hooked() { grep -c "monitorflow_${1}_up" "$WORK/hook/received.jsonl" || true; }
mailed() { curl -sf "$MAIL/api/v1/search?query=${1}_up" | jq '.messages_count'; }
# told CHECK [SECONDS]: a message about it reached both receivers.
told() {
    for _ in $(seq $(( ${2:-120} / 3 ))); do
        [[ $(hooked "$1") -gt 0 && $(mailed "$1") -gt 0 ]] && return 0
        sleep 3
    done
    fail "$1 was not told (webhook $(hooked "$1"), mail $(mailed "$1"))"
}
# quiet CHECK SECONDS: nothing about it reached either for that long.
quiet() {
    sleep "$2"
    [[ $(hooked "$1") == 0 && $(mailed "$1") == 0 ]] || fail "$1 was told during its silence"
}

step "outside any silence, a failing check is told"
failing_check "$WEB" port_a
incident "$WEB" port_a >/dev/null
told port_a
ok "port_a on web-01: by webhook and by mail"

step "a silence of one check holds that check only"
# A check the server runs, silenced before it fails.
add_check "$WEB" port_b
silence=$(send POST /api/silences "$(jq -n --arg s "$WEB" --arg e "$(date -u -d '+30 min' +%FT%TZ)" \
    '{scope: "server", target_id: $s, ends_at: $e, reason: "Rebuilding the port_b service", checks: [{name: "port_b"}]}')" \
    | expect 201)
alert_on "$WEB" port_b
failing_check "$WEB" port_c
b=$(incident "$WEB" port_b)
incident "$WEB" port_c >/dev/null
told port_c
quiet port_b 30
[[ $(jq -r .silenced_until <<<"$b") != null ]] || fail "port_b's incident does not say it is silenced"
[[ $(jq -r .in_maintenance <<<"$b") == false ]] || fail "port_b is not in a maintenance window"
state=$(get "/api/servers/$WEB" | jq -r .state)
[[ $state != silenced ]] || fail "web-01 shows as silenced for one check"
ok "port_b held, port_c told, web-01 is $state, not silenced"
send POST "/api/silences/$(jq -r .id <<<"$silence")/end" '' | expect 204 >/dev/null
told port_b
ok "port_b told once its silence ended"

step "a running maintenance window holds its server and says so"
start=$(TZ=$ZONE date -d '-10 min' +%H:%M)
day=$(TZ=$ZONE date -d '-10 min' +%F)
window=$(send POST /api/maintenance-windows "$(jq -n --arg site "$SITE" --arg s "$DB" --arg t "$start" --arg d "$day" --arg z "$ZONE" \
    '{site_id: $site, server_ids: [$s], repeat: "daily", start_time: $t, duration_minutes: 60, time_zone: $z,
      starts_on: $d, reason: "Nightly database maintenance"}')" | expect 201)
[[ $(jq -r .active_until <<<"$window") != null ]] || fail "the window does not run now: $window"
[[ $(jq '.next | length' <<<"$window") == 3 ]] || fail "the window lists $(jq '.next | length' <<<"$window") next windows"
db=$(get "/api/servers/$DB")
[[ $(jq -r .state <<<"$db") == silenced && $(jq -r .silence.window <<<"$db") == true ]] || fail "db-01 is not in maintenance: $(jq -c .silence <<<"$db")"
failing_check "$DB" port_d
d=$(incident "$DB" port_d)
quiet port_d 40
d=$(get "/api/incidents/$(jq -r .id <<<"$d")")
[[ $(jq -r .in_maintenance <<<"$d") == true ]] || fail "port_d's incident is not in maintenance"
ok "db-01 in maintenance until $(jq -r .silenced_until <<<"$d"), port_d held"
echo "$window" >"$WORK/window.json"

step "a window that does not run now holds nothing"
tomorrow=$(LC_ALL=C TZ=$ZONE date -d '+1 day' +%A | tr '[:upper:]' '[:lower:]')
add_check "$WEB" port_e
send POST /api/maintenance-windows "$(jq -n --arg site "$SITE" --arg s "$WEB" --arg w "$tomorrow" --arg z "$ZONE" \
    '{site_id: $site, server_ids: [$s], checks: [{name: "port_e"}], repeat: "weekly", weekdays: [$w],
      start_time: "03:00", duration_minutes: 120, time_zone: $z, reason: "Weekly patches"}')" | expect 201 >/dev/null
alert_on "$WEB" port_e
told port_e
ok "port_e told: its window is tomorrow"

if [[ ${MF_SILENCE_E2E_KEEP:-0} != 1 ]]; then
    step "removing the running window tells what it held"
    send DELETE "/api/maintenance-windows/$(jq -r .id "$WORK/window.json")" '' | expect 204 >/dev/null
    told port_d
    [[ $(get "/api/servers/$DB" | jq -r .state) != silenced ]] || fail "db-01 is still silenced"
    ok "port_d told once the window was removed"
fi

step "audit"
actions=$(psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT string_agg(DISTINCT action, ' ') FROM audit_log
    WHERE action LIKE 'silence.%' OR action LIKE 'maintenance_window.%'")
echo "audited: $actions"
for action in silence.create silence.end maintenance_window.create; do
    [[ " $actions " == *" $action "* ]] || fail "$action is not in the audit log"
done
ok "silences and windows are in the audit log"

echo
echo "All passed. Server: $API (admin / $PASSWORD), Mailpit: $MAIL, log in $WORK/server.log."
if [[ ${MF_SILENCE_E2E_STOP:-0} == 1 ]]; then
    kill "$(cat "$WORK/server.pid")"
    docker rm -f mf-silence-mail mf-silence-hook mf-silence-web-01 mf-silence-db-01 >/dev/null
    psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME" >/dev/null || true
else
    echo "Stop: kill \$(cat $WORK/server.pid); docker rm -f mf-silence-mail mf-silence-hook mf-silence-web-01 mf-silence-db-01"
fi
