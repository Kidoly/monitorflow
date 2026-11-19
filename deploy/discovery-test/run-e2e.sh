#!/usr/bin/env bash
# End-to-end test of network discovery (ADR-0154), against real software in
# local Docker containers only. Nothing outside the test's own Docker
# network is ever scanned: its gateway (this host) is left out of the range.
#
# 1. A private MonitorFlow server from this checkout (its own database on
#    the PostgreSQL of DATABASE_URL, its own VictoriaMetrics, API on
#    loopback, gRPC and enrollment on the Docker bridge only), a relay of
#    this checkout in a container, and an agent of this checkout in another.
# 2. On a Docker network of their own: a host with SSH, one with RDP's
#    port, a web server, net-snmp's snmpd (v3 only), snmpsim playing a
#    switch, a UPS and a printer, a host that only answers ping, one that
#    drops everything, the agent's container, and empty addresses.
# 3. A scan from the central server: what each address proposes, the agent
#    recognised by its MAC address, SNMP through the scan's credentials and
#    through those of a device of the site. "Add all", then the devices'
#    templates and their first SNMP values.
# 4. The same scan from the relay, with names from Docker's DNS.
# 5. A second scan after one host goes and another comes: only those two
#    are reported, and a notification is queued.
# 6. The web interface in headless Chromium (browser.mjs).
#
# Usage: run-e2e.sh            (leaves everything running for a look)
#        run-e2e.sh --clean    (removes the containers and data after)
# Needs: jq, curl, psql, docker, cargo, node, /usr/bin/chromium, and
# DATABASE_URL pointing at a PostgreSQL the current user may create
# databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CLEAN=${1:-}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-discovery-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_discovery_e2e
API_PORT=${MF_E2E_API_PORT:-27844}
GRPC_PORT=${MF_E2E_GRPC_PORT:-27843}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-27845}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-27880}
VM_PORT=${MF_E2E_VM_PORT:-27828}
WEB_PORT=${MF_E2E_WEB_PORT:-27846}
API=http://127.0.0.1:$API_PORT
PASSWORD=e2e-discovery-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
NET=mf-disc-e2e
P=mf-disc-e2e

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

cleanup() {
    [[ -f "$WORK/server.pid" ]] && kill "$(cat "$WORK/server.pid")" 2>/dev/null || true
    [[ -f "$WORK/web.pid" ]] && kill "$(cat "$WORK/web.pid")" 2>/dev/null || true
    docker rm -f $(docker ps -aq --filter "name=^$P-") >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
}
if [[ $CLEAN == --clean ]]; then trap 'cleanup; rm -rf "$WORK"' EXIT; fi

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-relay -p monitorflow-agent)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
SERVER_BIN=$TARGET/debug/monitorflow-server
RELAY_BIN=$TARGET/debug/monitorflow-relay
AGENT_BIN=$TARGET/debug/monitorflow-agent

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "the test network"
cleanup
mkdir -p "$WORK"
docker network create "$NET" >/dev/null
GATEWAY=$(docker network inspect "$NET" -f '{{(index .IPAM.Config 0).Gateway}}')
BASE=${GATEWAY%.*}
at() { echo "$BASE.$1"; }
# What is scanned: from .2 to .40 of the test network, never its gateway.
RANGE="$(at 2)-40"
ok "network $NET, scanning $RANGE (gateway $GATEWAY left out)"

run_on() { # name, last octet, image and command...
    local name=$1 n=$2; shift 2
    docker run -d --name "$P-$name" --hostname "$name" --network "$NET" --ip "$(at "$n")" "$@" >/dev/null
}
# A Linux host with SSH.
run_on ssh 10 alpine:3.21 sh -c 'apk add -q --no-cache openssh >/dev/null && ssh-keygen -A >/dev/null && exec /usr/sbin/sshd -D -e'
# A web server.
run_on web 12 nginx:1.27-alpine
# net-snmp on a Linux host without SSH: v3 only, a user the site knows.
run_on snmpd 13 -v "$HERE/snmpd.conf:/conf/snmpd.conf:ro" debian:trixie-slim \
    sh -c 'apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends snmpd >/dev/null && exec snmpd -f -Lo -C -c /conf/snmpd.conf'
# snmpsim playing a switch, a UPS and a printer, each its own address,
# community "public".
for kind in switch ups printer; do
    mkdir -p "$WORK/snmpsim-$kind"
    cp "$REPO/deploy/probe-test/snmp/data/$kind.snmprec" "$WORK/snmpsim-$kind/public.snmprec"
done
n=14
for kind in switch ups printer; do
    run_on "$kind" "$n" -v "$WORK/snmpsim-$kind:/data:ro" python:3.12-slim \
        sh -c 'pip install -q --root-user-action=ignore snmpsim pysmi >/dev/null 2>&1 && exec snmpsim-command-responder --data-dir=/data --agent-udpv4-endpoint=0.0.0.0:161 --process-user=nobody --process-group=nogroup'
    n=$((n + 1))
done
# Answers ping and nothing else.
run_on quiet 17 alpine:3.21 sleep infinity
# Drops everything: silent, like an address with nothing on it.
run_on walled 18 --cap-add NET_ADMIN alpine:3.21 \
    sh -c 'apk add -q --no-cache iptables >/dev/null && iptables -P INPUT DROP && exec sleep infinity'
# A Windows-like host: something listens on RDP's port.
run_on rdp 19 alpine:3.21 sh -c 'exec nc -lk -p 3389 -e true'
for _ in $(seq 120); do docker logs "$P-snmpd" 2>&1 | grep -q "NET-SNMP version" && break; sleep 1; done
for kind in switch ups printer; do
    for _ in $(seq 180); do docker logs "$P-$kind" 2>&1 | grep -q "Listening at" && break; sleep 1; done
done
for _ in $(seq 60); do docker exec "$P-walled" iptables -S 2>/dev/null | grep -q "P INPUT DROP" && break; sleep 1; done
for _ in $(seq 60); do docker exec "$P-ssh" sh -c 'pgrep sshd' >/dev/null 2>&1 && break; sleep 1; done
ok "ssh, web, snmpd, switch, ups, printer, quiet, walled and rdp on $NET"

step "a private MonitorFlow in $WORK"
docker run -d --name "$P-vm" -p "127.0.0.1:$VM_PORT:8428" victoriametrics/victoria-metrics:latest >/dev/null
rm -rf "$WORK/server" "$WORK/relay" "$WORK/agent"; mkdir -p "$WORK/server" "$WORK/relay" "$WORK/agent"
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
victoriametrics_url = "http://127.0.0.1:$VM_PORT"

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
send() { printf '%s' "$3" | curl -s -b "$JAR" -X "$1" -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' --data-binary @- -w '\n%{http_code}' "$API$2"; }
expect() {
    local out status
    out=$(cat); status=${out##*$'\n'}
    [[ "$status" == "$1" ]] || { echo "${out%$'\n'*}" >&2; fail "expected HTTP $1, got $status"; }
    echo "${out%$'\n'*}"
}
until_ok() {
    local seconds=$1; shift
    for _ in $(seq "$seconds"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
    return 1
}

SITE=$(get /api/sites | jq -r '.sites[0].id // empty')
[[ -n "$SITE" ]] || SITE=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"office"}' | expect 201 | jq -r .id)
ok "site $SITE"

step "an agent of this checkout on the test network"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
cat >"$WORK/agent.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
# On the test network to be scanned, and on the bridge to reach the
# server; both before it starts, so its Hello reports both MAC addresses.
docker create --name "$P-agent" --hostname app-01 --network "$NET" --ip "$(at 11)" --user "$(id -u):$(id -g)" \
    -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/agent.toml:/agent/agent.toml:ro" \
    -v "$WORK/agent:/agent/data" -v /usr/lib:/hostlib:ro \
    debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
docker network connect bridge "$P-agent"
docker start "$P-agent" >/dev/null
agent_online() { get "/api/servers?limit=500" | jq -e '.servers[] | select(.hostname == "app-01" and .status == "online")'; }
until_ok 90 agent_online || { docker logs --tail 30 "$P-agent"; fail "the agent did not connect"; }
AGENT_ID=$(get "/api/servers?limit=500" | jq -r '.servers[] | select(.hostname == "app-01") | .id')
AGENT_MAC=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").MacAddress}}" "$P-agent")
psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT mac_addresses FROM servers WHERE id = '$AGENT_ID'" | grep -qi "$AGENT_MAC" \
    || fail "the agent does not report its MAC address $AGENT_MAC"
ok "agent app-01 online at $(at 11), reporting $AGENT_MAC"

step "a device of the site with SNMP v3 credentials"
# At an empty address: the scan must find snmpd with these credentials.
OLD=$(send POST /api/devices "$(jq -n --arg site "$SITE" --arg a "$(at 30)" '{site_id: $site, name: "Old NAS", address: $a,
    ping: false, probe: "central", template: "generic",
    snmp: {version: "v3", user: "e2e-scan", auth_protocol: "sha256", auth_password: "e2e-auth-password",
           priv_protocol: "aes128", priv_password: "e2e-priv-password"}}')" | expect 201 | jq -r .id)
ok "Old NAS at $(at 30), nothing there"

# The last finished run of scan $1, once no run waits or runs.
results() { get "/api/network-scans/$1/results"; }
settled() { results "$1" | jq -e '.current == null and .run != null'; }
wait_scan() { # scan, seconds
    until_ok "$2" settled "$1" || { results "$1" | jq -c '.current' >&2; tail -20 "$WORK/server.log" >&2; fail "the scan did not finish"; }
}
proposal() { # results, last octet: the action
    jq -r --arg a "$(at "$2")" '.hosts[] | select(.address == $a) | .proposal.action' <<<"$1"
}
check_action() { # results, octet, action, what
    local got; got=$(proposal "$1" "$2")
    [[ $got == "$3" ]] || { jq -c --arg a "$(at "$2")" '.hosts[] | select(.address == $a)' <<<"$1" >&2; fail "$4: got '${got:-nothing}'"; }
    ok "$4"
}

step "a scan from the central server"
SCAN=$(send POST /api/network-scans "$(jq -n --arg site "$SITE" --arg r "$RANGE" '{site_id: $site, name: "Office network",
    targets: [$r], ports: [22, 80, 443, 3389], probe: "central", notify: true,
    snmp: {version: "v2c", community: "public"}}')" | expect 201)
SCAN_ID=$(jq -r .id <<<"$SCAN")
[[ $(jq -r .addresses <<<"$SCAN") == 39 ]] || fail "the scan should try 39 addresses"
[[ $(jq -r .snmp.version <<<"$SCAN") == v2c && $(jq -r '.snmp | has("community")' <<<"$SCAN") == false ]] || fail "the community shows"
STARTED=$(date +%s)
wait_scan "$SCAN_ID" 240
R=$(results "$SCAN_ID")
ok "39 addresses scanned in $(( $(date +%s) - STARTED )) s, $(jq -r .run.answered <<<"$R") answered"
echo "$R" >"$WORK/central-1.json"
jq -e --arg b "$BASE." '[.hosts[].address | select(startswith($b) | not)] == []' <<<"$R" >/dev/null || fail "an address outside the range"
jq -e --arg g "$GATEWAY" '[.hosts[] | select(.address == $g)] == []' <<<"$R" >/dev/null || fail "the gateway was scanned"
check_action "$R" 10 agent "the SSH host: install the agent"
[[ $(jq -r --arg a "$(at 10)" '.hosts[] | select(.address == $a) | .proposal.os' <<<"$R") == linux ]] || fail "SSH is Linux"
check_action "$R" 19 agent "the RDP host: install the agent"
[[ $(jq -r --arg a "$(at 19)" '.hosts[] | select(.address == $a) | .proposal.os' <<<"$R") == windows ]] || fail "RDP is Windows"
check_action "$R" 11 monitored "the agent's container is already monitored"
[[ $(jq -r --arg a "$(at 11)" '.hosts[] | select(.address == $a) | .proposal.monitored_by.matched_by' <<<"$R") == mac ]] \
    || fail "the agent should be recognised by its MAC address"
ok "recognised by its MAC address"
check_action "$R" 12 device "the web server: a device"
jq -e --arg a "$(at 12)" '.hosts[] | select(.address == $a) | .ports == [80] and .ping' <<<"$R" >/dev/null || fail "the web server's port 80"
check_action "$R" 13 snmp_device "snmpd: an SNMP device, through the site's v3 credentials"
for kind in switch:14 ups:15 printer:16; do
    check_action "$R" "${kind#*:}" snmp_device "snmpsim's ${kind%:*}: an SNMP device"
    tpl=$(jq -r --arg a "$(at "${kind#*:}")" '.hosts[] | select(.address == $a) | .proposal.template' <<<"$R")
    [[ $tpl == "${kind%:*}" ]] || fail "the ${kind%:*} should get its template, got $tpl"
done
ok "the switch, the UPS and the printer get their templates"
check_action "$R" 17 device "the quiet host: a device with its ping"
jq -e --arg a "$(at 17)" '.hosts[] | select(.address == $a) | [.proposal.checks[].kind] == ["ping"]' <<<"$R" >/dev/null || fail "ping only"
for silent in 18 30 35; do
    [[ -z $(proposal "$R" "$silent") ]] || fail "$(at "$silent") is silent"
done
ok "the walled host and the empty addresses are not listed"

step "add all"
ADDED=$(send POST "/api/network-scans/$SCAN_ID/add" '{"all": true}' | expect 200)
echo "$ADDED" >"$WORK/added.json"
count=$(jq '.added | length' <<<"$ADDED")
(( count >= 6 )) || { echo "$ADDED"; fail "add all added $count devices"; }
ok "$count devices added"
device_of() { jq -r --arg a "$(at "$1")" '.added[] | select(.address == $a) | .server_id' <<<"$ADDED"; }
for item in switch:14:snmp_if ups:15:snmp_ups printer:16:snmp_printer snmpd:13:snmp_if web:12:port_80 quiet:17:ping; do
    IFS=: read -r name octet check <<<"$item"
    id=$(device_of "$octet")
    [[ -n $id ]] || fail "$name was not added"
    get "/api/servers/$id/sensor_configs" | jq -e --arg c "$check" '.entries[] | select(.name == $c)' >/dev/null \
        || fail "$name has no $check check"
done
[[ $(get "/api/devices/$(device_of 13)/snmp" | jq -r .version) == v3 ]] || fail "snmpd keeps the v3 credentials that answered"
[[ $(get "/api/devices/$(device_of 14)/snmp" | jq -r .version) == v2c ]] || fail "the switch keeps v2c"
ok "each device has its template's checks and the credentials that answered"
snmp_up() { # device: SNMP answers from its probe
    get "/api/servers/$1/sensor-values" | jq -e '.sensors[] | select(.metric == "monitorflow_snmp_up") | .series[] | select(.last_value == 1)'
}
for octet in 13 14 15 16; do
    until_ok 150 snmp_up "$(device_of "$octet")" || fail "$(at "$octet") does not answer its SNMP check"
done
ok "the four SNMP devices answer their checks"
R=$(results "$SCAN_ID")
for octet in 12 13 14 15 16 17; do check_action "$R" "$octet" monitored "$(at "$octet") is now monitored"; done

step "the same addresses from the relay"
RKEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site, role: "relay"}')" | expect 201 \
    | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
cat >"$WORK/relay.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/relay/data"
enrollment_token = "$RKEY"
heartbeat_interval = 10
EOF
# On the test network first, so its resolver is Docker's, which names the
# test's containers; then on the bridge to reach the server.
docker create --name "$P-relay" --hostname relay-office --network "$NET" --ip "$(at 5)" --user "$(id -u):$(id -g)" \
    -v "$RELAY_BIN:/relay/monitorflow-relay:ro" -v "$WORK/relay.toml:/relay/relay.toml:ro" \
    -v "$WORK/relay:/relay/data" -v /usr/lib:/hostlib:ro \
    debian:trixie-slim /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /relay/monitorflow-relay --config /relay/relay.toml >/dev/null
docker network connect bridge "$P-relay"
docker start "$P-relay" >/dev/null
relay_online() { get /api/relays | jq -e '.relays[] | select(.peer_id == "relay-office" and .status == "online")'; }
until_ok 90 relay_online || { docker logs --tail 30 "$P-relay"; fail "the relay did not connect"; }
RELAY=$(get /api/relays | jq -r '.relays[] | select(.peer_id == "relay-office") | .id')
ok "relay-office online at $(at 5)"
RSCAN=$(send POST /api/network-scans "$(jq -n --arg site "$SITE" --arg r "$RANGE" --arg relay "$RELAY" '{site_id: $site,
    name: "Office from the relay", targets: [$r], ports: [22, 80, 3389], probe: $relay}')" | expect 201 | jq -r .id)
wait_scan "$RSCAN" 240
RR=$(results "$RSCAN")
echo "$RR" >"$WORK/relay-1.json"
[[ $(jq -r .run.error <<<"$RR") == null ]] || fail "the relay's scan failed: $(jq -r .run.error <<<"$RR")"
check_action "$RR" 10 agent "from the relay: the SSH host"
check_action "$RR" 11 monitored "from the relay: the agent, recognised"
check_action "$RR" 14 monitored "from the relay: the switch added before"
name=$(jq -r --arg a "$(at 12)" '.hosts[] | select(.address == $a) | .name // empty' <<<"$RR")
[[ $name == *web* ]] || fail "the relay should name the web server from Docker's DNS, got '${name}'"
ok "the relay names $(at 12) $name"
[[ -z $(proposal "$RR" 18) ]] || fail "the walled host is silent from the relay too"

step "the public API with a key"
APIKEY=$(send POST /api/api-keys '{"name": "e2e", "role": "operator"}' | expect 201 | jq -r '.. | strings | select(startswith("mfk"))' | head -1)
[[ -n "$APIKEY" ]] || fail "no API key"
curl -sf -H "Authorization: Bearer $APIKEY" "$API/api/v1/network-scans/$SCAN_ID/results" | jq -e '.hosts | length > 0' >/dev/null \
    || fail "the results through /api/v1"
curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $APIKEY" "$API/api/v1/network-scans/$SCAN_ID/run" \
    | grep -q 202 || fail "a scan run through /api/v1"
wait_scan "$SCAN_ID" 240
ok "/api/v1 reads the results and starts a scan with an API key"

step "a second scan reports only what changed"
docker rm -f "$P-quiet" >/dev/null
run_on newcomer 21 nginx:1.27-alpine
for _ in $(seq 30); do curl -s -m 1 "http://$(at 21)/" >/dev/null 2>&1 && break; docker exec "$P-newcomer" true; sleep 1; done
send POST "/api/network-scans/$SCAN_ID/run" '' | expect 202 >/dev/null
wait_scan "$SCAN_ID" 240
R2=$(results "$SCAN_ID")
echo "$R2" >"$WORK/central-2.json"
[[ $(jq -c .run.gone <<<"$R2") == "[\"$(at 17)\"]" ]] || fail "gone should be $(at 17) only: $(jq -c .run.gone <<<"$R2")"
[[ $(jq -r '[.hosts[] | select(.new) | .address] | join(",")' <<<"$R2") == "$(at 21)" ]] || fail "new should be $(at 21) only"
[[ $(jq -r .run.new <<<"$R2") == 1 ]] || fail "one new address"
check_action "$R2" 21 device "the newcomer is proposed"
ok "new: $(at 21); gone: $(at 17)"
NOTE=$(psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT details FROM notification_outbox WHERE event = 'network_scan.changed'
    ORDER BY id DESC LIMIT 1")
[[ $(jq -r '.network_scan.new[0]' <<<"$NOTE") == "$(at 21)" ]] || fail "no notification of the change"
ok "a notification of the change is queued"
AUDIT=$(psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT string_agg(action, ',' ORDER BY id) FROM audit_log WHERE action LIKE 'network_scan.%'")
[[ $AUDIT == network_scan.create,network_scan.run,network_scan.add,network_scan.create,network_scan.run,network_scan.run,network_scan.run ]] \
    || fail "audit: $AUDIT"
psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT details::text FROM audit_log" | grep -q '"public"' && fail "a community in the audit log"
ok "audited, without secrets"

step "the web interface"
if [[ -x /usr/bin/chromium ]] && command -v node >/dev/null; then
    (cd "$REPO/web" && [[ -d node_modules ]] || npm ci --no-audit --no-fund >/dev/null)
    (cd "$REPO/web" && MONITORFLOW_API_URL=$API setsid npx next dev -p "$WEB_PORT" >"$WORK/web.log" 2>&1 </dev/null & echo $! >"$WORK/web.pid")
    until_ok 120 curl -sf "http://localhost:$WEB_PORT/login/" || fail "the web interface did not start"
    mkdir -p "$WORK/browser" "$WORK/shots"
    [[ -d "$WORK/browser/node_modules/playwright-core" ]] || (cd "$WORK/browser" && npm init -y >/dev/null && npm i -q playwright-core@1 >/dev/null 2>&1)
    cp "$HERE/browser.mjs" "$WORK/browser/"
    SHOTS="$WORK/shots" node "$WORK/browser/browser.mjs" "http://localhost:$WEB_PORT" "$PASSWORD" "$(at 21)" "$(at 10)" \
        || fail "the browser test"
    ok "screenshots in $WORK/shots"
else
    echo "note: no Chromium or node here: the browser half was not run"
fi

echo
echo "all discovery end-to-end checks passed"
