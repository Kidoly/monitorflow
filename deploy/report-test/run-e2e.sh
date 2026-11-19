#!/usr/bin/env bash
# End-to-end test of availability reports (ADR-0155), against real
# software in local Docker containers only.
#
# 1. Starts a private MonitorFlow server from this checkout (its own
#    database on the PostgreSQL of DATABASE_URL, the API on loopback, gRPC
#    and enrollment on the Docker bridge only) and a mail catcher
#    (Mailpit).
# 2. Makes two customers, Acme and Globex, each with a site, a real agent
#    in a container (web-01 for Acme, db-01 for Globex), a port check of a
#    listener on the Docker bridge that the test stops and starts, a
#    business service with a target, and an administrator of its own.
# 3. Causes outages of known length: one on web-01, one on web-01 partly
#    under a silence made beforehand, one on db-01. Checks that the report
#    of today counts each to the second from the incidents' own times,
#    with the silence's 30 seconds left out and shown apart, and the
#    services' targets met or missed with the exact error budget.
# 4. Checks that each customer's administrator sees only its own report,
#    in JSON, CSV and PDF, and gets 404 for the other's.
# 5. Each administrator sets up a monthly report to Mailpit through an
#    email channel of its customer: Acme's sent with "Send now", Globex's
#    by the scheduler itself once its last month is due. Both emails are
#    read from Mailpit, their PDF (read with pdftotext) and CSV checked to
#    name their customer only.
# 6. Leaves everything running for browser.mjs unless MF_REPORT_E2E_STOP=1.
#
# Usage: run-e2e.sh
# Needs: jq, curl, docker, cargo, psql, python3, pdftotext, and
# DATABASE_URL pointing at a PostgreSQL the current user may create
# databases on.
set -euo pipefail
export LC_ALL=C

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-report-e2e}
DB_URL=${DATABASE_URL:?set DATABASE_URL to a PostgreSQL server, as in .env.example}
DB_NAME=mf_report_e2e
API_PORT=${MF_E2E_API_PORT:-29544}
GRPC_PORT=29543
ENROLL_PORT=29545
MIRROR_PORT=29580
SMTP_PORT=29525
MAIL_API_PORT=29526
SHOP_PORT=29591
DB_PORT=29592
API=http://127.0.0.1:$API_PORT
MAIL=http://127.0.0.1:$MAIL_API_PORT
PASSWORD=e2e-report-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
CONTAINERS="mf-report-mail mf-report-web-01 mf-report-db-01"

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent)
TARGET=${CARGO_TARGET_DIR:-target}
[[ $TARGET == /* ]] || TARGET=$REPO/$TARGET
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$BRIDGE" ]] || fail "no Docker bridge gateway"

step "a private MonitorFlow and a mail catcher"
for pidfile in "$WORK"/*.pid; do
    [[ -f "$pidfile" ]] && kill "$(cat "$pidfile")" 2>/dev/null || true
done
sleep 1
# shellcheck disable=SC2086
docker rm -f $CONTAINERS >/dev/null 2>&1 || true
rm -rf "$WORK"; mkdir -p "$WORK/server" "$WORK/files"
docker run -d --name mf-report-mail -p "$BRIDGE:$SMTP_PORT:1025" -p "127.0.0.1:$MAIL_API_PORT:8025" \
    axllent/mailpit:v1.21 >/dev/null

ADMIN_URL=${DB_URL%/*}
DB="$ADMIN_URL/$DB_NAME"
psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" -c "CREATE DATABASE $DB_NAME" >/dev/null
cat > "$WORK/server.toml" <<EOF
database_url = "$DB"
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
server() { NO_COLOR=1 "$SERVER_BIN" --config "$WORK/server.toml" "$@"; }
echo "$PASSWORD" | server user add --username admin --role admin >/dev/null
NO_COLOR=1 setsid "$SERVER_BIN" --config "$WORK/server.toml" >"$WORK/server.log" 2>&1 </dev/null &
echo $! >"$WORK/server.pid"
for _ in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
curl -sf "$API/api/health" >/dev/null || { tail -20 "$WORK/server.log"; fail "the server did not start"; }
# Another server on the same ports would answer too.
sleep 2
kill -0 "$(cat "$WORK/server.pid")" 2>/dev/null || { tail -5 "$WORK/server.log"; fail "the server stopped"; }
for _ in $(seq 30); do curl -sf "$MAIL/api/v1/messages" >/dev/null && break; sleep 1; done
ok "server on $API, Mailpit on $MAIL"

# login NAME: a cookie jar signed in as NAME.
login() {
    curl -sf -c "$WORK/$1.jar" -o /dev/null -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' \
        -d "{\"username\":\"$1\",\"password\":\"$PASSWORD\"}" "$API/api/auth/login" || fail "sign-in of $1"
}
AS=admin
get() { curl -sf -b "$WORK/$AS.jar" "$API$1"; }
status_of() { curl -s -o /dev/null -w '%{http_code}' -b "$WORK/$AS.jar" "$API$1"; }
send() { printf '%s' "$3" | curl -s -b "$WORK/$AS.jar" -X "$1" -H 'Content-Type: application/json' -H 'X-MonitorFlow-CSRF: 1' --data-binary @- -w '\n%{http_code}' "$API$2"; }
expect() {
    local out status
    out=$(cat); status=${out##*$'\n'}
    [[ "$status" == "$1" ]] || { echo "${out%$'\n'*}" >&2; fail "expected HTTP $1, got $status"; }
    echo "${out%$'\n'*}"
}
login admin

step "two customers, each with a site"
send PUT "/api/customers/$DEFAULT_CUSTOMER" '{"name":"Acme"}' | expect 200 >/dev/null
ACME=$DEFAULT_CUSTOMER
GLOBEX=$(send POST /api/customers '{"name":"Globex"}' | expect 201 | jq -r .id)
ACME_SITE=$(send POST "/api/customers/$ACME/sites" '{"name":"acme-paris"}' | expect 201 | jq -r .id)
GLOBEX_SITE=$(send POST "/api/customers/$GLOBEX/sites" '{"name":"globex-lyon"}' | expect 201 | jq -r .id)
for name in acme globex; do
    customer=$([[ $name == acme ]] && echo Acme || echo Globex)
    echo "$PASSWORD" | server user add --username "$name-admin" --role admin --customer "$customer" >/dev/null
    login "$name-admin"
done
ok "Acme and Globex, an administrator each"

step "listeners the checks watch, on the Docker bridge"
# listen PORT: an HTTP server the agents' port checks reach.
listen() {
    setsid python3 -m http.server --bind "$BRIDGE" "$1" >/dev/null 2>&1 </dev/null &
    echo $! >"$WORK/listen-$1.pid"
    for _ in $(seq 20); do (exec 3<>"/dev/tcp/$BRIDGE/$1") 2>/dev/null && return 0; sleep 0.5; done
    fail "the listener on $1 did not start"
}
quiet() {
    kill "$(cat "$WORK/listen-$1.pid")"
    rm -f "$WORK/listen-$1.pid"
    for _ in $(seq 20); do (exec 3<>"/dev/tcp/$BRIDGE/$1") 2>/dev/null || return 0; sleep 0.5; done
    fail "the listener on $1 did not stop"
}
listen $SHOP_PORT
listen $DB_PORT
ok "ports $SHOP_PORT and $DB_PORT answer"

step "an agent per customer: web-01 (Acme) and db-01 (Globex)"
agent() {
    local name=$1 site=$2 key
    key=$(send POST /api/install-keys "$(jq -n --arg site "$site" '{site_id: $site}')" | expect 201 \
        | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
    [[ -n "$key" ]] || fail "no install key for $name"
    mkdir -p "$WORK/$name"
    cat >"$WORK/$name.toml" <<EOF
upstream_url = "https://$BRIDGE:$GRPC_PORT"
enroll_url = "https://$BRIDGE:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$key"
heartbeat_interval = 10
EOF
    docker run -d --name "mf-report-$name" --hostname "$name" --user "$(id -u):$(id -g)" \
        -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/$name.toml:/agent/agent.toml:ro" \
        -v "$WORK/$name:/agent/data" -v /usr/lib:/hostlib:ro \
        debian:12 /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
}
agent web-01 "$ACME_SITE"
agent db-01 "$GLOBEX_SITE"
server_id() { get "/api/servers?limit=500" | jq -r --arg h "$1" '.servers[] | select(.hostname == $h) | .id'; }
for _ in $(seq 90); do
    WEB=$(server_id web-01); DBS=$(server_id db-01)
    if [[ -n "$WEB" && -n "$DBS" ]] \
        && [[ $(get "/api/servers/$WEB" | jq -r .status) == online && $(get "/api/servers/$DBS" | jq -r .status) == online ]]; then
        break
    fi
    sleep 2
done
[[ $(get "/api/servers/$WEB" | jq -r .status) == online ]] || { docker logs --tail 20 mf-report-web-01; fail "web-01 is not online"; }
[[ $(get "/api/servers/$DBS" | jq -r .status) == online ]] || fail "db-01 is not online"
ok "web-01 and db-01 online"

# watch SERVER NAME PORT: a port check of the listener every 10 s, critical
# when it does not answer.
watch() {
    send POST "/api/servers/$1/sensor_configs" "$(jq -n --arg n "$2" --arg h "$BRIDGE" --arg p "$3" \
        '{kind: "port", name: $n, interval_ms: 10000, params: {host: $h, port: $p}}')" | expect 201 >/dev/null
    send POST "/api/servers/$1/thresholds" "$(jq -n --arg m "monitorflow_${2}_up" \
        '{metric: $m, comparison: "<", critical: 1, duration_s: 0}')" | expect 201 >/dev/null
}
watch "$WEB" port_shop $SHOP_PORT
watch "$DBS" port_db $DB_PORT
# The checks reach the agents and run every 10 s: a few runs that answer.
sleep 30
send POST /api/business-services "$(jq -n --arg c "$ACME" --arg s "$WEB" \
    '{customer_id: $c, name: "Shop", rule: "all", target: 99.9,
      members: [{server_id: $s, check: {name: "port_shop", instance: ""}}]}')" | expect 201 >/dev/null
send POST /api/business-services "$(jq -n --arg c "$GLOBEX" --arg s "$DBS" \
    '{customer_id: $c, name: "Database", rule: "all", target: 99.0, members: [{server_id: $s}]}')" | expect 201 >/dev/null
ok "port checks answering, and a service per customer"

# closed SERVER NAME: the resolved incidents of the check, as
# "opened_at resolved_at" lines.
closed() {
    get "/api/incidents?state=resolved&limit=200" | jq -r --arg s "$1" --arg m "monitorflow_${2}_up" \
        '.incidents[] | select(.server_id == $s and .metric == $m) | "\(.opened_at) \(.resolved_at)"' | sort
}
# outage PORT SECONDS SERVER NAME COUNT: stops the listener for SECONDS,
# then waits until the check has COUNT resolved incidents.
outage() {
    quiet "$1"; sleep "$2"; listen "$1"
    for _ in $(seq 60); do
        [[ $(closed "$3" "$4" | wc -l) -ge $5 ]] && return 0
        sleep 2
    done
    fail "no resolved incident for $4"
}
secs() { date -u -d "$1" +%s.%N; }

step "outage 1: web-01's shop for 60 s"
outage $SHOP_PORT 60 "$WEB" port_shop 1
ok "$(closed "$WEB" port_shop | tail -1)"

step "outage 2: web-01's shop for 70 s, 30 s of it under a silence made beforehand"
FROM=$(date -u -d '+15 seconds' +%Y-%m-%dT%H:%M:%SZ)
UNTIL=$(date -u -d '+45 seconds' +%Y-%m-%dT%H:%M:%SZ)
send POST /api/silences "$(jq -n --arg s "$WEB" --arg f "$FROM" --arg u "$UNTIL" \
    '{scope: "server", target_id: $s, starts_at: $f, ends_at: $u, reason: "Shop patch"}')" | expect 201 >/dev/null
outage $SHOP_PORT 70 "$WEB" port_shop 2
ok "$(closed "$WEB" port_shop | tail -1), silence $FROM to $UNTIL"

step "outage 3: db-01's database for 90 s"
outage $DB_PORT 90 "$DBS" port_db 1
ok "$(closed "$DBS" port_db | tail -1)"

step "the report of today counts each outage to the second"
TODAY=$(date -u +%Y-%m-%dT00:00:00Z)
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
PERIOD="from=$TODAY&to=$NOW"
# The incidents' own lengths, and how much of the second one the silence
# covers.
mapfile -t SHOP < <(closed "$WEB" port_shop)
read -r a1 b1 <<<"${SHOP[0]}"; read -r a2 b2 <<<"${SHOP[1]}"
read -r a3 b3 < <(closed "$DBS" port_db)
python3 - "$a1" "$b1" "$a2" "$b2" "$FROM" "$UNTIL" "$a3" "$b3" >"$WORK/expected" <<'PY'
import sys
from datetime import datetime
t = [datetime.fromisoformat(x.replace("Z", "+00:00")).timestamp() for x in sys.argv[1:]]
a1, b1, a2, b2, s0, s1, a3, b3 = t
covered = max(0.0, min(b2, s1) - max(a2, s0))
print(f"{(b1 - a1) + (b2 - a2) - covered:.3f} {covered:.3f} {b1 - a1:.3f} {b2 - a2:.3f} {b3 - a3:.3f}")
PY
read -r WEB_DOWN COVERED LEN1 LEN2 DB_DOWN <"$WORK/expected"
echo "expected: web-01 down $WEB_DOWN s (outages $LEN1 s and $LEN2 s, $COVERED s planned), db-01 down $DB_DOWN s"
# Each outage lasted what the listener was stopped for, within a check
# interval either side.
python3 -c "import sys; l1,l2,l3=map(float,sys.argv[1:]); sys.exit(0 if 45<=l1<=80 and 55<=l2<=90 and 75<=l3<=110 else 1)" \
    "$LEN1" "$LEN2" "$DB_DOWN" || fail "outage lengths $LEN1 $LEN2 $DB_DOWN are not what was caused"
[[ $COVERED == 30.000 ]] || fail "the silence should cover 30 s of the second outage, not $COVERED"
get "/api/availability?$PERIOD" >"$WORK/report.json"
same() { python3 -c "import sys; sys.exit(0 if abs(float(sys.argv[1])-float(sys.argv[2])) < 0.001 else 1)" "$1" "$2"; }
machine() { jq -r --arg n "$1" ".machines[] | select(.name == \$n) | .numbers.$2" "$WORK/report.json"; }
same "$(machine web-01 down_seconds)" "$WEB_DOWN" || fail "web-01 down $(machine web-01 down_seconds), expected $WEB_DOWN"
same "$(machine web-01 maintenance_seconds)" 30 || fail "web-01 maintenance $(machine web-01 maintenance_seconds)"
# The silence in the middle of the second outage splits it in two.
[[ $(machine web-01 outages) == 3 ]] || fail "web-01 should have 3 outages, not $(machine web-01 outages)"
same "$(machine db-01 down_seconds)" "$DB_DOWN" || fail "db-01 down $(machine db-01 down_seconds), expected $DB_DOWN"
ok "web-01: $WEB_DOWN s down in 3 outages (the silence splits one), 30 s planned and left out; db-01: $DB_DOWN s"
counted=$(machine web-01 counted_seconds)
python3 - "$counted" "$WEB_DOWN" "$(machine web-01 availability)" <<'PY' || fail "web-01's availability is not (counted - down) / counted"
import sys
counted, down, shown = map(float, sys.argv[1:])
sys.exit(0 if abs(100 * (counted - down) / counted - shown) < 1e-9 else 1)
PY
service() { jq -r --arg n "$1" ".services[] | select(.name == \$n) | .$2" "$WORK/report.json"; }
same "$(service Shop numbers.down_seconds)" "$WEB_DOWN" || fail "the Shop service should be down as its check"
python3 - "$(service Shop numbers.counted_seconds)" "$WEB_DOWN" "$(service Shop budget_seconds)" <<'PY' || fail "the Shop's error budget"
import sys
counted, down, budget = map(float, sys.argv[1:])
sys.exit(0 if abs(0.001 * counted - down - budget) < 0.01 else 1)
PY
ok "Shop: $(service Shop numbers.availability) % against 99.9 %, met: $(service Shop met), budget $(service Shop budget_seconds) s"

step "each customer sees only its own report"
AS=acme-admin
get "/api/availability?$PERIOD" >"$WORK/acme.json"
[[ $(jq -r '[.machines[].name] | join(",")' "$WORK/acme.json") == web-01 ]] || fail "Acme sees $(jq -c '[.machines[].name]' "$WORK/acme.json")"
[[ $(jq -r '[.services[].name] | join(",")' "$WORK/acme.json") == Shop ]] || fail "Acme's services"
! grep -qi -e globex -e db-01 "$WORK/acme.json" || fail "Acme's report names Globex"
[[ $(status_of "/api/availability?$PERIOD&customer_id=$GLOBEX") == 404 ]] || fail "Acme reads Globex's report"
curl -sf -b "$WORK/acme-admin.jar" -o "$WORK/files/acme.pdf" "$API/api/availability/report?$PERIOD&format=pdf&time_zone=Europe/Paris"
curl -sf -b "$WORK/acme-admin.jar" -o "$WORK/files/acme.csv" "$API/api/availability/report?$PERIOD&format=csv"
pdftotext -layout "$WORK/files/acme.pdf" "$WORK/files/acme.txt"
grep -q "Availability report" "$WORK/files/acme.txt" || fail "the PDF has no title"
grep -q "web-01: check port_shop" "$WORK/files/acme.txt" || fail "the PDF lists no outage of web-01"
grep -q "Shop patch" "$WORK/files/acme.txt" || fail "the PDF shows no planned maintenance"
! grep -qi -e globex -e db-01 "$WORK/files/acme.txt" "$WORK/files/acme.csv" || fail "Acme's files name Globex"
grep -q "^server,web-01," "$WORK/files/acme.csv" || fail "the CSV has no row for web-01"
AS=globex-admin
get "/api/availability?$PERIOD" >"$WORK/globex.json"
[[ $(jq -r '[.machines[].name] | join(",")' "$WORK/globex.json") == db-01 ]] || fail "Globex sees $(jq -c '[.machines[].name]' "$WORK/globex.json")"
! grep -qi -e acme -e web-01 -e '"Shop"' "$WORK/globex.json" || fail "Globex's report names Acme"
[[ $(status_of "/api/availability?$PERIOD&customer_id=$ACME") == 404 ]] || fail "Globex reads Acme's report"
ok "Acme sees web-01 and Shop only, Globex db-01 and Database only, in JSON, CSV and PDF"

step "monthly reports by email, one per customer"
# The servers were added today: as if a month and a half ago, so that last
# month is theirs too. Nothing else is touched.
psql "$DB" -qc "UPDATE servers SET created_at = created_at - interval '45 days'"
# schedule CUSTOMER: an email channel of the customer to Mailpit and a
# monthly report through it, in French for Globex.
schedule() {
    local customer=$1 language=$2 channel who=$AS
    # The mail catcher is on a private address, which only a global
    # administrator may allow (ADR-0078): the MSP makes the customer's
    # channel, the customer's administrator the rest.
    AS=admin
    channel=$(send POST /api/notifications/channels "$(jq -n --arg h "$BRIDGE" --argjson p "$SMTP_PORT" --arg c "$customer" --arg to "$3" \
        '{kind: "email", customer_id: $c, settings: {host: $h, port: $p, username: "", from: "reports@example.com", to: [$to]}}')" \
        | expect 201 | jq -r .id)
    AS=$who
    send POST /api/report-schedules "$(jq -n --arg c "$customer" --arg ch "$channel" --arg l "$language" \
        '{customer_id: $c, frequency: "monthly", time_zone: "Europe/Paris", language: $l, channel_id: $ch}')" \
        | expect 201 | jq -r .id
}
AS=acme-admin
ACME_SCHEDULE=$(schedule "$ACME" en it@acme.example)
sent=$(send POST "/api/report-schedules/$ACME_SCHEDULE/send" '' | expect 200)
[[ $(jq -r .ok <<<"$sent") == true ]] || fail "Send now: $sent"
AS=globex-admin
GLOBEX_SCHEDULE=$(schedule "$GLOBEX" fr it@globex.example)
[[ $(status_of /api/report-schedules) == 200 ]] || fail "Globex lists its schedules"
[[ $(get /api/report-schedules | jq '.schedules | length') == 1 ]] || fail "Globex sees Acme's schedule"
# Made now, it starts with the next month; as if made before the last one
# ended, the scheduler sends it within a minute.
psql "$DB" -qc "UPDATE report_schedules SET last_period_end = NULL WHERE id = '$GLOBEX_SCHEDULE'"
for _ in $(seq 60); do
    [[ $(get /api/report-schedules | jq -r '.schedules[0].last_sent_at // empty') != "" ]] && break
    sleep 3
done
[[ $(get /api/report-schedules | jq -r '.schedules[0].last_sent_at // empty') != "" ]] || fail "the scheduler did not send Globex's report"
ok "Acme's sent now, Globex's sent by the scheduler"

# mail TO: the message to TO, its PDF as text and its CSV, in WORK/files.
mail() {
    local id parts
    # The channel carries the customer's incident notifications too: the
    # report is the message with files.
    id=$(curl -sf "$MAIL/api/v1/messages?limit=200" | jq -r --arg to "$1" \
        '[.messages[] | select(.To[0].Address == $to and .Attachments > 0)][0].ID // empty')
    [[ -n "$id" ]] || fail "no email to $1"
    curl -sf "$MAIL/api/v1/message/$id" >"$WORK/files/$1.json"
    parts=$(jq -r '.Attachments[] | "\(.PartID) \(.FileName)"' "$WORK/files/$1.json")
    while read -r part name; do
        curl -sf -o "$WORK/files/$1-$name" "$MAIL/api/v1/message/$id/part/$part"
    done <<<"$parts"
    echo "$parts"
}
LAST_MONTH=$(LC_ALL=C date -d "$(date +%Y-%m-01) -1 day" +"%B %Y")
for to in it@acme.example it@globex.example; do
    parts=$(mail "$to")
    pdf=$(awk '/\.pdf$/ {print $2}' <<<"$parts"); csv=$(awk '/\.csv$/ {print $2}' <<<"$parts")
    [[ -n "$pdf" && -n "$csv" ]] || fail "the email to $to lacks the PDF or the CSV: $parts"
    pdftotext -layout "$WORK/files/$to-$pdf" "$WORK/files/$to.txt"
    subject=$(jq -r .Subject "$WORK/files/$to.json")
    echo "  $to: \"$subject\", $pdf, $csv"
    if [[ $to == it@acme.example ]]; then
        grep -q "Availability report" "$WORK/files/$to.txt" && grep -q "For Acme" "$WORK/files/$to.txt" \
            && grep -q "$LAST_MONTH" "$WORK/files/$to.txt" || fail "Acme's PDF: $(head -5 "$WORK/files/$to.txt")"
        grep -q "web-01" "$WORK/files/$to.txt" || fail "Acme's PDF does not list web-01"
        ! grep -qi -e globex -e db-01 "$WORK/files/$to.txt" "$WORK/files/$to-$csv" || fail "Acme's email names Globex"
    else
        grep -q "Rapport de disponibilit" "$WORK/files/$to.txt" && grep -q "Pour Globex" "$WORK/files/$to.txt" \
            || fail "Globex's PDF: $(head -5 "$WORK/files/$to.txt")"
        grep -q "db-01" "$WORK/files/$to.txt" || fail "Globex's PDF does not list db-01"
        ! grep -qi -e acme -e web-01 -e shop "$WORK/files/$to.txt" "$WORK/files/$to-$csv" || fail "Globex's email names Acme"
    fi
done
ok "two emails with a PDF and a CSV each, read and each naming only its customer"

step "audit"
actions=$(psql "$DB" -Atc "SELECT string_agg(DISTINCT action, ' ') FROM audit_log
    WHERE action LIKE 'business_service.%' OR action LIKE 'report%'")
echo "audited: $actions"
for action in business_service.create report_schedule.create report.sent; do
    [[ " $actions " == *" $action "* ]] || fail "$action is not in the audit log"
done
[[ $(psql "$DB" -Atc "SELECT count(*) FROM audit_log WHERE action = 'report.sent'") == 2 ]] || fail "two reports sent"
ok "services, schedules and both sendings are in the audit log"

cat >"$WORK/env" <<EOF
API=$API
PASSWORD=$PASSWORD
ACME=$ACME
GLOBEX=$GLOBEX
TODAY=$TODAY
EOF
echo
echo "All passed. Server: $API (admin / $PASSWORD), Mailpit: $MAIL, files in $WORK/files."
if [[ ${MF_REPORT_E2E_STOP:-0} == 1 ]]; then
    for pidfile in "$WORK"/*.pid; do kill "$(cat "$pidfile")" 2>/dev/null || true; done
    # shellcheck disable=SC2086
    docker rm -f $CONTAINERS >/dev/null
    psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME WITH (FORCE)" >/dev/null || true
else
    echo "Stop: MF_REPORT_E2E_STOP=1 runs it again and stops; or kill the PIDs in $WORK/*.pid and docker rm -f $CONTAINERS"
fi
