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
for t in dns ntp web lossy blocked; do [[ -n $(ip_of $t) ]] || fail "no address for $t"; done
for _ in $(seq 60); do docker exec "$P-lossy" tc qdisc show dev eth0 2>/dev/null | grep -q netem && break; sleep 1; done
for _ in $(seq 60); do docker exec "$P-blocked" sh -c 'command -v iptables' >/dev/null 2>&1 && break; sleep 1; done
ok "dns $(ip_of dns), ntp $(ip_of ntp), web $(ip_of web), lossy $(ip_of lossy), blocked $(ip_of blocked)"

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

echo
echo "All passed. Server: $API (admin / $PASSWORD), log in $WORK/server.log."
[[ $CLEAN == --clean ]] || echo "Stop: $0 --clean, or kill \$(cat $WORK/server.pid) and docker rm -f the $P-* containers"
