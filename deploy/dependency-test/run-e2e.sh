#!/usr/bin/env bash
# End-to-end test of dependencies (ADR-0152), against real software in
# local Docker containers only.
#
# 1. Starts a private MonitorFlow server from this checkout (its own
#    database on the PostgreSQL of DATABASE_URL, the API on loopback, gRPC
#    and enrollment on the Docker bridge only), a mail catcher (Mailpit)
#    and a webhook receiver, both set up as notification channels.
# 2. Builds a small site: gw-host, a plain host with its own agent that
#    forwards a private Docker network to the central server; app-01 and
#    app-02, real agents on that network, which reach the central server
#    only through gw-host; nas, a device the central server checks only
#    through gw-host; and relay-1, a relay with branch-01 behind it.
#    app-01, app-02 and nas are put behind gw-host through the API.
# 3. Stops gw-host and checks, through the API and both receivers, that
#    one incident and one notification arrive, naming what is behind it,
#    and that everything behind it shows as unreachable.
# 4. Stops app-02 and the nas's service while gw-host is down, starts
#    gw-host again, and checks that app-01 opens nothing, that the nas
#    opens its own incident 2 minutes after gw-host is back, and app-02
#    its own 5 minutes after.
# 5. Stops relay-1 and checks that only its incident arrives, as before
#    dependencies: the relay is the automatic parent of branch-01.
#
# With MF_DEPENDENCY_E2E_BROWSER=1 it runs browser.mjs while gw-host is
# down, against `next dev` started on MF_E2E_UI_PORT (needs npm ci in
# web/ and playwright-core on NODE_PATH).
#
# Usage: run-e2e.sh
# Needs: jq, curl, docker, cargo, psql, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-dependency-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_dependency_e2e
API_PORT=${MF_E2E_API_PORT:-28144}
GRPC_PORT=28143
ENROLL_PORT=28145
MIRROR_PORT=28180
SMTP_PORT=28125
MAIL_API_PORT=28126
HOOK_PORT=28127
NAS_PORT=28181
UI_PORT=${MF_E2E_UI_PORT:-28190}
API=http://127.0.0.1:$API_PORT
MAIL=http://127.0.0.1:$MAIL_API_PORT
PASSWORD=e2e-dependency-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
LAN=mf-dep-lan
WAN=mf-dep-wan
# Fixed addresses: gw-host keeps them across a restart, as a real host does.
LAN_NET=10.231.81
WAN_NET=10.231.82
CONTAINERS="mf-dep-mail mf-dep-hook mf-dep-gw mf-dep-gw-agent mf-dep-nas mf-dep-app-01 mf-dep-app-02 mf-dep-relay mf-dep-branch-01"

fail() {
    echo "FAIL: $*" >&2
    if [[ -f "$WORK/server.log" ]]; then echo "--- server log"; tail -30 "$WORK/server.log" >&2; fi
    exit 1
}
ok() { echo "ok: $*"; }
step() { echo; echo "== $* ($(date +%T))"; }

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent -p monitorflow-relay)
# CARGO_TARGET_DIR may be absolute, or relative to the checkout.
TARGET=${CARGO_TARGET_DIR:-target}
[[ $TARGET == /* ]] || TARGET=$REPO/$TARGET
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent
RELAY_BIN=$TARGET/debug/monitorflow-relay

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "clean up an earlier run"
if [[ -f "$WORK/server.pid" ]] && kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null; then
    pid=$(cat "$WORK/server.pid"); kill "$pid"
    for _ in $(seq 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
fi
# shellcheck disable=SC2086
docker rm -f $CONTAINERS >/dev/null 2>&1 || true
docker network rm "$LAN" "$WAN" >/dev/null 2>&1 || true
rm -rf "$WORK"; mkdir -p "$WORK/server" "$WORK/hook"
: >"$WORK/hook/received.jsonl"
for port in "$API_PORT" "$MIRROR_PORT" "$MAIL_API_PORT" "$UI_PORT"; do
    ! ss -ltn "sport = :$port" | grep -q LISTEN || fail "port $port is taken; set MF_E2E_*_PORT"
done

step "the site: a private network behind gw-host, and the nas on it"
docker network create --internal --subnet "$LAN_NET.0/24" "$LAN" >/dev/null
docker network create --subnet "$WAN_NET.0/24" "$WAN" >/dev/null
ip_on() { docker inspect -f "{{(index .NetworkSettings.Networks \"$2\").IPAddress}}" "$1"; }
# The nas's web service, reachable only on the private network.
docker run -d --name mf-dep-nas --network "$LAN" --ip "$LAN_NET.10" --user "$(id -u):$(id -g)" python:3.12-slim \
    python3 -m http.server 8000 >/dev/null
NAS_IP=$(ip_on mf-dep-nas "$LAN")
# gw-host forwards the central server's gRPC and enrollment ports to the
# private network, and the nas's service to the central server.
docker run -d --name mf-dep-gw --hostname gw-host --network "$WAN" --ip "$WAN_NET.2" --user "$(id -u):$(id -g)" \
    -v "$HERE/forward.py:/forward.py:ro" python:3.12-slim \
    python3 /forward.py "$GRPC_PORT:$BRIDGE:$GRPC_PORT" "$ENROLL_PORT:$BRIDGE:$ENROLL_PORT" "$NAS_PORT:$NAS_IP:8000" >/dev/null
docker network connect --ip "$LAN_NET.2" "$LAN" mf-dep-gw
GW_LAN=$(ip_on mf-dep-gw "$LAN")
GW_BRIDGE=$(ip_on mf-dep-gw "$WAN")
[[ -n "$GW_LAN" && -n "$GW_BRIDGE" ]] || fail "gw-host has no address"
ok "gw-host at $GW_BRIDGE, and $GW_LAN on the private network; the nas at $NAS_IP behind it"

step "a private MonitorFlow, a mail catcher and a webhook receiver"
docker run -d --name mf-dep-mail -p "$BRIDGE:$SMTP_PORT:1025" -p "127.0.0.1:$MAIL_API_PORT:8025" \
    axllent/mailpit:v1.21 >/dev/null
docker run -d --name mf-dep-hook --user "$(id -u):$(id -g)" -p "$BRIDGE:$HOOK_PORT:8080" \
    -v "$HERE/hook.py:/hook.py:ro" -v "$WORK/hook:/out" python:3.12-slim python3 /hook.py /out/received.jsonl >/dev/null
ADMIN_URL=${DB_URL%/*}
psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" -c "CREATE DATABASE $DB_NAME" >/dev/null
cat >"$WORK/server.toml" <<TOML
database_url = "$ADMIN_URL/$DB_NAME"
api_cookie_secure = false
data_dir = "$WORK/server"
server_names = ["localhost", "127.0.0.1", "$BRIDGE", "$GW_LAN"]
api_listen_addr = "127.0.0.1:$API_PORT"
grpc_listen_addr = "$BRIDGE:$GRPC_PORT"
enroll_listen_addr = "$BRIDGE:$ENROLL_PORT"
mirror_listen_addr = "127.0.0.1:$MIRROR_PORT"

[tls]
ca_cert = "$WORK/server/tls/ca.pem"
cert = "$WORK/server/tls/cert.pem"
key = "$WORK/server/tls/key.pem"
TOML
echo "$PASSWORD" | NO_COLOR=1 "$SERVER_BIN" --config "$WORK/server.toml" user add --username admin --role admin >/dev/null
NO_COLOR=1 setsid "$SERVER_BIN" --config "$WORK/server.toml" >"$WORK/server.log" 2>&1 </dev/null &
echo $! >"$WORK/server.pid"
for _ in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
curl -sf "$API/api/health" >/dev/null || fail "the server did not start"
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
# The containers see the host's disks, whose real use is not this test's
# business: no disk alerts on this site.
send POST "/api/sites/$SITE/thresholds" \
    '{"metric": "monitorflow_disk_used_ratio", "comparison": ">", "warning": 0.9, "enabled": false}' | expect 201 >/dev/null
send POST /api/notifications/channels "$(jq -n --arg h "$BRIDGE" --argjson p "$SMTP_PORT" \
    '{kind: "email", settings: {host: $h, port: $p, username: "", from: "monitorflow@example.com", to: ["ops@example.com"]}}')" \
    | expect 201 >/dev/null
send POST /api/notifications/channels "$(jq -n --arg u "http://$BRIDGE:$HOOK_PORT/hook" '{kind: "webhook", settings: {url: $u}}')" \
    | expect 201 >/dev/null
ok "an email channel to Mailpit and a webhook channel"

step "agents: gw-host itself, app-01 and app-02 behind it"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
# agent NAME UPSTREAM DOCKER_ARGS...: the agent of this checkout, run by the
# host's own loader and libraries.
agent() {
    local name=$1 upstream=$2; shift 2
    mkdir -p "$WORK/$name"
    cat >"$WORK/$name.toml" <<TOML
upstream_url = "https://$upstream:$GRPC_PORT"
enroll_url = "https://$upstream:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
TOML
    docker run -d --name "mf-dep-$name" "$@" --user "$(id -u):$(id -g)" \
        -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/$name.toml:/agent/agent.toml:ro" \
        -v "$WORK/$name:/agent/data" -v /usr/lib:/hostlib:ro \
        debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
}
# gw-host's own agent shares its network and its name.
agent gw-agent "$BRIDGE" --network container:mf-dep-gw
agent app-01 "$GW_LAN" --hostname app-01 --network "$LAN"
agent app-02 "$GW_LAN" --hostname app-02 --network "$LAN"
server_id() { get "/api/servers?limit=500" | jq -r --arg h "$1" '.servers[] | select(.hostname == $h) | .id'; }
online() { [[ $(get "/api/servers/$1" | jq -r .status) == online ]]; }
for _ in $(seq 90); do
    GW=$(server_id gw-host); APP1=$(server_id app-01); APP2=$(server_id app-02)
    if [[ -n "$GW" && -n "$APP1" && -n "$APP2" ]] && online "$GW" && online "$APP1" && online "$APP2"; then break; fi
    sleep 2
done
for name in gw-host app-01 app-02; do
    id=$(server_id "$name"); [[ -n "$id" ]] && online "$id" || fail "$name is not online"
done
ok "gw-host, and app-01 and app-02 through it, online"

step "the nas, a device checked through gw-host"
NAS=$(send POST /api/devices "$(jq -n --arg site "$SITE" --arg a "$GW_BRIDGE" \
    '{site_id: $site, name: "nas", address: $a, probe: "central", ping: false}')" | expect 201 | jq -r .id)
# A website check: gw-host's forwarder accepts connections itself, so only
# an answer says the nas's service is there.
send POST "/api/servers/$NAS/sensor_configs" "$(jq -n --arg u "http://$GW_BRIDGE:$NAS_PORT/" \
    '{kind: "http", name: "http_web", interval_ms: 10000, timeout_ms: 4000, params: {url: $u}, probe: "central"}')" | expect 201 >/dev/null
send POST "/api/servers/$NAS/thresholds" '{"metric": "monitorflow_http_web_up", "comparison": "<", "critical": 1, "duration_s": 0}' \
    | expect 201 >/dev/null
ok "nas ($NAS) with its web page checked from the central server"

step "a relay, relay-1, and branch-01 behind it"
RKEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site, role: "relay"}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$RKEY" ]] || fail "no relay install key"
mkdir -p "$WORK/relay"
cat >"$WORK/relay.toml" <<TOML
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/relay/data"
enrollment_token = "$RKEY"
heartbeat_interval = 10
TOML
docker run -d --name mf-dep-relay --hostname relay-1 --network "$WAN" --ip "$WAN_NET.3" --user "$(id -u):$(id -g)" \
    -v "$RELAY_BIN:/relay/monitorflow-relay:ro" -v "$WORK/relay.toml:/relay/relay.toml:ro" \
    -v "$WORK/relay:/relay/data" -v /usr/lib:/hostlib:ro \
    debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /relay/monitorflow-relay --config /relay/relay.toml >/dev/null
RELAY_IP=$(ip_on mf-dep-relay "$WAN")
RELAY=""
for _ in $(seq 60); do
    RELAY=$(get /api/relays | jq -r '.relays[] | select(.peer_id == "relay-1") | .id' | head -1)
    [[ -n "$RELAY" ]] && break
    sleep 2
done
[[ -n "$RELAY" ]] || fail "relay-1 did not connect"
send POST "/api/relays/$RELAY/names" "$(jq -n --arg ip "$RELAY_IP" '{names: [$ip]}')" | expect 204 >/dev/null
cat >"$WORK/branch-01.toml" <<TOML
upstream_url = "https://$RELAY_IP:7443"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
TOML
mkdir -p "$WORK/branch-01"
docker run -d --name mf-dep-branch-01 --hostname branch-01 --network "$WAN" --user "$(id -u):$(id -g)" \
    -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/branch-01.toml:/agent/agent.toml:ro" \
    -v "$WORK/branch-01:/agent/data" -v /usr/lib:/hostlib:ro \
    debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
BRANCH=""
for _ in $(seq 90); do
    BRANCH=$(server_id branch-01)
    [[ -n "$BRANCH" ]] && online "$BRANCH" && [[ $(get "/api/servers/$BRANCH" | jq -r .via_relay) == relay-1 ]] && break
    sleep 2
done
[[ -n "$BRANCH" ]] && online "$BRANCH" || { docker logs --tail 20 mf-dep-branch-01; fail "branch-01 is not online through relay-1"; }
ok "relay-1 ($RELAY_IP), and branch-01 through it"

step "app-01, app-02 and the nas are behind gw-host"
send POST /api/dependencies "$(jq -n --arg p "$GW" --arg a "$APP1" --arg b "$APP2" --arg n "$NAS" \
    '{parent: {server_id: $p}, children: [{server_id: $a}, {server_id: $b}, {server_id: $n}]}')" | expect 201 >/dev/null
page=$(get "/api/servers/$GW/dependencies")
[[ $(jq '.children | length' <<<"$page") == 3 ]] || fail "gw-host lists $(jq -c '.children' <<<"$page")"
# A loop is refused in words.
refused=$(send POST /api/dependencies "$(jq -n --arg p "$APP1" --arg c "$GW" '{parent: {server_id: $p}, children: [{server_id: $c}]}')" | expect 409)
[[ $(jq -r .error <<<"$refused") == "gw-host cannot depend on app-01: app-01 already depends on gw-host (app-01, then gw-host)." ]] \
    || fail "the loop reads: $(jq -r .error <<<"$refused")"
# branch-01 is behind its relay without being set.
auto=$(get "/api/servers/$BRANCH/dependencies" | jq -c '.parents[] | select(.automatic)')
[[ $(jq -r .parent.name <<<"$auto") == relay-1 ]] || fail "branch-01's automatic parent: $auto"
ok "set through the API, a loop refused, branch-01 behind relay-1 automatically"

# Everything is well before the outage: nothing open, nothing sent.
for _ in $(seq 30); do
    [[ $(get "/api/servers/$NAS" | jq -r .state) == ok ]] && break
    sleep 2
done
[[ $(get "/api/incidents?state=open&limit=200" | jq '.incidents | length') == 0 ]] || fail "incidents open before the outage"
sleep 15
[[ $(wc -l <"$WORK/hook/received.jsonl") == 0 ]] || fail "something was sent before the outage"

opened() { jq -c 'select(.event == "incident.opened")' "$WORK/hook/received.jsonl"; }
mails() { curl -sf "$MAIL/api/v1/messages" | jq -r '.messages[].Subject'; }
open_incidents() { get "/api/incidents?state=open&limit=200" | jq -c '.incidents[]'; }
# wait_for WHAT SECONDS COMMAND...: until COMMAND succeeds.
wait_for() {
    local what=$1 seconds=$2; shift 2
    for _ in $(seq $(( seconds / 5 ))); do
        "$@" && return 0
        sleep 5
    done
    fail "$what, after $seconds s"
}
has_incident() { open_incidents | jq -e --arg s "$1" --arg m "$2" 'select((.server_id == $s or .relay_id == $s) and .metric == $m)' >/dev/null; }

step "gw-host goes down: one incident, one notification"
# A host shutting down stops its agent first, then its network.
docker stop -t 5 mf-dep-gw-agent >/dev/null
sleep 10
docker stop -t 2 mf-dep-gw >/dev/null
down_at=$(date +%s)
wait_for "gw-host's offline incident" 480 has_incident "$GW" offline
# The dispatcher waits 10 s for a quiet customer; give it a full window.
sleep 75
incidents=$(open_incidents)
[[ $(wc -l <<<"$incidents") == 1 ]] || fail "open incidents: $incidents"
GW_INCIDENT=$(jq -r .id <<<"$incidents")
[[ $(opened | wc -l) == 1 ]] || fail "webhook: $(opened)"
[[ $(opened | jq -r .server.name) == gw-host ]] || fail "webhook was about $(opened | jq -r .server.name)"
subjects=$(mails)
[[ $(wc -l <<<"$subjects") == 1 ]] || fail "mails: $subjects"
[[ $subjects == *"gw-host"*"is offline; 3 behind it are unreachable"* ]] || fail "the mail reads: $subjects"
ok "after $(( $(date +%s) - down_at )) s: one incident, one webhook request, one mail: $subjects"
# The nas's check does fail meanwhile: the engine holds it.
held=$(psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT count(*) FROM alert_states WHERE server_id = '$NAS' AND severity = 'critical'")
[[ $held == 1 ]] || fail "the nas's check is not failing ($held)"
for id in "$APP1" "$APP2" "$NAS"; do
    s=$(get "/api/servers/$id")
    [[ $(jq -r .state <<<"$s") == unreachable && $(jq -r .unreachable.because.name <<<"$s") == gw-host ]] \
        || fail "$(jq -r .hostname <<<"$s") is $(jq -r .state <<<"$s"): $(jq -c .unreachable <<<"$s")"
done
[[ $(get /api/servers/summary | jq .unreachable) == 3 ]] || fail "the summary counts $(get /api/servers/summary | jq -c .)"
hides=$(get "/api/incidents/$GW_INCIDENT" | jq -r '[.hides[].name] | sort | join(",")')
[[ $hides == "app-01,app-02,nas" ]] || fail "the incident hides $hides"
ok "app-01, app-02 and the nas are unreachable because gw-host is down, and its incident lists them"

if [[ ${MF_DEPENDENCY_E2E_BROWSER:-0} == 1 ]]; then
    step "in a real browser"
    (cd "$REPO/web" && MONITORFLOW_API_URL=$API setsid npx next dev -p "$UI_PORT" >"$WORK/next.log" 2>&1 </dev/null & echo $! >"$WORK/next.pid")
    for _ in $(seq 120); do curl -sf -o /dev/null "http://localhost:$UI_PORT/login/" && break; sleep 2; done
    node "$HERE/browser.mjs" "http://localhost:$UI_PORT" "$PASSWORD" "$WORK/screens" "$APP1" "$GW" "$GW_INCIDENT" \
        || fail "the browser check"
    kill -- "-$(cat "$WORK/next.pid")" 2>/dev/null || true
    ok "screenshots in $WORK/screens"
fi

step "while it is down, app-02 and the nas's service stop too; then gw-host is back"
docker stop -t 2 mf-dep-app-02 mf-dep-nas >/dev/null
docker start mf-dep-gw >/dev/null
docker start mf-dep-gw-agent >/dev/null
back_at=$(date +%s)
wait_for "gw-host back online" 120 online "$GW"
wait_for "app-01 back through gw-host" 180 online "$APP1"
# Two minutes to settle: nothing yet.
sleep $(( 100 - ($(date +%s) - back_at) ))
for id in "$APP1" "$APP2" "$NAS"; do
    [[ $(open_incidents | jq -c --arg s "$id" 'select(.server_id == $s)') == "" ]] || fail "an incident opened within 2 minutes of gw-host's return"
done
ok "nothing opened in the first 100 s after gw-host came back"
wait_for "the nas's own incident" 120 has_incident "$NAS" monitorflow_http_web_up
nas_after=$(( $(date +%s) - back_at ))
(( nas_after >= 120 )) || fail "the nas opened $nas_after s after gw-host was back"
ok "the nas, still failing, opened its own incident $nas_after s after gw-host was back"
wait_for "app-02's own offline incident" 420 has_incident "$APP2" offline
app2_after=$(( $(date +%s) - back_at ))
(( app2_after >= 290 )) || fail "app-02 opened $app2_after s after gw-host was back"
ok "app-02, still silent, opened its own incident $app2_after s after gw-host was back"
sleep 70
for name in nas app-02; do
    opened | jq -e --arg n "$name" 'select(.server.name == $n)' >/dev/null || fail "$name was not told by webhook"
done
mails | grep -q "nas" || fail "the nas was not mailed"
mails | grep -q "app-02" || fail "app-02 was not mailed"
has_incident "$APP1" offline && fail "app-01 has an incident"
ok "both were told by webhook and mail, app-01 never"

step "relay-1 goes down: its incident only, as before dependencies"
before=$(opened | wc -l)
docker stop -t 2 mf-dep-relay >/dev/null
wait_for "relay-1's offline incident" 480 has_incident "$RELAY" offline
sleep 75
new=$(opened | tail -n +$(( before + 1 )))
[[ $(wc -l <<<"$new") == 1 && $(jq -r .relay.name <<<"$new") == relay-1 ]] || fail "webhook after relay-1: $new"
has_incident "$BRANCH" offline && fail "branch-01 has its own incident"
[[ $(get "/api/servers/$BRANCH" | jq -r .state) == unreachable ]] || fail "branch-01 is $(get "/api/servers/$BRANCH" | jq -r .state)"
mails | grep -q "relay relay-1.*the server behind it cannot report" || fail "the relay's mail: $(mails | head -3)"
ok "one incident and one request for relay-1; branch-01 is unreachable, with no incident"
docker start mf-dep-relay >/dev/null
wait_for "branch-01 back through relay-1" 240 online "$BRANCH"
has_incident "$BRANCH" offline && fail "branch-01 opened an incident on its way back"
ok "branch-01 back, nothing opened for it"

step "audit"
actions=$(psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT count(*) FROM audit_log WHERE action = 'dependency.create'")
[[ $actions == 3 ]] || fail "$actions dependency.create entries"
ok "the three dependencies are in the audit log"

echo
echo "All passed. Server: $API (admin / $PASSWORD), Mailpit: $MAIL, log in $WORK/server.log."
if [[ ${MF_DEPENDENCY_E2E_KEEP:-0} != 1 ]]; then
    kill "$(cat "$WORK/server.pid")"
    # shellcheck disable=SC2086
    docker rm -f $CONTAINERS >/dev/null
    docker network rm "$LAN" "$WAN" >/dev/null || true
    psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" >/dev/null || true
else
    echo "Stop: kill \$(cat $WORK/server.pid); docker rm -f $CONTAINERS; docker network rm $LAN $WAN"
fi
