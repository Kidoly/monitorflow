#!/usr/bin/env bash
# The import half of the dependency test (ADR-0152): starts a throwaway
# Zabbix 7.0 (zabbix-compose.yml, local Docker only), gives it a switch
# watched by ping and a server whose triggers depend on the switch's, and
# imports it into the MonitorFlow that run-e2e.sh left running
# (MF_DEPENDENCY_E2E_KEEP=1), from an export file and from the live API.
# Checks the dependencies the preview proposes and the confirmation
# makes, and undoes them.
#
#   zabbix.sh API_URL PASSWORD
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
MF=${1:?the MonitorFlow API URL}
PASSWORD=${2:?the admin password}
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-dependency-e2e}/zabbix
ZBX=http://127.0.0.1:28080
COMPOSE=(docker compose -f "$HERE/zabbix-compose.yml")

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $* ($(date +%T))"; }
mkdir -p "$WORK"

step "a throwaway Zabbix 7.0"
"${COMPOSE[@]}" up -d >/dev/null 2>&1
AUTH=""
api() {
    local body answer
    body=$(jq -nc --arg m "$1" --argjson p "$2" '{jsonrpc: "2.0", method: $m, params: $p, id: 1}')
    if [[ -n $AUTH ]]; then
        answer=$(curl -sS -X POST -H 'Content-Type: application/json-rpc' -H "Authorization: Bearer $AUTH" --data "$body" "$ZBX/api_jsonrpc.php")
    else
        answer=$(curl -sS -X POST -H 'Content-Type: application/json-rpc' --data "$body" "$ZBX/api_jsonrpc.php")
    fi
    jq -e 'has("error")' <<<"$answer" >/dev/null && { echo "$1: $(jq -c .error <<<"$answer")" >&2; return 1; }
    jq -c .result <<<"$answer"
}
VERSION=""
for _ in $(seq 120); do VERSION=$(api apiinfo.version '{}' 2>/dev/null | jq -r .) && [[ -n $VERSION ]] && break; sleep 3; done
for _ in $(seq 60); do
    AUTH=$(api user.login '{"username": "Admin", "password": "zabbix"}' 2>/dev/null | jq -r .) && [[ -n $AUTH && $AUTH != null ]] && break
    AUTH=""; sleep 3
done
[[ -n $AUTH ]] || fail "cannot sign in to Zabbix"
ok "Zabbix $VERSION on $ZBX"

step "a switch watched by ping, and shop-01 behind it"
group=$(api hostgroup.get '{"filter": {"name": ["Network"]}, "output": ["groupid"]}' | jq -r '.[0].groupid // empty')
[[ -n $group ]] || group=$(api hostgroup.create '{"name": "Network"}' | jq -r '.groupids[0]')
host() { # NAME IP agent|snmp: its ID and its interface's
    local id interface
    if [[ $3 == agent ]]; then
        interface='{type: 1, main: 1, useip: 1, ip: $ip, dns: "", port: "10050"}'
    else
        interface='{type: 2, main: 1, useip: 1, ip: $ip, dns: "", port: "161", details: {version: 2, community: "public"}}'
    fi
    id=$(api host.get "$(jq -nc --arg h "$1" '{filter: {host: [$h]}, output: ["hostid"]}')" | jq -r '.[0].hostid // empty')
    if [[ -z $id ]]; then
        id=$(api host.create "$(jq -nc --arg h "$1" --arg ip "$2" --arg g "$group" \
            "{host: \$h, groups: [{groupid: \$g}], interfaces: [$interface]}")" \
            | jq -r '.hostids[0]')
    fi
    echo "$id $(api hostinterface.get "$(jq -nc --arg h "$id" '{hostids: $h, output: ["interfaceid"]}')" | jq -r '.[0].interfaceid')"
}
item() { # HOST INTERFACE NAME KEY TYPE VALUE_TYPE
    api item.get "$(jq -nc --arg h "$1" --arg k "$4" '{hostids: $h, filter: {key_: $k}, output: ["itemid"]}')" | jq -e 'length > 0' >/dev/null && return
    api item.create "$(jq -nc --arg h "$1" --arg i "$2" --arg n "$3" --arg k "$4" --argjson t "$5" --argjson v "$6" \
        '{hostid: $h, interfaceid: $i, name: $n, key_: $k, type: $t, value_type: $v, delay: "1m"}')" >/dev/null
}
trigger() { # NAME EXPRESSION PRIORITY: its ID
    local id
    id=$(api trigger.get "$(jq -nc --arg n "$1" '{filter: {description: $n}, output: ["triggerid"]}')" | jq -r '.[0].triggerid // empty')
    [[ -n $id ]] || id=$(api trigger.create "$(jq -nc --arg n "$1" --arg e "$2" --argjson p "$3" \
        '{description: $n, expression: $e, priority: $p}')" | jq -r '.triggerids[0]')
    echo "$id"
}
read -r sw sw_if <<<"$(host core-switch 192.0.2.2 snmp)"
read -r app app_if <<<"$(host shop-01 192.0.2.40 agent)"
item "$sw" "$sw_if" "Switch answers ping" icmpping 3 3
item "$app" "$app_if" "Agent answers" agent.ping 0 3
item "$app" "$app_if" "HTTPS port" 'net.tcp.port[,443]' 0 3
switch=$(trigger "Switch is unreachable" 'max(/core-switch/icmpping,3m)=0' 4)
agent=$(trigger "Agent is unavailable" 'nodata(/shop-01/agent.ping,5m)=1' 4)
https=$(trigger "HTTPS is down" 'max(/shop-01/net.tcp.port[,443],3m)=0' 4)
for id in "$agent" "$https"; do
    api trigger.update "$(jq -nc --arg id "$id" --arg on "$switch" '{triggerid: $id, dependencies: [{triggerid: $on}]}')" >/dev/null
done
api trigger.get "$(jq -nc --arg id "$https" '{triggerids: $id, selectDependencies: ["description"], output: ["description"]}')" \
    | jq -e '.[0].dependencies[0].description == "Switch is unreachable"' >/dev/null || fail "Zabbix did not keep the dependency"
api configuration.export "$(jq -nc --arg a "$sw" --arg b "$app" '{format: "yaml", options: {hosts: [$a, $b]}, prettyprint: true}')" \
    | jq -r . >"$WORK/hosts.yaml"
grep -q "dependencies:" "$WORK/hosts.yaml" || fail "the export has no dependencies"
token_id=$(api token.get '{"filter": {"name": "MonitorFlow dependencies"}, "output": ["tokenid"]}' | jq -r '.[0].tokenid // empty')
[[ -n $token_id ]] || token_id=$(api token.create '{"name": "MonitorFlow dependencies", "userid": "1"}' | jq -r '.tokenids[0]')
TOKEN=$(api token.generate "[\"$token_id\"]" | jq -r '.[0].token')
ok "two triggers of shop-01 depend on the switch's, exported to $WORK/hosts.yaml"

step "into MonitorFlow"
JAR=$WORK/cookies
curl -sf -c "$JAR" -o /dev/null -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" "$MF/api/auth/login" || fail "sign-in to MonitorFlow"
get() { curl -sf -b "$JAR" "$MF$1"; }
send() { printf '%s' "$3" | curl -s -b "$JAR" -X "$1" -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' --data-binary @- -w '\n%{http_code}' "$MF$2"; }
expect() {
    local out status
    out=$(cat); status=${out##*$'\n'}
    [[ "$status" == "$1" ]] || { echo "${out%$'\n'*}" >&2; fail "expected HTTP $1, got $status"; }
    echo "${out%$'\n'*}"
}
SITE=$(get /api/sites | jq -r '.sites[0].id')
proposed() { jq -c '[.items[] | select(.kind == "dependency") | {id, list}] | sort_by(.id)' <<<"$1"; }
WANTED='[{"id":"dependency:shop-01/port_443:core-switch","list":"exact"},{"id":"dependency:shop-01:core-switch","list":"exact"}]'

run=$(jq -n --arg site "$SITE" --rawfile c "$WORK/hosts.yaml" \
    '{source: "zabbix", site_id: $site, files: [{name: "hosts.yaml", content: $c}]}' \
    | send POST /api/imports "$(cat)" | expect 201)
[[ $(proposed "$run") == "$WANTED" ]] || fail "the export proposes $(proposed "$run")"
ok "from the export: $(proposed "$run")"
live=$(send POST /api/imports "$(jq -n --arg site "$SITE" --arg u "$ZBX" --arg t "$TOKEN" \
    '{source: "zabbix", site_id: $site, connection: {url: $u, token: $t}}')" | expect 201)
[[ $(proposed "$live") == "$WANTED" ]] || fail "the live API proposes $(proposed "$live")"
ok "from the live API: the same"

confirmed=$(send POST "/api/imports/$(jq -r .id <<<"$live")/confirm" '{}' | expect 200)
[[ $(jq '.result.failed | length' <<<"$confirmed") == 0 ]] || fail "failed: $(jq -c .result.failed <<<"$confirmed")"
[[ $(jq '.result.created.dependency' <<<"$confirmed") == 2 ]] || fail "created $(jq -c .result.created <<<"$confirmed")"
app_id=$(get "/api/servers?limit=500" | jq -r '.servers[] | select(.hostname == "shop-01" and .awaiting_agent) | .id')
[[ -n $app_id ]] || fail "no imported shop-01"
made=$(get "/api/dependencies?server_id=$app_id" | jq -c '[.dependencies[] | {child: (.child.check.name // "all"), parent: .parent.name, kind: .parent.kind}] | sort_by(.child)')
[[ $made == '[{"child":"all","parent":"core-switch","kind":"device"},{"child":"port_443","parent":"core-switch","kind":"device"}]' ]] \
    || fail "MonitorFlow has $made"
ok "confirmed: shop-01 and its port_443 check depend on the device core-switch"
send POST "/api/imports/$(jq -r .id <<<"$live")/undo" '' | expect 200 >/dev/null
[[ $(get "/api/dependencies" | jq '[.dependencies[] | select(.parent.name == "core-switch")] | length') == 0 ]] \
    || fail "the undo left dependencies"
ok "undone"

if [[ ${MF_DEPENDENCY_ZABBIX_KEEP:-0} != 1 ]]; then
    "${COMPOSE[@]}" down -v >/dev/null 2>&1
fi
echo "All passed."
