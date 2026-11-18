#!/usr/bin/env bash
# End-to-end test of network checks from a probe (ADR-0148), against real
# software in local Docker containers only.
#
# 1. A private MonitorFlow server from this checkout (its own database on
#    the PostgreSQL of DATABASE_URL, its own VictoriaMetrics container, API
#    on loopback, gRPC and enrollment on the Docker bridge only), and a
#    relay of this checkout in a container, enrolled with an install key.
# 2. Targets on a Docker network of their own: a DNS server, an NTP
#    server, a web server with a port nothing listens on, one that loses
#    half its packets, and one that drops the relay's pings only.
# 3. Adds them as devices through the API with ping, TCP and UDP checks
#    from the relay and from the central server, and checks the values,
#    an alert that fires on the relay's check only, and its recovery.
# 4. SNMP (ADR-0149): net-snmp's snmpd with v2c and v3 users, and
#    snmpsim playing the recordings of snmp/data (a switch, a UPS, a
#    printer). Runs the crate's tests against both, detects what they
#    answer through the relay, adds them from templates, checks their
#    values, and puts the UPS on battery to see its alert fire and recover.
#
# Usage: run-e2e.sh            (leaves everything running for a look)
#        run-e2e.sh --clean    (removes the containers and data after)
# Needs: jq, curl, psql, docker, cargo, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CLEAN=${1:-}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-probe-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_probe_e2e
API_PORT=${MF_E2E_API_PORT:-17544}
GRPC_PORT=${MF_E2E_GRPC_PORT:-17543}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-17545}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-17580}
VM_PORT=${MF_E2E_VM_PORT:-17528}
API=http://127.0.0.1:$API_PORT
PASSWORD=e2e-probe-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
NET=mf-probe-e2e
P=mf-probe-e2e

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

cleanup() {
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
mkdir -p "$WORK"
cat >"$WORK/Corefile" <<'EOF'
. {
    hosts {
        192.0.2.53 probe-e2e.test
    }
    errors
}
EOF
docker run -d --name "$P-dns" --network "$NET" -v "$WORK/Corefile:/Corefile:ro" coredns/coredns:1.12.1 -conf /Corefile >/dev/null
docker run -d --name "$P-ntp" --network "$NET" cturra/ntp:latest >/dev/null
docker run -d --name "$P-web" --network "$NET" nginx:1.27-alpine >/dev/null
# Loses half of what it sends: its pings come back half the time.
docker run -d --name "$P-lossy" --network "$NET" --cap-add NET_ADMIN nginx:1.27-alpine \
    sh -c 'apk add -q --no-cache iproute2-tc >/dev/null && tc qdisc add dev eth0 root netem loss 50% && exec nginx -g "daemon off;"' >/dev/null
# Drops pings from the relay only, once told which address it has.
docker run -d --name "$P-blocked" --network "$NET" --cap-add NET_ADMIN nginx:1.27-alpine \
    sh -c 'apk add -q --no-cache iptables >/dev/null && exec nginx -g "daemon off;"' >/dev/null
# SNMP: net-snmp (with its tools, for SETs) and snmpsim.
docker run -d --name "$P-snmpd" --network "$NET" -v "$HERE/snmp/snmpd.conf:/conf/snmpd.conf:ro" debian:trixie-slim \
    sh -c 'apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends snmpd snmp >/dev/null && exec snmpd -f -Lo -C -c /conf/snmpd.conf' >/dev/null
docker run -d --name "$P-snmpsim" --network "$NET" -v "$HERE/snmp/data:/data:ro" python:3.12-slim \
    sh -c 'pip install -q --root-user-action=ignore snmpsim pysmi >/dev/null 2>&1 && exec snmpsim-command-responder --data-dir=/data --agent-udpv4-endpoint=0.0.0.0:161 --process-user=nobody --process-group=nogroup' >/dev/null
for t in dns ntp web lossy blocked snmpd snmpsim; do [[ -n $(ip_of $t) ]] || fail "no address for $t"; done
for _ in $(seq 60); do docker exec "$P-lossy" tc qdisc show dev eth0 2>/dev/null | grep -q netem && break; sleep 1; done
for _ in $(seq 60); do docker exec "$P-blocked" sh -c 'command -v iptables' >/dev/null 2>&1 && break; sleep 1; done
ok "dns $(ip_of dns), ntp $(ip_of ntp), web $(ip_of web), lossy $(ip_of lossy), blocked $(ip_of blocked)"

for _ in $(seq 120); do docker logs "$P-snmpd" 2>&1 | grep -q "NET-SNMP version" && break; sleep 1; done
for _ in $(seq 120); do docker logs "$P-snmpsim" 2>&1 | grep -q "Listening at" && break; sleep 1; done
ok "snmpd $(ip_of snmpd), snmpsim $(ip_of snmpsim)"

step "the SNMP checks against snmpd and snmpsim"
(cd "$REPO" && MF_SNMPD="$(ip_of snmpd):161" MF_SNMPSIM="$(ip_of snmpsim):161" cargo test -q -p monitorflow-snmp --tests 2>&1 | tail -3)
(cd "$REPO" && MF_SNMPD="$(ip_of snmpd):161" MF_SNMPSIM="$(ip_of snmpsim):161" cargo test -q -p monitorflow-snmp --tests >/dev/null 2>&1) \
    || fail "the SNMP checks failed against the real agents"
ok "v2c, v3 (SHA-256/AES-128, SHA-512/AES-256), interfaces, host resources, a switch, a UPS and a printer"

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
RELAY_IP=$(ip_of relay)
relay_online() { get /api/relays | jq -e '.relays[] | select(.peer_id == "relay-branch" and .status == "online")'; }
until_ok 90 relay_online || { docker logs --tail 30 "$P-relay"; fail "the relay did not connect"; }
RELAY=$(get /api/relays | jq -r '.relays[] | select(.peer_id == "relay-branch") | .id')
NAMES=$(get "/api/relays/$RELAY" | jq -c '.pending_names // []')
if [[ $NAMES != "[]" ]]; then send POST "/api/relays/$RELAY/names" "$(jq -n --argjson n "$NAMES" '{names: $n}')" | expect 204 >/dev/null; fi
ok "relay-branch online at $RELAY_IP"
PROBES=$(get "/api/sites/$SITE/probes")
[[ $(jq -r '.probes[] | select(.default) | .key' <<<"$PROBES") == relay-branch ]] || { echo "$PROBES"; fail "the relay is not the default probe"; }
ok "the relay is the site's default probe"

step "devices"
add_device() { # name, address, [probe]: prints the device ID
    local body
    body=$(jq -n --arg site "$SITE" --arg n "$1" --arg a "$2" --arg p "${3:-}" \
        '{site_id: $site, name: $n, address: $a} + (if $p == "" then {} else {probe: $p} end)')
    send POST /api/devices "$body" | expect 201 | jq -r .id
}
add_check() { # device, JSON row: prints the row
    send POST "/api/servers/$1/sensor_configs" "$2" | expect 201
}
DNS=$(add_device "DNS server" "$(ip_of dns)")
add_check "$DNS" '{"kind": "port", "name": "udp_53", "interval_ms": 10000, "params": {"port": 53, "protocol": "udp"}}' >/dev/null
add_check "$DNS" '{"kind": "port", "name": "udp_53", "interval_ms": 10000, "params": {"port": 53, "protocol": "udp"}, "probe": "central"}' >/dev/null
NTP=$(add_device "NTP server" "$(ip_of ntp)" central)
add_check "$NTP" '{"kind": "port", "name": "udp_123", "interval_ms": 10000, "params": {"port": 123, "protocol": "udp"}, "probe": "central"}' >/dev/null
WEB=$(add_device "Web server" "$(ip_of web)" central)
add_check "$WEB" '{"kind": "port", "name": "port_80", "interval_ms": 10000, "params": {"port": 80}, "probe": "central"}' >/dev/null
add_check "$WEB" '{"kind": "port", "name": "port_81", "interval_ms": 10000, "params": {"port": 81}, "probe": "central"}' >/dev/null
add_check "$WEB" '{"kind": "port", "name": "udp_9999", "interval_ms": 10000, "params": {"port": 9999, "protocol": "udp", "query": "none"}, "probe": "central"}' >/dev/null
add_check "$WEB" '{"kind": "port", "name": "udp_53", "interval_ms": 10000, "params": {"port": 53, "protocol": "udp", "query": "dns"}, "probe": "central"}' >/dev/null
LOSSY=$(add_device "Lossy link" "$(ip_of lossy)")
BLOCKED=$(add_device "Branch NAS" "$(ip_of blocked)")
add_check "$BLOCKED" '{"kind": "ping", "name": "ping", "interval_ms": 10000, "params": {}, "probe": "central"}' >/dev/null
# Faster pings for the test: every 10 s, 20 echoes on the lossy link.
for d in "$DNS" "$LOSSY" "$BLOCKED" "$NTP" "$WEB"; do
    get "/api/servers/$d/sensor_configs" | jq -c '.entries[] | select(.kind == "ping")' | while read -r row; do
        id=$(jq -r .id <<<"$row")
        count=$([[ $d == "$LOSSY" ]] && echo 20 || echo 3)
        body=$(jq -c --argjson c "$count" '{kind, name, instance, interval_ms: 10000, timeout_ms: 9000, params: (.params + {count: $c}), enabled, probe: (if .probe == "relay" then .probe_relay_id else .probe end)}' <<<"$row")
        send PUT "/api/sensor_configs/$id" "$body" | expect 200 >/dev/null
    done
    get "/api/servers/$d/thresholds" | jq -c '.entries[]' | while read -r row; do
        id=$(jq -r .id <<<"$row")
        send PUT "/api/thresholds/$id" "$(jq -c '{metric, instance, comparison, warning, critical, duration_s: 20, hysteresis, enabled}' <<<"$row")" | expect 200 >/dev/null
    done
done
docker exec "$P-blocked" iptables -A INPUT -s "$RELAY_IP" -p icmp -j DROP
ok "5 devices with their checks; the NAS drops the relay's pings"

step "values"
# The last value of the metric $2 on device $1, instance $3 (the probe).
value() {
    get "/api/servers/$1/sensor-values" | jq -er --arg m "$2" --arg i "$3" \
        '.sensors[] | select(.metric == $m) | .series[] | select(.instance == $i) | .last_value // empty'
}
is() { # the values are numbers: 1.0 is 1
    local got; got=$(value "$1" "$2" "$3") || return 1
    awk -v a="$got" -v b="$4" 'BEGIN { exit !(a + 0 == b + 0) }'
}
wait_value() { # device, metric, instance, value, what
    until_ok 90 is "$1" "$2" "$3" "$4" || { get "/api/servers/$1/sensor-values" | jq -c '.sensors[] | {metric, series: [.series[] | {instance, last_value}]}' >&2; fail "$5"; }
    ok "$5"
}
wait_value "$DNS" monitorflow_ping_up relay-branch 1 "the DNS server answers ping from the relay"
wait_value "$DNS" monitorflow_udp_53_up relay-branch 1 "the DNS server answers a DNS query from the relay"
wait_value "$DNS" monitorflow_udp_53_up central 1 "the DNS server answers a DNS query from the central server"
wait_value "$NTP" monitorflow_udp_123_up central 1 "the NTP server answers an NTP request"
wait_value "$WEB" monitorflow_port_80_up central 1 "port 80 of the web server is open"
wait_value "$WEB" monitorflow_port_81_up central 0 "port 81 of the web server is closed"
wait_value "$WEB" monitorflow_udp_9999_up central 0 "UDP 9999 of the web server is refused"
wait_value "$WEB" monitorflow_udp_53_up central 0 "the web server does not answer DNS"
wait_value "$BLOCKED" monitorflow_ping_up central 1 "the NAS answers the central server"
wait_value "$BLOCKED" monitorflow_ping_up relay-branch 0 "the NAS does not answer the relay"
# 20 echoes at 50% loss: all or none answered is a chance in a million.
partial_loss() {
    LOSS=$(curl -sf -b "$JAR" -G "$API/api/metrics/query" --data-urlencode "server_id=$LOSSY" \
        --data-urlencode metric=monitorflow_ping_loss_ratio \
        | jq -er '.series[] | select(.labels.probe == "relay-branch") | .points[-1][1]')
    awk -v l="$LOSS" 'BEGIN { exit !(l > 0.1 && l < 0.9) }'
}
LOSS=""
until_ok 90 partial_loss || fail "the lossy link reports a loss of ${LOSS:-nothing}"
ok "the lossy link loses $LOSS of its pings"

step "alerts"
open_incident() { # device, metric, instance
    get "/api/incidents?state=open&server_id=$1&limit=50" | jq -e --arg m "$2" --arg i "$3" \
        '.incidents[] | select(.metric == $m and .instance == $i)'
}
until_ok 90 open_incident "$BLOCKED" monitorflow_ping_up relay-branch || fail "no incident for the relay's ping of the NAS"
ok "an incident opened: the relay cannot ping the NAS"
open_incident "$BLOCKED" monitorflow_ping_up central && fail "an incident opened for the central server's ping"
ok "the central server's ping of the NAS opened none"
SUMMARY=$(get "/api/servers/summary?site_id=$SITE")
[[ $(jq '.offline + (.waiting // 0)' <<<"$SUMMARY") == 0 ]] || { echo "$SUMMARY"; fail "a device shows offline or waiting"; }
STATUSES=$(get "/api/servers?site_id=$SITE" | jq -r '[.servers[] | .status] | unique | join(",")')
[[ $STATUSES == device ]] || fail "devices have the status $STATUSES"
ok "devices are never offline nor waiting for an agent: $(jq -c . <<<"$SUMMARY")"
docker exec "$P-blocked" iptables -D INPUT -s "$RELAY_IP" -p icmp -j DROP
resolved() { ! open_incident "$BLOCKED" monitorflow_ping_up relay-branch; }
until_ok 120 resolved || fail "the incident did not resolve"
ok "the incident resolved once the relay's pings got through"

step "the relay is believed for its own site's devices only"
OTHER=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"elsewhere"}' | expect 201 | jq -r .id)
send POST /api/devices "$(jq -n --arg site "$OTHER" --arg r "$RELAY" '{site_id: $site, name: "x", address: "192.0.2.1", probe: $r}')" | expect 400 >/dev/null
ok "a check of another site cannot run from this relay"

step "SNMP devices (ADR-0149)"
SNMPD=$(ip_of snmpd); SIM=$(ip_of snmpsim)
v3='{"version": "v3", "user": "e2e-sha512", "auth_protocol": "sha512", "auth_password": "e2e-auth-password", "priv_protocol": "aes256", "priv_password": "e2e-priv-password"}'
detect() { # address, credentials JSON, [probe]
    send POST /api/devices/detect "$(jq -n --arg site "$SITE" --arg a "$1" --argjson c "$2" --arg p "${3:-}" \
        '{site_id: $site, address: $a, snmp: $c} + (if $p == "" then {} else {probe: $p} end)')" | expect 200
}
FOUND=$(detect "$SNMPD" "$v3")
[[ $(jq -r .probe <<<"$FOUND") == relay-branch ]] || { echo "$FOUND"; fail "the detection did not run on the relay"; }
[[ $(jq -r '.answers, .name, .template' <<<"$FOUND" | tr '\n' ' ') == "true nas-e2e nas " ]] || { echo "$FOUND"; fail "snmpd was not detected as a NAS"; }
ok "the relay detects net-snmp over v3 as a NAS: $(jq -r .description <<<"$FOUND" | cut -c1-40)"
WRONG=$(detect "$SNMPD" '{"version": "v2c", "community": "wrong"}')
[[ $(jq -r .answers <<<"$WRONG") == false ]] || fail "a wrong community was answered"
ok "a wrong community gets no answer: $(jq -r .error <<<"$WRONG" | cut -c1-60)"
for device in switch ups printer; do
    found=$(detect "$SIM" "{\"version\": \"v2c\", \"community\": \"$device\"}" central)
    [[ $(jq -r .template <<<"$found") == "$device" || ($device == switch && $(jq -r .template <<<"$found") == switch) ]] \
        || { echo "$found"; fail "the $device recording was detected as $(jq -r .template <<<"$found")"; }
done
ok "the switch, UPS and printer recordings are detected as such"

add_snmp() { # name, address, template, credentials JSON, probe
    send POST /api/devices "$(jq -n --arg site "$SITE" --arg n "$1" --arg a "$2" --arg t "$3" --argjson c "$4" --arg p "$5" \
        '{site_id: $site, name: $n, address: $a, template: $t, snmp: $c, probe: $p, ping: false}')" | expect 201 | jq -r .id
}
NAS=$(add_snmp "NAS" "$SNMPD" nas "$v3" "$RELAY")
SWITCH=$(add_snmp "Floor switch" "$SIM" switch '{"version": "v2c", "community": "switch"}' central)
UPS=$(add_snmp "UPS" "$SIM" ups '{"version": "v2c", "community": "ups"}' central)
PRINTER=$(add_snmp "Printer" "$SIM" printer '{"version": "v2c", "community": "printer"}' central)
get "/api/devices/$NAS/snmp" | jq -e '.auth_password_set and (tostring | contains("e2e-auth") | not)' >/dev/null \
    || fail "the API showed a secret, or lost one"
ok "4 SNMP devices from templates; their credentials are never shown back"
# Faster polls for the test, and the alerts at once.
for d in "$NAS" "$SWITCH" "$UPS" "$PRINTER"; do
    get "/api/servers/$d/sensor_configs" | jq -c '.entries[]' | while read -r row; do
        body=$(jq -c '{kind, name, instance, interval_ms: 10000, timeout_ms: 9000, params, enabled, probe: (if .probe == "relay" then .probe_relay_id else .probe end)}' <<<"$row")
        send PUT "/api/sensor_configs/$(jq -r .id <<<"$row")" "$body" | expect 200 >/dev/null
    done
    get "/api/servers/$d/thresholds" | jq -c '.entries[]' | while read -r row; do
        send PUT "/api/thresholds/$(jq -r .id <<<"$row")" "$(jq -c '{metric, instance, comparison, warning, critical, duration_s: 0, hysteresis, enabled}' <<<"$row")" | expect 200 >/dev/null
    done
done
metric() { # device, metric, label filter (jq): the last value
    curl -sf -b "$JAR" -G "$API/api/metrics/query" --data-urlencode "server_id=$1" --data-urlencode "metric=$2" \
        | jq -er "[.series[] | select($3) | .points[-1][1]] | first // empty"
}
near() { # device, metric, filter, low, high
    local v; v=$(metric "$1" "$2" "$3") || return 1
    awk -v v="$v" -v lo="$4" -v hi="$5" 'BEGIN { exit !(v >= lo && v <= hi) }'
}
wait_near() { # device, metric, filter, low, high, what
    until_ok 120 near "$1" "$2" "$3" "$4" "$5" || fail "$6 ($(metric "$1" "$2" "$3" || echo nothing))"
    ok "$6: $(metric "$1" "$2" "$3")"
}
wait_near "$NAS" monitorflow_snmp_up 'true' 1 1 "the NAS answers SNMP v3 through the relay"
wait_near "$NAS" monitorflow_snmp_host_memory_used_ratio 'true' 0.01 1 "its memory in use"
wait_near "$NAS" monitorflow_snmp_if_oper_up '.labels.instance | startswith("eth")' 1 1 "its eth interface is up"
wait_near "$SWITCH" monitorflow_snmp_if_in_bytes_per_second '.labels.instance == "Gi0/1"' 100000 150000 "the switch's Gi0/1 traffic in, in bytes a second"
wait_near "$SWITCH" monitorflow_snmp_if_oper_up '.labels.instance == "Gi0/12"' 0 0 "Gi0/12 is down"
wait_near "$SWITCH" monitorflow_snmp_temp_celsius '.labels.instance == "Chassis"' 42 42 "the chassis temperature"
wait_near "$UPS" monitorflow_snmp_ups_battery_charge_ratio 'true' 1 1 "the UPS battery charge"
wait_near "$UPS" monitorflow_snmp_ups_runtime_seconds 'true' 2700 2700 "its runtime left"
wait_near "$PRINTER" monitorflow_snmp_printer_supply_level_ratio '.labels.instance == "Black Toner"' 0.06 0.06 "the black toner level"

open_metric() { # device, metric
    get "/api/incidents?state=open&server_id=$1&limit=50" | jq -e --arg m "$2" '.incidents[] | select(.metric == $m)'
}
until_ok 90 open_metric "$PRINTER" monitorflow_snmp_printer_supply_level_ratio || fail "no alert for the low toner"
ok "the low black toner raised an alert: $(open_metric "$PRINTER" monitorflow_snmp_printer_supply_level_ratio | jq -r .severity)"
open_metric "$UPS" monitorflow_snmp_ups_on_battery >/dev/null && fail "the UPS is on battery before the test"
# On battery: snmpsim's writable recording takes a SET of upsOutputSource.
docker exec "$P-snmpd" snmpset -v2c -c ups "$SIM" 1.3.6.1.2.1.33.1.4.1.0 i 5 >/dev/null || fail "the SET was refused"
until_ok 90 open_metric "$UPS" monitorflow_snmp_ups_on_battery || fail "no alert when the UPS went on battery"
ok "the UPS on battery raised a $(open_metric "$UPS" monitorflow_snmp_ups_on_battery | jq -r .severity) alert"
docker exec "$P-snmpd" snmpset -v2c -c ups "$SIM" 1.3.6.1.2.1.33.1.4.1.0 i 3 >/dev/null
back() { ! open_metric "$UPS" monitorflow_snmp_ups_on_battery; }
until_ok 90 back || fail "the alert did not resolve"
ok "it resolved once the UPS was back on mains"

echo
echo "All passed. Server: $API (admin / $PASSWORD), log in $WORK/server.log."
[[ $CLEAN == --clean ]] || echo "Stop: $0 --clean, or kill \$(cat $WORK/server.pid) and docker rm -f the $P-* containers"
