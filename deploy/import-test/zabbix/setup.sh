#!/usr/bin/env bash
# Starts the throwaway Zabbix of compose.yml, configures it through its
# API the way a small real setup looks, and writes configuration exports
# for MonitorFlow's import tests (ADR-0146). Local Docker only.
#
#   deploy/import-test/zabbix/setup.sh [OUT_DIR]
#   ZABBIX_VERSION=6.0 deploy/import-test/zabbix/setup.sh /tmp/zbx-6.0
#
# OUT_DIR defaults to deploy/import-test/zabbix/out (gitignored). Running
# it again updates what exists instead of failing. At the end it prints
# the URL and an API token for a live import.
#
# Everything here is example data: example.com addresses, no real
# credentials.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/out}"
export ZABBIX_VERSION="${ZABBIX_VERSION:-7.0}"
URL="http://127.0.0.1:18080"
API="$URL/api_jsonrpc.php"
COMPOSE=(docker compose -p mf-import-zbx -f "$HERE/compose.yml")

for tool in curl jq docker; do
  command -v "$tool" >/dev/null || { echo "setup.sh needs $tool" >&2; exit 1; }
done

say() { printf '== %s\n' "$*" >&2; }

# Switching versions needs a fresh database: Zabbix does not downgrade.
running="$(docker inspect -f '{{.Config.Image}}' mf-import-zbx-server 2>/dev/null || true)"
if [[ -n "$running" && "$running" != *"ubuntu-$ZABBIX_VERSION-latest" ]]; then
  say "Removing the Zabbix of another version ($running)"
  "${COMPOSE[@]}" down -v
fi

say "Starting Zabbix $ZABBIX_VERSION"
"${COMPOSE[@]}" up -d >&2

AUTH=""
REQ_ID=0

# api METHOD PARAMS_JSON: the result, or exit with the error.
api() {
  local method="$1" params="$2" body answer
  REQ_ID=$((REQ_ID + 1))
  body="$(jq -nc --arg m "$method" --argjson p "$params" --argjson id "$REQ_ID" \
    '{jsonrpc: "2.0", method: $m, params: $p, id: $id}')"
  if [[ -n "$AUTH" ]]; then
    answer="$(curl -sS -X POST -H 'Content-Type: application/json-rpc' \
      -H "Authorization: Bearer $AUTH" --data "$body" "$API")"
  else
    answer="$(curl -sS -X POST -H 'Content-Type: application/json-rpc' --data "$body" "$API")"
  fi
  if jq -e 'has("error")' <<<"$answer" >/dev/null; then
    echo "$method failed: $(jq -c .error <<<"$answer")" >&2
    return 1
  fi
  jq -c .result <<<"$answer"
}

say "Waiting for the API"
# The web interface answers before the server has made its database:
# wait for a version, whatever comes before.
version=""
for _ in $(seq 200); do
  version="$(curl -s -m 5 -X POST -H 'Content-Type: application/json-rpc' \
    --data '{"jsonrpc": "2.0", "method": "apiinfo.version", "params": {}, "id": 1}' "$API" \
    | jq -r '.result // empty' 2>/dev/null || true)"
  [[ -n "$version" ]] && break
  sleep 3
done
[[ -n "$version" ]] || { echo "The Zabbix API did not answer at $API" >&2; exit 1; }
say "Zabbix API $version"
major="${version%%.*}"
minor="$(cut -d. -f2 <<<"$version")"

# Session for the setup, Admin with the stock password of a new install.
session="$(api user.login '{"username": "Admin", "password": "zabbix"}' | jq -r .)"
if ((major > 6 || (major == 6 && minor >= 4))); then
  AUTH="$session"
else
  # Before 6.4 the session goes in the request's auth field.
  api() {
    local method="$1" params="$2" body answer
    REQ_ID=$((REQ_ID + 1))
    body="$(jq -nc --arg m "$method" --argjson p "$params" --argjson id "$REQ_ID" --arg a "$session" \
      '{jsonrpc: "2.0", method: $m, params: $p, id: $id, auth: $a}')"
    answer="$(curl -sS -X POST -H 'Content-Type: application/json-rpc' --data "$body" "$API")"
    if jq -e 'has("error")' <<<"$answer" >/dev/null; then
      echo "$method failed: $(jq -c .error <<<"$answer")" >&2
      return 1
    fi
    jq -c .result <<<"$answer"
  }
fi
admin_id="$(api user.get '{"filter": {"username": "Admin"}, "output": ["userid"]}' | jq -r '.[0].userid')"

# id_of METHOD FILTER_JSON ID_FIELD: the ID of the first match, or empty.
id_of() {
  api "$1.get" "$(jq -nc --argjson f "$2" '{filter: $f, output: "extend"}')" | jq -r ".[0].$3 // empty"
}

# upsert OBJECT FILTER_JSON ID_FIELD PARAMS_JSON: updates the match or
# creates one; prints its ID.
upsert() {
  local object="$1" filter="$2" field="$3" params="$4" id
  id="$(id_of "$object" "$filter" "$field")"
  if [[ -n "$id" ]]; then
    api "$object.update" "$(jq -c --arg id "$id" --arg f "$field" '. + {($f): $id}' <<<"$params")" >/dev/null
    echo "$id"
  else
    api "$object.create" "$params" | jq -r ".[keys[0]][0]"
  fi
}

say "Host groups"
servers_group="$(upsert hostgroup '{"name": "Linux servers"}' groupid '{"name": "Linux servers"}')"
web_group="$(upsert hostgroup '{"name": "Web"}' groupid '{"name": "Web"}')"
db_group="$(upsert hostgroup '{"name": "Databases"}' groupid '{"name": "Databases"}')"

template_id() {
  api template.get "$(jq -nc --arg n "$1" '{filter: {host: $n}, output: ["templateid"]}')" \
    | jq -r '.[0].templateid // empty'
}
linux="$(template_id 'Linux by Zabbix agent')"
nginx="$(template_id 'Nginx by Zabbix agent')"
[[ -n "$linux" ]] || { echo "No 'Linux by Zabbix agent' template in this Zabbix" >&2; exit 1; }

say "Global macro"
global_id="$(api usermacro.get '{"globalmacro": true, "filter": {"macro": "{$DB.CPU.HIGH}"}}' | jq -r '.[0].globalmacroid // empty')"
if [[ -n "$global_id" ]]; then
  api usermacro.updateglobal "{\"globalmacroid\": \"$global_id\", \"value\": \"85\"}" >/dev/null
else
  api usermacro.createglobal '{"macro": "{$DB.CPU.HIGH}", "value": "85", "description": "CPU threshold of databases"}' >/dev/null
fi

# host NAME VISIBLE DNS GROUP_IDS TEMPLATE_IDS MACROS_JSON
host() {
  local name="$1" visible="$2" dns="$3" groups="$4" templates="$5" macros="$6" params id
  params="$(jq -nc --arg host "$name" --arg name "$visible" --arg dns "$dns" \
    --argjson groups "$groups" --argjson templates "$templates" --argjson macros "$macros" '{
      host: $host, name: $name,
      groups: [$groups[] | {groupid: .}],
      templates: [$templates[] | {templateid: .}],
      interfaces: [{type: 1, main: 1, useip: 0, ip: "", dns: $dns, port: "10050"}],
      macros: $macros,
      inventory_mode: 0,
      inventory: {os: "Linux (Ubuntu 24.04 LTS)"}
    }')"
  id="$(id_of host "{\"host\": \"$name\"}" hostid)"
  if [[ -n "$id" ]]; then
    # Interfaces are kept: items refer to them.
    api host.update "$(jq -c --arg id "$id" '. + {hostid: $id} | del(.interfaces)' <<<"$params")" >/dev/null
    echo "$id"
  else
    api host.create "$params" | jq -r '.hostids[0]'
  fi
}

say "Hosts"
web_templates="[\"$linux\"]"
[[ -n "$nginx" ]] && web_templates="[\"$linux\", \"$nginx\"]"
web01="$(host web-01 'Web server 01' mf-import-zbx-web-01 "[\"$servers_group\", \"$web_group\"]" "$web_templates" \
  '[{"macro": "{$CPU.UTIL.CRIT}", "value": "80", "description": "Busier than the default"},
    {"macro": "{$NGINX.STUB_STATUS.PATH}", "value": "basic_status"}]')"
db01="$(host db-01 'Database 01' mf-import-zbx-db-01 "[\"$servers_group\", \"$db_group\"]" "[\"$linux\"]" \
  '[{"macro": "{$VFS.FS.PUSED.MAX.CRIT:\"/var/lib/postgresql\"}", "value": "95"},
    {"macro": "{$MEMORY.UTIL.MAX}", "value": "95"}]')"
db_if="$(api hostinterface.get "{\"hostids\": \"$db01\", \"output\": [\"interfaceid\"]}" | jq -r '.[0].interfaceid')"
web_if="$(api hostinterface.get "{\"hostids\": \"$web01\", \"output\": [\"interfaceid\"]}" | jq -r '.[0].interfaceid')"

# item HOSTID INTERFACEID NAME KEY TYPE VALUE_TYPE DELAY
item() {
  local params
  params="$(jq -nc --arg h "$1" --arg i "$2" --arg n "$3" --arg k "$4" --argjson t "$5" --argjson v "$6" --arg d "$7" \
    '{hostid: $h, name: $n, key_: $k, type: $t, value_type: $v, delay: $d}
     + (if $t != 7 then {interfaceid: $i} else {} end)')"
  local id
  id="$(api item.get "$(jq -nc --arg h "$1" --arg k "$4" '{hostids: $h, filter: {key_: $k}, output: ["itemid"]}')" \
    | jq -r '.[0].itemid // empty')"
  if [[ -n "$id" ]]; then
    api item.update "$(jq -c --arg id "$id" '{itemid: $id, name, delay}' <<<"$params")" >/dev/null
  else
    api item.create "$params" >/dev/null
  fi
}

# trigger HOSTID NAME EXPRESSION PRIORITY [RECOVERY_EXPRESSION] [TAGS_JSON]
trigger() {
  local params id
  params="$(jq -nc --arg n "$2" --arg e "$3" --argjson p "$4" --arg r "${5:-}" --argjson t "${6:-[]}" \
    '{description: $n, expression: $e, priority: $p, tags: $t}
     + (if $r == "" then {recovery_mode: 0} else {recovery_mode: 1, recovery_expression: $r} end)')"
  id="$(api trigger.get "$(jq -nc --arg h "$1" --arg n "$2" '{hostids: $h, filter: {description: $n}, output: ["triggerid"]}')" \
    | jq -r '.[0].triggerid // empty')"
  if [[ -n "$id" ]]; then
    api trigger.update "$(jq -c --arg id "$id" '. + {triggerid: $id}' <<<"$params")" >/dev/null
  else
    api trigger.create "$params" >/dev/null
  fi
}

# Item types: 0 agent, 3 simple check, 7 active agent. Value types: 0
# float, 1 character, 2 log, 3 unsigned, 4 text.
say "Items and triggers"
item "$db01" "$db_if" "PostgreSQL port" 'net.tcp.port[,5432]' 0 3 30s
item "$db01" "$db_if" "Syslog errors" 'log[/var/log/syslog,error]' 7 2 1m
item "$db01" "$db_if" "PostgreSQL service" 'systemd.unit.info[postgresql.service]' 0 4 1m
item "$db01" "$db_if" "PostgreSQL processes" 'proc.num[postgres]' 0 3 1m
item "$db01" "$db_if" "NTP answers" 'net.udp.service[ntp]' 3 3 5m
item "$db01" "$db_if" "Backup age" 'backup.age[pg]' 0 3 1h
item "$web01" "$web_if" "SSH answers" 'net.tcp.service[ssh]' 0 3 1m
item "$web01" "$web_if" "Root space used" 'vfs.fs.size[/,pused]' 0 0 1m

trigger "$db01" "PostgreSQL port 5432 is down" 'max(/db-01/net.tcp.port[,5432],3m)=0' 4 "" \
  '[{"tag": "service", "value": "postgresql"}]'
trigger "$db01" "Errors in syslog" 'find(/db-01/log[/var/log/syslog,error],,"regexp","error")=1' 2
trigger "$db01" "PostgreSQL service is not running" 'last(/db-01/systemd.unit.info[postgresql.service])<>"active"' 3
trigger "$db01" "No PostgreSQL process" 'last(/db-01/proc.num[postgres])<1' 5
trigger "$db01" "Database CPU is high" 'min(/db-01/system.cpu.util,10m)>{$DB.CPU.HIGH}' 5 \
  'max(/db-01/system.cpu.util,5m)<70'
trigger "$db01" "Backup is old" 'last(/db-01/backup.age[pg])>1d' 2
trigger "$web01" "Root file system is filling up" 'last(/web-01/vfs.fs.size[/,pused])>85' 2
trigger "$web01" "SSH is down" 'last(/web-01/net.tcp.service[ssh])=0' 4

say "Web scenario"
scenario_id="$(api httptest.get "{\"hostids\": \"$web01\", \"filter\": {\"name\": \"Shop home\"}, \"output\": [\"httptestid\"]}" | jq -r '.[0].httptestid // empty')"
steps='[{"no": 1, "name": "Home", "url": "http://127.0.0.1/", "status_codes": "200"},
        {"no": 2, "name": "Status", "url": "http://127.0.0.1/basic_status", "status_codes": "200"}]'
if [[ -n "$scenario_id" ]]; then
  api httptest.update "{\"httptestid\": \"$scenario_id\", \"delay\": \"2m\", \"steps\": $steps}" >/dev/null
else
  api httptest.create "{\"hostid\": \"$web01\", \"name\": \"Shop home\", \"delay\": \"2m\", \"steps\": $steps}" >/dev/null
fi
trigger "$web01" "Shop home page fails" 'last(/web-01/web.test.fail[Shop home])<>0' 4 "" \
  '[{"tag": "component", "value": "shop"}]'


say "Maintenances"
# A one-time window a week from now (whole hours), and a weekly one on
# Sundays 02:00 to 04:00 for a year.
now="$(date -u +%s)"
start=$(((now / 3600 + 24 * 7) * 3600))
maintenance() {
  local name="$1" params="$2" id
  id="$(id_of maintenance "{\"name\": \"$name\"}" maintenanceid)"
  if [[ -n "$id" ]]; then
    api maintenance.update "$(jq -c --arg id "$id" '. + {maintenanceid: $id}' <<<"$params")" >/dev/null
  else
    api maintenance.create "$params" >/dev/null
  fi
}
maintenance "Database upgrade" "$(jq -nc --arg s "$start" --arg e "$((start + 3 * 86400))" --arg h "$db01" \
  --arg p "$start" '{name: "Database upgrade", description: "PostgreSQL 17", active_since: $s, active_till: $e,
    hosts: [{hostid: $h}],
    timeperiods: [{timeperiod_type: 0, start_date: $p, period: 14400}]}')"
maintenance "Weekly patching" "$(jq -nc --arg s "$((now / 86400 * 86400))" --arg e "$((now / 86400 * 86400 + 365 * 86400))" \
  --arg g "$servers_group" '{name: "Weekly patching", maintenance_type: 0,
    active_since: $s, active_till: $e, groups: [{groupid: $g}],
    timeperiods: [{timeperiod_type: 3, every: 1, dayofweek: 64, start_time: 7200, period: 7200}]}')"
# Recurring ones of every kind (ADR-0151): the second Tuesday and Friday
# of January and July for the problems tagged service=postgres*, and a
# nightly hour without data collection.
maintenance "Database work" "$(jq -nc --arg s "$((now / 86400 * 86400))" --arg e "$((now / 86400 * 86400 + 365 * 86400))" \
  --arg h "$db01" '{name: "Database work", maintenance_type: 0, active_since: $s, active_till: $e,
    hosts: [{hostid: $h}], tags_evaltype: 0, tags: [{tag: "service", operator: 0, value: "postgres"}],
    timeperiods: [{timeperiod_type: 4, every: 2, month: 65, dayofweek: 18, start_time: 79200, period: 14400}]}')"
maintenance "Nightly backups" "$(jq -nc --arg s "$((now / 86400 * 86400))" --arg e "$((now / 86400 * 86400 + 365 * 86400))" \
  --arg h "$db01" '{name: "Nightly backups", maintenance_type: 1, active_since: $s, active_till: $e,
    hosts: [{hostid: $h}],
    timeperiods: [{timeperiod_type: 2, every: 1, start_time: 3600, period: 3600}]}')"

if ((major >= 6)); then
  say "Services and SLA"
  # service NAME PARAMS_JSON: made or brought up to date, by name; prints
  # its ID.
  service() {
    local id
    id="$(id_of service "{\"name\": \"$1\"}" serviceid)"
    if [[ -n "$id" ]]; then
      api service.update "$(jq -c --arg id "$id" '. + {serviceid: $id}' <<<"$2")" >/dev/null
    else
      id="$(api service.create "$2" | jq -r '.serviceids[0]')"
    fi
    echo "$id"
  }
  # The database's PostgreSQL problems, and a shop of its home page and
  # the database, up while all of them are; both on a gold SLA.
  database="$(service "Database" '{"name": "Database", "algorithm": 2, "sortorder": 0,
    "problem_tags": [{"tag": "service", "operator": 0, "value": "postgresql"}],
    "tags": [{"tag": "sla", "value": "gold"}]}')"
  shop_web="$(service "Shop web" '{"name": "Shop web", "algorithm": 2, "sortorder": 0,
    "problem_tags": [{"tag": "component", "operator": 0, "value": "shop"}]}')"
  service "Shop" "$(jq -nc --arg d "$database" --arg w "$shop_web" '{name: "Shop", algorithm: 2,
    sortorder: 0, children: [{serviceid: $d}, {serviceid: $w}], tags: [{tag: "sla", value: "gold"}]}')" >/dev/null
  sla_id="$(id_of sla '{"name": "Gold"}' slaid)"
  sla="$(jq -nc --argjson d "$((now / 86400 * 86400))" '{name: "Gold", slo: "99.9", period: 2,
    timezone: "UTC", effective_date: $d,
    service_tags: [{tag: "sla", operator: 0, value: "gold"}]}')"
  if [[ -n "$sla_id" ]]; then
    api sla.update "$(jq -c --arg id "$sla_id" '. + {slaid: $id}' <<<"$sla")" >/dev/null
  else
    api sla.create "$sla" >/dev/null
  fi
fi

say "Media types"
email_id="$(upsert mediatype '{"name": "Ops mail"}' mediatypeid '{
  "name": "Ops mail", "type": 0, "status": 0, "smtp_server": "mail.example.com", "smtp_port": 587,
  "smtp_helo": "example.com", "smtp_email": "zabbix@example.com", "smtp_security": 1,
  "smtp_authentication": 1, "username": "zabbix@example.com", "passwd": "example-only",
  "content_type": 0}')"
webhook_id="$(upsert mediatype '{"name": "Ops webhook"}' mediatypeid '{
  "name": "Ops webhook", "type": 4, "status": 0,
  "parameters": [{"name": "URL", "value": "https://hooks.example.com/zabbix/0123456789"},
                 {"name": "Subject", "value": "{ALERT.SUBJECT}"},
                 {"name": "Message", "value": "{ALERT.MESSAGE}"}],
  "script": "var p = JSON.parse(value); var r = new HttpRequest(); r.addHeader(\"Content-Type: application/json\"); r.post(p.URL, JSON.stringify({subject: p.Subject, message: p.Message})); return \"OK\";"}')"
# Stock chat media types: "MS Teams" before 7.0, "MS Teams Workflow" since.
teams_id="$(id_of mediatype '{"name": "MS Teams Workflow"}' mediatypeid)"
[[ -n "$teams_id" ]] || teams_id="$(id_of mediatype '{"name": "MS Teams"}' mediatypeid)"
discord_id="$(id_of mediatype '{"name": "Discord"}' mediatypeid)"
for id in $teams_id $discord_id; do
  api mediatype.update "{\"mediatypeid\": \"$id\", \"status\": 0}" >/dev/null
done
slack_id="$(id_of mediatype '{"name": "Slack"}' mediatypeid)"
sms_id="$(id_of mediatype '{"name": "SMS"}' mediatypeid)"

say "Media of Admin"
medias="$(jq -nc --arg e "$email_id" --arg w "$webhook_id" --arg t "$teams_id" --arg s "$slack_id" --arg m "$sms_id" --arg d "$discord_id" '[
  {mediatypeid: $e, sendto: ["admin@example.com", "oncall@example.com"], active: 0, severity: 63, period: "1-7,00:00-24:00"},
  {mediatypeid: $w, sendto: "ops", active: 0, severity: 60, period: "1-7,00:00-24:00"}]
  + (if $t != "" then [{mediatypeid: $t, sendto: "https://example.webhook.office.com/webhookb2/0123456789", active: 0, severity: 48, period: "1-7,00:00-24:00"}] else [] end)
  + (if $d != "" then [{mediatypeid: $d, sendto: "https://discord.com/api/webhooks/0123456789/example", active: 0, severity: 48, period: "1-7,00:00-24:00"}] else [] end)
  + (if $s != "" then [{mediatypeid: $s, sendto: "#ops", active: 0, severity: 48, period: "1-7,00:00-24:00"}] else [] end)
  + (if $m != "" then [{mediatypeid: $m, sendto: "+15550100", active: 1, severity: 32, period: "1-7,00:00-24:00"}] else [] end)')"
api user.update "{\"userid\": \"$admin_id\", \"medias\": $medias}" >/dev/null

say "API token"
token_id="$(api token.get "{\"userids\": \"$admin_id\", \"filter\": {\"name\": \"MonitorFlow import\"}, \"output\": [\"tokenid\"]}" | jq -r '.[0].tokenid // empty')"
if [[ -z "$token_id" ]]; then
  token_id="$(api token.create "{\"name\": \"MonitorFlow import\", \"userid\": \"$admin_id\"}" | jq -r '.tokenids[0]')"
fi
token="$(api token.generate "[\"$token_id\"]" | jq -r '.[0].token')"

say "Exports to $OUT"
mkdir -p "$OUT"
template_ids="$(api host.get "{\"hostids\": [\"$web01\", \"$db01\"], \"selectParentTemplates\": [\"templateid\"], \"output\": [\"hostid\"]}" \
  | jq -c '[.[].parentTemplates[].templateid] | unique')"
# The media types made here and the stock SMS one: the stock chat
# webhooks carry long scripts and are read from the API instead.
mediatype_ids="$(jq -nc --arg e "$email_id" --arg w "$webhook_id" --arg m "$sms_id" \
  '[$e, $w, $m] | map(select(. != ""))')"
for format in yaml xml json; do
  ext="$format"
  for kind in hosts templates media_types; do
    case "$kind" in
      hosts) options="{\"hosts\": [\"$web01\", \"$db01\"]}" ;;
      templates) options="{\"templates\": $template_ids}" ;;
      media_types) options="{\"mediaTypes\": $mediatype_ids}" ;;
    esac
    params="$(jq -nc --arg f "$format" --argjson o "$options" '{format: $f, options: $o, prettyprint: true}')"
    api configuration.export "$params" | jq -r . >"$OUT/$kind.$ext"
  done
done
# For run-e2e.sh: a local test stack's address and throwaway token.
printf '%s\n' "$URL" >"$OUT/url"
(umask 077 && printf '%s\n' "$token" >"$OUT/token")
ls -l "$OUT" >&2

cat <<EOF
Zabbix $version is configured.
  Web interface: $URL (Admin / zabbix)
  API URL:       $URL
  API token:     $token
Live import test:
  MF_ZABBIX_URL=$URL MF_ZABBIX_TOKEN=$token cargo test -p monitorflow-import --test zabbix -- --ignored
EOF
