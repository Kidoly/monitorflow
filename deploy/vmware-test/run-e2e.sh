#!/usr/bin/env bash
# End-to-end test of VMware checks (ADR-0156), against real software in
# local Docker containers only.
#
# 1. vcsim, govmomi's vCenter simulator, with two clusters of two hosts,
#    a standalone host, two datastores and their virtual machines, set up
#    with govc: two snapshots on DC0_H0_VM0, host DC0_C0_H1 disconnected,
#    and datastore LocalDS_1 filled by a virtual machine's 5100 GB disk.
#    The crate's tests run against it first (certificate pinning, wrong
#    password, paging).
# 2. A private MonitorFlow server from this checkout (its own database on
#    the PostgreSQL of DATABASE_URL, its own VictoriaMetrics), and a relay
#    of this checkout in a container, on vcsim's network. The vCenter's
#    check and its connection test run on the relay.
# 3. Through the API: tests the connection from the relay (certificate
#    shown, no login), trusts the fingerprint, adds the vCenter as a
#    device, and waits for its inventory, the virtual machines matched to
#    servers by UUID and by name, and the alerts of a disconnected host, a
#    full datastore and an old snapshot; reconnects the host and sees its
#    alert resolve; installs the agent on a virtual machine without one.
#
# Usage: run-e2e.sh            (leaves everything running for a look)
#        run-e2e.sh --clean    (removes the containers and data after)
# Needs: jq, curl, psql, docker, cargo, and DATABASE_URL pointing at a
# PostgreSQL the current user may create databases on.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CLEAN=${1:-}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-vmware-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_vmware_e2e
API_PORT=${MF_E2E_API_PORT:-27944}
GRPC_PORT=${MF_E2E_GRPC_PORT:-27943}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-27945}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-27980}
VM_PORT=${MF_E2E_VM_PORT:-27928}
VCSIM_PORT=${MF_E2E_VCSIM_PORT:-27989}
API=http://127.0.0.1:$API_PORT
PASSWORD=e2e-vmware-password-2026
VC_USER=reader
VC_PASSWORD=reader-pass
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
NET=mf-vmware-e2e
P=mf-vmware-e2e
VCSIM_IMAGE=vmware/vcsim:v0.52.0
GOVC_IMAGE=vmware/govc:v0.52.0

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

cleanup() {
    [[ -f "$WORK/server.pid" ]] && kill "$(cat "$WORK/server.pid")" 2>/dev/null || true
    docker rm -f $(docker ps -aq --filter "name=^$P-") >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
}
if [[ $CLEAN == --clean ]]; then trap 'cleanup; rm -rf "$WORK"' EXIT; fi
for port in "$API_PORT" "$GRPC_PORT" "$ENROLL_PORT" "$MIRROR_PORT" "$VM_PORT" "$VCSIM_PORT"; do
    ss -ltn "sport = :$port" | grep -q LISTEN && fail "port $port is taken: set MF_E2E_*_PORT"
done

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-relay)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
SERVER_BIN=$TARGET/debug/monitorflow-server
RELAY_BIN=$TARGET/debug/monitorflow-relay

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "vcsim"
cleanup
mkdir -p "$WORK"
docker network create "$NET" >/dev/null
ip_of() { docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "$P-$1"; }
docker run -d --name "$P-vcsim" --network "$NET" -p "127.0.0.1:$VCSIM_PORT:8989" "$VCSIM_IMAGE" \
    -l 0.0.0.0:8989 -username "$VC_USER" -password "$VC_PASSWORD" -cluster 2 -host 2 -ds 2 >/dev/null
VCSIM=$(ip_of vcsim)
govc() {
    docker run --rm --network "container:$P-vcsim" -e "GOVC_URL=https://$VC_USER:$VC_PASSWORD@127.0.0.1:8989/sdk" \
        -e GOVC_INSECURE=1 -e "GOVC_NETWORK=/DC0/network/VM Network" --entrypoint /govc "$GOVC_IMAGE" "$@"
}
for _ in $(seq 60); do govc about >/dev/null 2>&1 && break; sleep 1; done
govc about >/dev/null || fail "vcsim did not start"
govc snapshot.create -vm DC0_H0_VM0 before-upgrade >/dev/null
govc snapshot.create -vm DC0_H0_VM0 second >/dev/null
govc host.disconnect /DC0/host/DC0_C0/DC0_C0_H1 >/dev/null
govc vm.create -ds LocalDS_1 -host /DC0/host/DC0_C1/DC0_C1_H0 -pool /DC0/host/DC0_C1/Resources -disk 5100GB -on=false bigvm >/dev/null
VM0_UUID=$(govc vm.info DC0_H0_VM0 | awk '/UUID:/ {print $2; exit}')
[[ -n "$VM0_UUID" ]] || fail "no UUID for DC0_H0_VM0"
ok "vcsim at $VCSIM: 2 snapshots on DC0_H0_VM0 ($VM0_UUID), DC0_C0_H1 disconnected, LocalDS_1 full"

step "the vmware checks against vcsim"
(cd "$REPO" && MF_VCSIM="127.0.0.1:$VCSIM_PORT" MF_VCSIM_SCENARIO=1 cargo test -q -p monitorflow-vmware --test vcsim >/dev/null 2>&1) \
    || { (cd "$REPO" && MF_VCSIM="127.0.0.1:$VCSIM_PORT" MF_VCSIM_SCENARIO=1 cargo test -q -p monitorflow-vmware --test vcsim 2>&1 | tail -20); fail "the crate's tests failed against vcsim"; }
ok "an untrusted certificate is never logged in to, a pinned one is, a changed one and a wrong password say so, pages are followed"

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

step "a test of the connection from the relay"
test_vc() { # credentials JSON
    send POST /api/devices/vmware/test "$(jq -n --arg site "$SITE" --arg a "$VCSIM" --arg r "$RELAY" --argjson c "$1" \
        '{site_id: $site, address: $a, probe: $r, vmware: $c}')" | expect 200
}
CREDS=$(jq -n --arg u "$VC_USER" --arg p "$VC_PASSWORD" '{port: 8989, user: $u, password: $p}')
FIRST=$(test_vc "$CREDS")
[[ $(jq -r '.answers, .probe, .certificate.trusted, .login' <<<"$FIRST" | tr '\n' ' ') == "true relay-branch false null " ]] \
    || { echo "$FIRST"; fail "the first test should show an untrusted certificate without logging in"; }
PRINT=$(jq -r .certificate.fingerprint <<<"$FIRST")
ok "the relay reaches $(jq -r .product <<<"$FIRST"), certificate $PRINT not trusted, no password sent"
CREDS=$(jq --arg f "$PRINT" '. + {fingerprint: $f}' <<<"$CREDS")
SECOND=$(test_vc "$CREDS")
[[ $(jq -r .login <<<"$SECOND") == true ]] || { echo "$SECOND"; fail "the login failed with the trusted fingerprint"; }
ok "with its fingerprint trusted, the login works: $(jq -c .objects <<<"$SECOND")"
WRONG=$(test_vc "$(jq '.password = "wrong password"' <<<"$CREDS")")
[[ $(jq -r .login <<<"$WRONG") == false ]] || { echo "$WRONG"; fail "a wrong password logged in"; }
ok "a wrong password is refused: $(jq -r .error <<<"$WRONG")"

step "the vCenter as a device"
VC=$(send POST /api/devices "$(jq -n --arg site "$SITE" --arg a "$VCSIM" --arg r "$RELAY" --argjson c "$CREDS" \
    '{site_id: $site, name: "vcenter-1", address: $a, probe: $r, ping: false, vmware: $c}')" | expect 201 | jq -r .id)
get "/api/devices/$VC/vmware" | jq -e --arg p "$VC_PASSWORD" '.password_set and (tostring | contains($p) | not)' >/dev/null \
    || fail "the API showed the password, or lost it"
ok "vcenter-1 added; its password is never shown back"
# Faster readings for the test, the alerts at once, and a snapshot is
# old after a second (the default is 7 days).
get "/api/servers/$VC/sensor_configs" | jq -c '.entries[]' | while read -r row; do
    body=$(jq -c '{kind, name, instance, interval_ms: 15000, timeout_ms: 14000, params, enabled, probe: (if .probe == "relay" then .probe_relay_id else .probe end)}' <<<"$row")
    send PUT "/api/sensor_configs/$(jq -r .id <<<"$row")" "$body" | expect 200 >/dev/null
done
get "/api/servers/$VC/thresholds" | jq -c '.entries[]' | while read -r row; do
    body=$(jq -c '{metric, instance, comparison, warning: (if .metric == "monitorflow_vmware_vm_snapshot_age_seconds" then 1 else .warning end), critical, duration_s: 0, hysteresis, enabled}' <<<"$row")
    send PUT "/api/thresholds/$(jq -r .id <<<"$row")" "$body" | expect 200 >/dev/null
done
# Servers already monitored: one whose agent reports DC0_H0_VM0's UUID in
# the other byte order, one named like DC0_H0_VM1.
swapped=$(sed -E 's/^(..)(..)(..)(..)-(..)(..)-(..)(..)-/\4\3\2\1-\6\5-\8\7-/' <<<"$VM0_UUID")
psql "$ADMIN_URL/$DB_NAME" -qc "INSERT INTO servers (peer_id, site_id, hostname, os_family, system_uuid, awaiting_agent, package_updates)
    VALUES ('app-01', '$SITE', 'app-01', 'linux', '$swapped', true, false),
           ('dc0_h0_vm1', '$SITE', 'DC0_H0_VM1', 'linux', NULL, true, false)" >/dev/null
APP=$(get "/api/servers?site_id=$SITE" | jq -r '.servers[] | select(.hostname == "app-01") | .id')
ok "app-01 reports the UUID $swapped, DC0_H0_VM1 is a server by name"

step "the inventory"
virt() { get "/api/servers/$VC/virtualization"; }
object() { # kind, name: the object
    virt | jq -e --arg k "$1" --arg n "$2" '.objects[] | select(.kind == $k and .name == $n)'
}
measure() { # kind, name, measure, jq test of the value
    object "$1" "$2" | jq -e --arg m "$3" ".metrics[\$m] | $4"
}
until_ok 120 measure vm DC0_H0_VM0 snapshots '. == 2' || { virt | jq . | head -40; tail -20 "$WORK/server.log"; fail "no inventory"; }
INVENTORY=$(virt)
jq -e '.source.up' <<<"$INVENTORY" >/dev/null || fail "the source is not up: $(jq -c .source <<<"$INVENTORY")"
COUNTS=$(jq -c '[.objects | group_by(.kind)[] | {(.[0].kind): length}] | add' <<<"$INVENTORY")
ok "read through the relay: $COUNTS, $(jq -r .source.details.product <<<"$INVENTORY")"
measure host DC0_C0_H1 connected '. == 0' >/dev/null || fail "DC0_C0_H1 is not disconnected"
measure datastore LocalDS_1 free_ratio '. < 0.05' >/dev/null || fail "LocalDS_1 is not full"
measure vm DC0_H0_VM0 snapshot_age_seconds '. >= 0' >/dev/null || fail "no snapshot age"
ok "DC0_C0_H1 disconnected, LocalDS_1 $(measure datastore LocalDS_1 free_ratio '.') free, 2 snapshots on DC0_H0_VM0"
matched() { # name, server ID, signal
    object vm "$1" | jq -e --arg s "$2" --arg b "$3" '.server.id == $s and .matched_by == $b'
}
VM1=$(get "/api/servers?site_id=$SITE" | jq -r '.servers[] | select(.hostname == "DC0_H0_VM1") | .id')
until_ok 90 matched DC0_H0_VM0 "$APP" uuid || { object vm DC0_H0_VM0 | jq -c '{server, matched_by, details}'; fail "DC0_H0_VM0 not matched to app-01"; }
until_ok 30 matched DC0_H0_VM1 "$VM1" name || fail "DC0_H0_VM1 not matched by name"
ok "DC0_H0_VM0 is app-01 by its hardware ID, DC0_H0_VM1 the server of that name"
get "/api/servers/$APP/virtualization" | jq -e '.guest_of[0].source_name == "vcenter-1" and .guest_of[0].object.details.host == "DC0_H0"' >/dev/null \
    || fail "app-01's page does not show its virtual machine"
ok "app-01's page shows it runs on DC0_H0 of vcenter-1"

step "alerts"
open_incident() { # metric, instance
    get "/api/incidents?state=open&server_id=$VC&limit=50" | jq -e --arg m "$1" --arg i "$2" \
        '.incidents[] | select(.metric == $m and .instance == $i)'
}
until_ok 90 open_incident monitorflow_vmware_host_connected DC0_C0_H1 || fail "no alert for the disconnected host"
ok "host disconnected: $(open_incident monitorflow_vmware_host_connected DC0_C0_H1 | jq -r .severity)"
until_ok 60 open_incident monitorflow_vmware_datastore_free_ratio LocalDS_1 || fail "no alert for the full datastore"
ok "datastore full: $(open_incident monitorflow_vmware_datastore_free_ratio LocalDS_1 | jq -r .severity)"
until_ok 60 open_incident monitorflow_vmware_vm_snapshot_age_seconds DC0_H0_VM0 || fail "no alert for the old snapshot"
ok "old snapshot: $(open_incident monitorflow_vmware_vm_snapshot_age_seconds DC0_H0_VM0 | jq -r .severity)"
open_incident monitorflow_vmware_host_connected DC0_C0_H0 >/dev/null && fail "a connected host has an alert"
govc host.reconnect /DC0/host/DC0_C0/DC0_C0_H1 >/dev/null
resolved() { ! open_incident monitorflow_vmware_host_connected DC0_C0_H1; }
until_ok 120 resolved || fail "the host's alert did not resolve"
ok "the alert resolved once DC0_C0_H1 was reconnected"

step "install the agent on a virtual machine without one"
BIG=$(object vm bigvm | jq -r .id)
WAITING=$(send POST "/api/virtual-objects/$BIG/install-agent" '' | expect 201)
[[ $(jq -r '.hostname, .awaiting_agent' <<<"$WAITING" | tr '\n' ' ') == "bigvm true " ]] || { echo "$WAITING"; fail "no server waiting for its agent"; }
object vm bigvm | jq -e '.server.awaiting_agent and .matched_by == "person"' >/dev/null || fail "bigvm is not tied to its new server"
ok "bigvm is now a server waiting for its agent, tied to its virtual machine"

echo
echo "All passed. Server: $API (admin / $PASSWORD), log in $WORK/server.log."
[[ $CLEAN == --clean ]] || echo "Stop: $0 --clean, or kill \$(cat $WORK/server.pid) and docker rm -f the $P-* containers"
