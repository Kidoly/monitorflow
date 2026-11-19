#!/usr/bin/env bash
# End-to-end test of custom SNMP checks (ADR-0157), against real software in
# local Docker containers only, on a network of their own: nothing outside
# it is ever polled.
#
# 1. Targets: snmpsim playing snmp/data (a firewall and a NAS: a moving
#    gauge, a counter that wraps, a firmware text and an HA state one can
#    SET, a temperature in tenths, a RAID table), and net-snmp's snmpd with
#    v1, v2c and v3 and a table whose rows come from a file the test
#    rewrites. The crate's tests run against both.
# 2. A private MonitorFlow server from this checkout (its own database on
#    the PostgreSQL of DATABASE_URL, its own VictoriaMetrics container, API
#    on loopback, gRPC and enrollment on the Docker bridge only), and a
#    relay of this checkout in a container, enrolled with an install key.
# 3. Through the API: a MIB upload, lookups, devices from the central
#    server and from the relay, custom checks with limits, Test and
#    Browse, values, rates, texts and states arriving, alerts opening when
#    the test SETs a failed state, a text and a RAID row, and resolving,
#    table rows that appear, and an import of a PRTG configuration holding
#    each custom SNMP sensor kind with its lookup files.
# 4. In a real browser (headless Chromium): the device page, a check added
#    with the library and Browse, tested and saved, and the lookups and MIBs
#    page.
#
# Usage: run-e2e.sh            (leaves everything running for a look)
#        run-e2e.sh --clean    (removes the containers and data after)
# Needs: jq, curl, psql, docker, cargo, node, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CLEAN=${1:-}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-snmpx-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_snmpx_e2e
API_PORT=${MF_E2E_API_PORT:-29644}
GRPC_PORT=${MF_E2E_GRPC_PORT:-29643}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-29645}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-29680}
VM_PORT=${MF_E2E_VM_PORT:-29628}
WEB_PORT=${MF_E2E_WEB_PORT:-29646}
API=http://127.0.0.1:$API_PORT
PASSWORD=e2e-snmpx-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
NET=mf-snmpx-e2e
P=mf-snmpx-e2e

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

cleanup() {
    [[ -f "$WORK/web.pid" ]] && kill -- -"$(cat "$WORK/web.pid")" 2>/dev/null || true
    [[ -f "$WORK/server.pid" ]] && kill "$(cat "$WORK/server.pid")" 2>/dev/null || true
    docker rm -f $(docker ps -aq --filter "name=^$P-") >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
}
if [[ $CLEAN == --clean ]]; then trap 'cleanup; rm -rf "$WORK"' EXIT; fi

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-relay)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
SERVER_BIN=$TARGET/debug/monitorflow-server
RELAY_BIN=$TARGET/debug/monitorflow-relay

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "targets"
cleanup
docker network create "$NET" >/dev/null
ip_of() { docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "$P-$1"; }
mkdir -p "$WORK/rows"
# The jobs table of snmpd: index|name|state|count, rewritten by the test.
printf '1|backup-nightly|1|100\n2|backup-weekly|1|200\n' >"$WORK/rows/rows"
chmod 777 "$WORK/rows"; chmod 666 "$WORK/rows/rows"
docker run -d --name "$P-snmpd" --network "$NET" -v "$HERE/snmp/snmpd.conf:/conf/snmpd.conf:ro" \
    -v "$HERE/snmp/table.sh:/conf/table.sh:ro" -v "$WORK/rows:/work" debian:trixie-slim \
    sh -c 'apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends snmpd snmp >/dev/null && exec snmpd -f -Lo -C -c /conf/snmpd.conf' >/dev/null
docker run -d --name "$P-snmpsim" --network "$NET" -v "$HERE/snmp/data:/data:ro" python:3.12-slim \
    sh -c 'pip install -q --root-user-action=ignore snmpsim pysmi >/dev/null 2>&1 && exec snmpsim-command-responder --data-dir=/data --agent-udpv4-endpoint=0.0.0.0:161 --process-user=nobody --process-group=nogroup' >/dev/null
for _ in $(seq 180); do docker logs "$P-snmpd" 2>&1 | grep -q "NET-SNMP version" && break; sleep 1; done
for _ in $(seq 180); do docker logs "$P-snmpsim" 2>&1 | grep -q "Listening at" && break; sleep 1; done
SNMPD=$(ip_of snmpd); SIM=$(ip_of snmpsim)
[[ -n $SNMPD && -n $SIM ]] || fail "no address for the agents"
ok "snmpd $SNMPD, snmpsim $SIM"
# SETs go through snmpsim's writable values, from snmpd's container.
set_sim() { docker exec "$P-snmpd" snmpset -v2c -c "$1" "$SIM" "$2" "$3" "$4" >/dev/null || fail "the SET of $2 was refused"; }

step "the custom SNMP checks of the crate against snmpd and snmpsim"
(cd "$REPO" && MF_SNMPX_SIM="$SIM:161" MF_SNMPX_SNMPD="$SNMPD:161" MF_SNMPX_ROWS="$WORK/rows/rows" \
    cargo test -q -p monitorflow-snmp --test custom 2>&1 | tail -3)
(cd "$REPO" && MF_SNMPX_SIM="$SIM:161" MF_SNMPX_SNMPD="$SNMPD:161" MF_SNMPX_ROWS="$WORK/rows/rows" \
    cargo test -q -p monitorflow-snmp --test custom >/dev/null 2>&1) || fail "the crate's tests failed against the real agents"
ok "gauges, rates, texts, lookups, tables, reads and walks; v1, v2c and v3; rows that change"

step "a private MonitorFlow in $WORK"
docker run -d --name "$P-vm" -p "127.0.0.1:$VM_PORT:8428" victoriametrics/victoria-metrics:latest >/dev/null
rm -rf "$WORK/server" "$WORK/relay"; mkdir -p "$WORK/server" "$WORK/relay"
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
# Waits up to $1 seconds for the command $2... to succeed.
until_ok() {
    local seconds=$1; shift
    for _ in $(seq "$seconds"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done
    return 1
}

SITE=$(get /api/sites | jq -r '.sites[0].id // empty')
[[ -n "$SITE" ]] || SITE=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"branch"}' | expect 201 | jq -r .id)
ok "site $SITE"

step "a relay of the site"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site, role: "relay"}')" | expect 201 \
    | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no relay install key"
cat >"$WORK/relay.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/relay/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
# The relay of this checkout, run by the host's own loader and libraries,
# on the bridge to reach the server and on the targets' network.
docker run -d --name "$P-relay" --hostname relay-branch --user "$(id -u):$(id -g)" \
    -v "$RELAY_BIN:/relay/monitorflow-relay:ro" -v "$WORK/relay.toml:/relay/relay.toml:ro" \
    -v "$WORK/relay:/relay/data" -v /usr/lib:/hostlib:ro \
    debian:trixie-slim /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /relay/monitorflow-relay --config /relay/relay.toml >/dev/null
docker network connect "$NET" "$P-relay"
relay_online() { get /api/relays | jq -e '.relays[] | select(.peer_id == "relay-branch" and .status == "online")'; }
until_ok 90 relay_online || { docker logs --tail 30 "$P-relay"; fail "the relay did not connect"; }
RELAY=$(get /api/relays | jq -r '.relays[] | select(.peer_id == "relay-branch") | .id')
NAMES=$(get "/api/relays/$RELAY" | jq -c '.pending_names // []')
if [[ $NAMES != "[]" ]]; then send POST "/api/relays/$RELAY/names" "$(jq -n --argjson n "$NAMES" '{names: $n}')" | expect 204 >/dev/null; fi
ok "relay-branch online"

step "a MIB file and lookups"
UPLOAD=$(jq -n --rawfile t "$HERE/mibs/TEST-FW-MIB.txt" '{files: [{name: "TEST-FW-MIB.txt", text: $t}]}')
MIB=$(send POST /api/snmp-mibs "$UPLOAD" | expect 201)
[[ $(jq -r '.mibs[0].module, .mibs[0].objects' <<<"$MIB" | tr '\n' ' ') == "TEST-FW-MIB 6 " ]] || { echo "$MIB"; fail "the MIB was not read"; }
[[ $(jq -r '.offered_lookups[0].name' <<<"$MIB") == testFwHaRole ]] || fail "the HA role was not offered as a lookup"
ok "TEST-FW-MIB read: 6 objects, the HA role offered as a lookup"
HA_LOOKUP=$(send POST /api/snmp-lookups '{"name": "HA role", "entries": [
    {"value": 1, "name": "Master", "state": "ok"}, {"value": 2, "name": "Backup", "state": "ok"},
    {"value": 3, "name": "Failed", "state": "critical"}], "otherwise": "critical"}' | expect 201 | jq -r .id)
RAID_LOOKUP=$(send POST /api/snmp-lookups '{"name": "RAID state", "entries": [
    {"value": 1, "name": "Normal", "state": "ok"}, {"value": 2, "name": "Repairing", "state": "warning"},
    {"value": 11, "max": 12, "name": "Degraded or crashed", "state": "critical"}]}' | expect 201 | jq -r .id)
SYS_LOOKUP=$(send POST /api/snmp-lookups '{"name": "NAS status", "entries": [
    {"value": 1, "name": "Normal", "state": "ok"}, {"value": 2, "name": "Failed", "state": "critical"}]}' | expect 201 | jq -r .id)
send POST /api/snmp-lookups '{"name": "HA role", "entries": [{"value": 1, "name": "x", "state": "ok"}]}' | expect 409 >/dev/null
ok "three lookups of our own, names unique"

step "devices from the central server and from the relay"
add_device() { # name, address, credentials JSON, probe: prints the device ID
    send POST /api/devices "$(jq -n --arg site "$SITE" --arg n "$1" --arg a "$2" --argjson c "$3" --arg p "$4" \
        '{site_id: $site, name: $n, address: $a, snmp: $c, probe: $p, ping: false}')" | expect 201 | jq -r .id
}
FW=$(add_device "Edge firewall" "$SIM" '{"version": "v2c", "community": "firewall"}' central)
NAS=$(add_device "Files NAS" "$SIM" '{"version": "v2c", "community": "nas"}' "$RELAY")
JOBS=$(add_device "Jobs server" "$SNMPD" '{"version": "v1", "community": "e2e-community"}' "$RELAY")
ok "a firewall checked from the central server, a NAS and an SNMPv1 server from the relay"

step "Test and Browse from each probe"
READ=$(send POST "/api/devices/$FW/snmp/read" '{"probe": "central", "oids": ["1.3.6.1.4.1.12356.101.4.1.8.0", "1.3.6.1.4.1.12356.101.13.2.1.1.15.1", "1.3.6.1.4.1.12356.9.9.0"]}' | expect 200)
[[ $(jq -r '.probe, .objects[0].name, .objects[0].type, .objects[1].name, .objects[2].type' <<<"$READ" | tr '\n' ' ') == \
    "central testFwSessions.0 Gauge32 testFwHaRole.1 noSuchInstance " ]] || { echo "$READ"; fail "Test from the central server"; }
ok "Test from the central server: types, values and the MIB's names"
WALK=$(send POST "/api/devices/$NAS/snmp/read" '{"walk": "1.3.6.1.4.1.6574.3", "max": 4}' | expect 200)
[[ $(jq -r '.probe, (.objects | length), .cut' <<<"$WALK" | tr '\n' ' ') == "relay-branch 4 true " ]] || { echo "$WALK"; fail "Browse through the relay"; }
ok "Browse through the relay: $(jq -r '[.objects[] | .value] | join(", ")' <<<"$WALK"), cut at 4"
V1=$(send POST "/api/devices/$JOBS/snmp/read" '{"walk": "1.3.6.1.4.1.8072.9999.9999.1.1.2"}' | expect 200)
[[ $(jq -r '[.objects[].value] | join(",")' <<<"$V1") == "backup-nightly,backup-weekly" ]] || { echo "$V1"; fail "SNMPv1 through the relay"; }
ok "SNMPv1 through the relay: the jobs table's names"

step "custom checks"
FW_CHECK=$(jq -n --arg ha "$HA_LOOKUP" '{name: "snmp_fw", probe: "central", interval_ms: 10000, channels: [
    {oid: "1.3.6.1.4.1.12356.101.4.1.8.0", name: "Sessions", unit: "sessions",
     limits: {comparison: ">", warning: 5000, critical: 8000, duration_s: 0}},
    {oid: "1.3.6.1.2.1.2.2.1.10.1", name: "Inbound", type: "counter", multiply: 8, unit: "bit/s"},
    {oid: "1.3.6.1.4.1.12356.101.4.1.1.0", name: "Firmware", type: "text", match: "matches", expect: "^v7\\.", severity: "warning"},
    {oid: "1.3.6.1.4.1.12356.101.13.2.1.1.15.1", name: "HA role", lookup: $ha}]}')
TESTED=$(send POST "/api/devices/$FW/snmp/test" "$FW_CHECK" | expect 200)
[[ $(jq -r '.rows[0].channels | map(.state // .value // "-" | tostring) | join(",")' <<<"$TESTED") =~ ^[0-9.]+,-,0,0$ ]] \
    || { echo "$TESTED"; fail "the test of the firewall's check"; }
ok "the firewall's check tested: $(jq -c '[.rows[0].channels[] | [.name, .type, .raw]]' <<<"$TESTED")"
send POST "/api/devices/$FW/snmp-checks" "$FW_CHECK" | expect 201 >/dev/null
send POST "/api/devices/$NAS/snmp-checks" "$(jq -n --arg s "$SYS_LOOKUP" '{name: "snmp_nas", interval_ms: 10000, channels: [
    {oid: "1.3.6.1.4.1.6574.1.2.0", name: "Temperature", divide: 10, unit: "°C", limits: {comparison: ">", warning: 50, critical: 60, duration_s: 0}},
    {oid: "1.3.6.1.4.1.6574.1.1.0", name: "System status", lookup: $s}]}')" | expect 201 >/dev/null
send POST "/api/devices/$NAS/snmp-checks" "$(jq -n --arg r "$RAID_LOOKUP" '{name: "snmp_raid", interval_ms: 10000,
    table: "1.3.6.1.4.1.6574.3.1.1", name_column: "1.3.6.1.4.1.6574.3.1.1.2",
    channels: [{oid: "1.3.6.1.4.1.6574.3.1.1.3", name: "RAID state", lookup: $r}]}')" | expect 201 >/dev/null
send POST "/api/devices/$JOBS/snmp-checks" '{"name": "snmp_jobs", "interval_ms": 10000,
    "table": "1.3.6.1.4.1.8072.9999.9999.1.1", "name_column": "1.3.6.1.4.1.8072.9999.9999.1.1.2",
    "channels": [{"oid": "1.3.6.1.4.1.8072.9999.9999.1.1.3", "name": "Exit"}, {"oid": "1.3.6.1.4.1.8072.9999.9999.1.1.4", "name": "Runs", "type": "counter"}]}' | expect 201 >/dev/null
ok "4 custom checks: the firewall's, the NAS's and its RAID table, and the jobs table over SNMPv1"
send DELETE "/api/snmp-lookups/$HA_LOOKUP" '' | expect 409 >/dev/null
ok "a lookup a check uses cannot be removed"

step "values, rates, texts and states"
# The last value of a channel: device, check, key, instance, field.
channel() {
    get "/api/devices/$1/snmp-checks" | jq -er --arg c "$2" --arg k "$3" --arg i "$4" --arg f "$5" \
        '.checks[] | select(.name == $c) | .channels[] | select(.key == $k) | .values[] | select(.instance == $i) | .[$f] // empty'
}
between() { # device, check, key, instance, field, low, high
    local v; v=$(channel "$1" "$2" "$3" "$4" "$5") || return 1
    awk -v v="$v" -v lo="$6" -v hi="$7" 'BEGIN { exit !(v >= lo && v <= hi) }'
}
is_text() { [[ $(channel "$1" "$2" "$3" "$4" text) == "$5" ]]; }
wait_between() { # ..., what
    until_ok 120 between "$1" "$2" "$3" "$4" "$5" "$6" "$7" || { get "/api/devices/$1/snmp-checks" | jq -c . >&2; fail "$8"; }
    ok "$8: $(channel "$1" "$2" "$3" "$4" "$5")"
}
wait_text() { # device, check, key, instance, text, what
    until_ok 120 is_text "$1" "$2" "$3" "$4" "$5" || { get "/api/devices/$1/snmp-checks" | jq -c . >&2; fail "$6"; }
    ok "$6"
}
wait_between "$FW" snmp_fw sessions "" value 1000 2000 "the firewall's sessions, a gauge"
wait_between "$FW" snmp_fw inbound "" value 1.0e8 2.5e8 "its inbound traffic, a Counter32 as bits a second"
wait_text "$FW" snmp_fw firmware "" "v7.2.5" "its firmware, a text matching ^v7\\."
wait_text "$FW" snmp_fw ha_role "" "Master" "its HA role named by our lookup"
wait_between "$NAS" snmp_nas temperature "" value 41.5 41.5 "the NAS's temperature in tenths, divided by 10, through the relay"
wait_text "$NAS" snmp_nas system_status "" "Normal" "its system status"
wait_text "$NAS" snmp_raid raid "Volume 1" "Normal" "its RAID table, a row per volume named by its name column"
wait_text "$NAS" snmp_raid raid "Volume 2" "Normal" "and the second volume"
wait_between "$JOBS" snmp_jobs exit "backup-nightly" value 1 1 "the jobs table over SNMPv1 through the relay"
wait_between "$JOBS" snmp_jobs runs "backup-weekly" value 0 0 "and a counter rate of 0 while it stands still"

step "alerts open and resolve"
open_incident() { # device, metric, instance
    get "/api/incidents?state=open&server_id=$1&limit=50" | jq -e --arg m "$2" --arg i "$3" \
        '.incidents[] | select(.metric == $m and .instance == $i)'
}
wait_open() { # device, metric, instance, what
    until_ok 120 open_incident "$1" "$2" "$3" || { get "/api/incidents?state=open&server_id=$1" | jq -c '.incidents[] | {metric, instance}' >&2; fail "$4"; }
    ok "$4: $(open_incident "$1" "$2" "$3" | jq -r '.severity + ", " + (.reason // "")')"
}
wait_resolved() {
    gone() { ! open_incident "$1" "$2" "$3"; }
    until_ok 120 gone "$1" "$2" "$3" || fail "$4"
    ok "$4"
}
open_incident "$FW" monitorflow_snmp_fw_ha_role_state central >/dev/null && fail "an HA incident before the test"
set_sim firewall 1.3.6.1.4.1.12356.101.13.2.1.1.15.1 i 3
wait_open "$FW" monitorflow_snmp_fw_ha_role_state central "the HA role failed: a critical incident"
set_sim firewall 1.3.6.1.4.1.12356.101.13.2.1.1.15.1 i 1
wait_resolved "$FW" monitorflow_snmp_fw_ha_role_state central "it resolved once the unit was master again"
set_sim firewall 1.3.6.1.4.1.12356.101.4.1.1.0 s "v8.0.1"
wait_open "$FW" monitorflow_snmp_fw_firmware_state central "an unexpected firmware: a warning"
[[ $(open_incident "$FW" monitorflow_snmp_fw_firmware_state central | jq -r .severity) == warning ]] || fail "the firmware's incident is not a warning"
set_sim firewall 1.3.6.1.4.1.12356.101.4.1.1.0 s "v7.2.5"
wait_resolved "$FW" monitorflow_snmp_fw_firmware_state central "it resolved with the expected firmware"
set_sim nas 1.3.6.1.4.1.6574.3.1.1.3.0 i 12
wait_open "$NAS" monitorflow_snmp_raid_raid_state "Volume 1" "a crashed volume: an incident on its row"
open_incident "$NAS" monitorflow_snmp_raid_raid_state "Volume 2" >/dev/null && fail "the healthy volume has an incident"
set_sim nas 1.3.6.1.4.1.6574.3.1.1.3.0 i 1
wait_resolved "$NAS" monitorflow_snmp_raid_raid_state "Volume 1" "it resolved once the volume was normal"

step "table rows appear and go"
printf '2|backup-weekly|1|200\n3|archive|2|7\n' >"$WORK/rows/rows"
wait_between "$JOBS" snmp_jobs exit "archive" value 2 2 "a new row appears by itself"
stale() { # the nightly row stopped reporting: its time no longer moves
    local a b; a=$(channel "$JOBS" snmp_jobs exit backup-nightly at); sleep 25; b=$(channel "$JOBS" snmp_jobs exit backup-nightly at)
    [[ $a == "$b" ]]
}
until_ok 3 stale || fail "the row that went away still reports"
ok "the row that went away stopped reporting"

step "the counter's wraps give no wrong rate"
RATES=$(curl -sf -b "$JAR" -G "$API/api/metrics/query_range" --data-urlencode "server_id=$FW" \
    --data-urlencode metric=monitorflow_snmp_fw_inbound --data-urlencode "start=$(date -u -d '-15 min' +%Y-%m-%dT%H:%M:%SZ)" \
    | jq '[.series[].points[][1] | select(. != null)]')
[[ $(jq 'length' <<<"$RATES") -gt 3 ]] || fail "too few rates: $RATES"
jq -e 'all(.[]; . >= 1.0e8 and . <= 2.5e8)' <<<"$RATES" >/dev/null || fail "a rate out of bounds: $RATES"
ok "$(jq length <<<"$RATES") rates, all between 100 and 250 Mbit/s, across the counter's wraps"

step "a PRTG configuration with each custom SNMP sensor kind"
sed -e "s/192\.0\.2\.40/$SIM/; s/192\.0\.2\.41/$SIM/" "$REPO/crates/import/tests/fixtures/prtg/configuration-snmp.dat" >"$WORK/prtg.dat"
jq -n --arg site "$SITE" --rawfile dat "$WORK/prtg.dat" \
    --rawfile raid "$REPO/crates/import/tests/fixtures/prtg/raid.ovl" --rawfile ha "$REPO/crates/import/tests/fixtures/prtg/ha.ovl" \
    '{source: "prtg", site_id: $site, files: [{name: "PRTG Configuration.dat", content: $dat},
      {name: "raid.ovl", content: $raid}, {name: "ha.ovl", content: $ha}]}' >"$WORK/import.json"
IMPORT=$(send POST /api/imports "$(cat "$WORK/import.json")" | expect 201)
CUSTOM=$(jq '[.items[] | select(.create.kind == "snmp_custom")] | length' <<<"$IMPORT")
[[ $CUSTOM == 6 ]] || { jq -c '[.items[] | {kind, list, note: .note.code, create: .create.kind}]' <<<"$IMPORT"; fail "the preview has $CUSTOM custom checks"; }
ok "the preview lists 6 custom SNMP checks: $(jq -r '[.items[] | select(.create.kind == "snmp_custom") | .create.name] | join(", ")' <<<"$IMPORT")"
RUN=$(send POST "/api/imports/$(jq -r .id <<<"$IMPORT")/confirm" '{}' | expect 200)
[[ $(jq -c .result.failed <<<"$RUN") == "[]" ]] || { echo "$RUN"; fail "the import failed something"; }
IMPORTED_FW=$(get "/api/servers?site_id=$SITE" | jq -r '.servers[] | select(.hostname == "fw-edge") | .id')
IMPORTED_NAS=$(get "/api/servers?site_id=$SITE" | jq -r '.servers[] | select(.hostname == "nas-files") | .id')
send PUT "/api/devices/$IMPORTED_FW/snmp" '{"version": "v2c", "community": "firewall"}' | expect 200 >/dev/null
send PUT "/api/devices/$IMPORTED_NAS/snmp" '{"version": "v2c", "community": "nas"}' | expect 200 >/dev/null
wait_between "$IMPORTED_FW" snmp_sessions sessions "" value 1000 2000 "the imported SNMP Custom sensor reads the sessions"
wait_text "$IMPORTED_FW" snmp_firewall_health ha "" "Master" "the imported Custom Advanced sensor's HA state, named by its lookup file"
wait_text "$IMPORTED_NAS" snmp_raid_volumes status "Volume 2" "Normal" "the imported Custom Table sensor, a row per volume"
wait_between "$IMPORTED_NAS" snmp_temperature temperature "" value 41.5 41.5 "the imported temperature, divided as PRTG did"
[[ $(get /api/snmp-lookups | jq '[.lookups[] | select(.name | startswith("oid.test."))] | length') == 2 ]] || fail "the lookup files did not become lookups"
ok "the two lookup files became lookups"

step "audit"
AUDIT=$(psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT string_agg(DISTINCT action, ',' ORDER BY action) FROM audit_log WHERE action LIKE 'snmp_%'")
[[ $AUDIT == snmp_check.create,snmp_lookup.create,snmp_mib.upload ]] || fail "audit: $AUDIT"
psql "$ADMIN_URL/$DB_NAME" -tAc "SELECT details::text FROM audit_log" | grep -qE '"(firewall|nas|e2e-community)"' && fail "a community in the audit log"
ok "audited, without communities"

step "the web interface"
if [[ -x /usr/bin/chromium ]] && command -v node >/dev/null; then
    (cd "$REPO/web" && [[ -d node_modules ]] || npm ci --no-audit --no-fund >/dev/null)
    (cd "$REPO/web" && MONITORFLOW_API_URL=$API setsid npx next dev -p "$WEB_PORT" >"$WORK/web.log" 2>&1 </dev/null & echo $! >"$WORK/web.pid")
    until_ok 120 curl -sf "http://localhost:$WEB_PORT/login/" || fail "the web interface did not start"
    mkdir -p "$WORK/browser" "$WORK/shots"
    [[ -d "$WORK/browser/node_modules/playwright-core" ]] || (cd "$WORK/browser" && npm init -y >/dev/null && npm i -q playwright-core@1 >/dev/null 2>&1)
    cp "$HERE/browser.mjs" "$WORK/browser/"
    SHOTS="$WORK/shots" node "$WORK/browser/browser.mjs" "http://localhost:$WEB_PORT" "$PASSWORD" "$FW" "$HERE/mibs/TEST-FW-MIB.txt" \
        || fail "the browser test"
    ok "screenshots in $WORK/shots"
else
    echo "note: no Chromium or node here: the browser half was not run"
fi

echo
echo "All passed. Server: $API (admin / $PASSWORD), log in $WORK/server.log."
[[ $CLEAN == --clean ]] || echo "Stop: $0 --clean, or kill \$(cat $WORK/server.pid) and docker rm -f the $P-* containers"
