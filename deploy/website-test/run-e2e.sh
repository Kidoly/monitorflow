#!/usr/bin/env bash
# End-to-end test of website checks (ADR-0150), against real software in
# local Docker containers only.
#
# 1. Makes a throwaway certificate authority and certificates (certs.sh),
#    and starts nginx serving pages with chosen status codes, text,
#    redirects, a slow page and one behind a password, over HTTP and over
#    HTTPS with a valid certificate, an expired one, one for another name,
#    one expiring in 5 days and a self-signed one; plus VictoriaMetrics and
#    VictoriaLogs.
# 2. Starts a private MonitorFlow server from this checkout, trusting the
#    test authority, and an agent of this checkout in a container that
#    reaches the sites as web.test, and a relay of the site there too.
# 3. Adds website checks through the API, run by the agent and by the
#    central server and by the relay, and checks what they measure through the API, alerts
#    opening and resolving, and the server page in Chromium.
#
# Usage: run-e2e.sh   (MF_TEST_KEEP=1 leaves everything running)
# Needs: docker with compose, jq, curl, openssl, psql, cargo, npm, node,
# Chromium (MF_TEST_CHROMIUM picks one). DATABASE_URL points at a
# PostgreSQL the current user may create databases on; without it, one
# runs in a container.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
WORK=${MF_E2E_DIR:-${TMPDIR:-/tmp}/mf-website-e2e}
DB_URL=${DATABASE_URL:-}
DB_NAME=mf_website_e2e
export MF_API_PORT=${MF_E2E_API_PORT:-27744}
export MF_UI_PORT=${MF_E2E_UI_PORT:-27790}
GRPC_PORT=${MF_E2E_GRPC_PORT:-27743}
ENROLL_PORT=${MF_E2E_ENROLL_PORT:-27745}
MIRROR_PORT=${MF_E2E_MIRROR_PORT:-27780}
API=http://127.0.0.1:$MF_API_PORT
PASSWORD=e2e-website-password-2026
DEFAULT_CUSTOMER=00000000-0000-0000-0000-000000000001
AGENT=mf-website-e2e-agent
RELAY=mf-website-e2e-relay

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
step() { echo; echo "== $*"; }
for tool in docker jq curl openssl psql cargo; do
    command -v "$tool" >/dev/null || fail "needs $tool"
done

export MF_WEBSITE_OUT=$WORK/out MF_WEB_OUT=$REPO/web/out
compose() { docker compose -f "$HERE/compose.yml" "$@"; }
cleanup() {
    local status=$?
    if [[ "${MF_TEST_KEEP:-}" == 1 ]]; then
        echo "kept running: $API (admin / $PASSWORD), web interface http://localhost:$MF_UI_PORT, state in $WORK"
        return
    fi
    if [[ $status -ne 0 ]]; then
        echo "--- server log"; tail -n 40 "$WORK/server.log" 2>/dev/null || true
        echo "--- agent log"; docker logs --tail 30 "$AGENT" 2>&1 || true
        echo "--- relay log"; docker logs --tail 30 "$RELAY" 2>&1 || true
    fi
    [[ -f "$WORK/server.pid" ]] && kill "$(cat "$WORK/server.pid")" 2>/dev/null || true
    docker rm -f "$AGENT" "$RELAY" >/dev/null 2>&1 || true
    compose --profile db down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

step "certificates, pages and sites"
# What an earlier run left, if it was kept.
[[ -f "$WORK/server.pid" ]] && kill "$(cat "$WORK/server.pid")" 2>/dev/null || true
docker rm -f "$AGENT" "$RELAY" >/dev/null 2>&1 || true
compose --profile db down -v --remove-orphans >/dev/null 2>&1 || true
rm -rf "$WORK"; mkdir -p "$WORK/server" "$WORK/agent" "$MF_WEBSITE_OUT/pages"
"$HERE/certs.sh" "$MF_WEBSITE_OUT/certs" >/dev/null
P=$MF_WEBSITE_OUT/pages
echo "<h1>Welcome to the shop</h1>" >"$P/index.html"
echo "<h1>Database Error</h1>" >"$P/error.html"
head -c 40000 /dev/zero | tr '\0' 'x' >"$P/big.html"
printf 'monitor:%s\n' "$(openssl passwd -apr1 s3cret-page)" >"$P/htpasswd"
chmod -R a+rX "$MF_WEBSITE_OUT"
mkdir -p "$MF_WEB_OUT"
if [[ -z "$DB_URL" ]]; then
    compose --profile db up -d --quiet-pull postgres >/dev/null
    DB_URL=postgres://monitorflow:monitorflow@127.0.0.1:18432/monitorflow
    for _ in $(seq 30); do psql "$DB_URL" -qc 'SELECT 1' >/dev/null 2>&1 && break; sleep 1; done
fi
compose up -d --quiet-pull web victoriametrics victorialogs >/dev/null
for _ in $(seq 30); do curl -sf http://127.0.0.1:18080/ >/dev/null && break; sleep 1; done
curl -sf --cacert "$MF_WEBSITE_OUT/certs/ca.pem" https://127.0.0.1:18443/ | grep -q Welcome || fail "nginx does not serve the shop"
ok "nginx serves the sites"

step "build"
(cd "$REPO" && cargo build -q -p monitorflow-server -p monitorflow-agent -p monitorflow-relay)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
SERVER_BIN=$TARGET/debug/monitorflow-server
AGENT_BIN=$TARGET/debug/monitorflow-agent
RELAY_BIN=$TARGET/debug/monitorflow-relay

# The sites network's gateway: where its containers reach the host.
NET=mf-website-test_sites
GATEWAY=$(docker network inspect "$NET" -f '{{(index .IPAM.Config 0).Gateway}}')
[[ -n "$GATEWAY" ]] || fail "no gateway on $NET"

step "a private MonitorFlow in $WORK"
for port in "$MF_API_PORT" "$MIRROR_PORT" "$MF_UI_PORT"; do
    ! ss -ltn "sport = :$port" | grep -q LISTEN || fail "port $port is taken; set MF_E2E_*_PORT"
done
ADMIN_URL=${DB_URL%/*}
psql "$ADMIN_URL/postgres" -qc "DROP DATABASE IF EXISTS $DB_NAME" -c "CREATE DATABASE $DB_NAME" >/dev/null
cat >"$WORK/server.toml" <<EOF
database_url = "$ADMIN_URL/$DB_NAME"
api_cookie_secure = false
data_dir = "$WORK/server"
server_names = ["localhost", "127.0.0.1", "$GATEWAY"]
api_listen_addr = "127.0.0.1:$MF_API_PORT"
grpc_listen_addr = "$GATEWAY:$GRPC_PORT"
enroll_listen_addr = "$GATEWAY:$ENROLL_PORT"
mirror_listen_addr = "127.0.0.1:$MIRROR_PORT"
victoriametrics_url = "http://127.0.0.1:18428"
victorialogs_url = "http://127.0.0.1:18429"

[tls]
ca_cert = "$WORK/server/tls/ca.pem"
cert = "$WORK/server/tls/cert.pem"
key = "$WORK/server/tls/key.pem"
EOF
# The central server trusts the test authority as a machine trusts its own.
cat "$MF_WEBSITE_OUT/certs/ca.pem" >"$WORK/roots.pem"
echo "$PASSWORD" | NO_COLOR=1 "$SERVER_BIN" --config "$WORK/server.toml" user add --username admin --role admin >/dev/null
SSL_CERT_FILE=$WORK/roots.pem NO_COLOR=1 setsid "$SERVER_BIN" --config "$WORK/server.toml" >"$WORK/server.log" 2>&1 </dev/null &
echo $! >"$WORK/server.pid"
for _ in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
curl -sf "$API/api/health" >/dev/null || fail "the server did not start"
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
SITE=$(get /api/sites | jq -r '.sites[0].id // empty')
[[ -n "$SITE" ]] || SITE=$(send POST "/api/customers/$DEFAULT_CUSTOMER/sites" '{"name":"main"}' | expect 201 | jq -r .id)

step "an agent that reaches the sites"
KEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$KEY" ]] || fail "no install key"
cp "$MF_WEBSITE_OUT/certs/ca.pem" "$WORK/agent-roots.pem"
cat >"$WORK/agent.toml" <<EOF
upstream_url = "https://$GATEWAY:$GRPC_PORT"
enroll_url = "https://$GATEWAY:$ENROLL_PORT"
data_dir = "/agent/data"
enrollment_token = "$KEY"
heartbeat_interval = 10
EOF
docker rm -f "$AGENT" >/dev/null 2>&1 || true
# The agent of this checkout, run by the host's own loader and libraries
# (the host's glibc may be newer than any image's), trusting the test
# authority as its machine would.
LOADER=$(readlink -f "$(ldd "$AGENT_BIN" | awk '/ld-linux/ {print $1}')")
LIBC=$(readlink -f "$(ldd "$AGENT_BIN" | awk '/libc\.so/ {print $3}')")
docker run -d --name "$AGENT" --hostname web-agent --network "$NET" --user "$(id -u):$(id -g)" \
    -e SSL_CERT_FILE=/agent/roots.pem \
    -v "$AGENT_BIN:/agent/monitorflow-agent:ro" -v "$WORK/agent.toml:/agent/agent.toml:ro" \
    -v "$WORK/agent-roots.pem:/agent/roots.pem:ro" -v "$WORK/agent:/agent/data" \
    -v "$(dirname "$LOADER"):/hostld:ro" -v "$(dirname "$LIBC"):/hostlib:ro" \
    debian:12 "/hostld/$(basename "$LOADER")" --library-path /hostlib /agent/monitorflow-agent --config /agent/agent.toml >/dev/null
SERVER=""
for _ in $(seq 60); do
    SERVER=$(get "/api/servers?limit=50" | jq -r '.servers[] | select(.hostname == "web-agent" and .status == "online") | .id' | head -1)
    [[ -n "$SERVER" ]] && break
    sleep 2
done
[[ -n "$SERVER" ]] || fail "the agent did not come online"
ok "web-agent online ($SERVER)"

step "a relay of the site, a probe (ADR-0148)"
RKEY=$(send POST /api/install-keys "$(jq -n --arg site "$SITE" '{site_id: $site, role: "relay"}')" | expect 201 | jq -r '.. | strings | select(startswith("mfe1"))' | head -1)
[[ -n "$RKEY" ]] || fail "no relay install key"
mkdir -p "$WORK/relay"
cat >"$WORK/relay.toml" <<TOML
upstream_url = "https://$GATEWAY:$GRPC_PORT"
enroll_url = "https://$GATEWAY:$ENROLL_PORT"
data_dir = "/relay/data"
enrollment_token = "$RKEY"
heartbeat_interval = 10
TOML
docker rm -f "$RELAY" >/dev/null 2>&1 || true
docker run -d --name "$RELAY" --hostname web-relay --network "$NET" --user "$(id -u):$(id -g)" \
    -e SSL_CERT_FILE=/relay/roots.pem \
    -v "$RELAY_BIN:/relay/monitorflow-relay:ro" -v "$WORK/relay.toml:/relay/relay.toml:ro" \
    -v "$WORK/agent-roots.pem:/relay/roots.pem:ro" -v "$WORK/relay:/relay/data" \
    -v "$(dirname "$LOADER"):/hostld:ro" -v "$(dirname "$LIBC"):/hostlib:ro" \
    debian:12 "/hostld/$(basename "$LOADER")" --library-path /hostlib /relay/monitorflow-relay --config /relay/relay.toml >/dev/null
RELAY_ID=""
for _ in $(seq 60); do
    RELAY_ID=$(get /api/relays | jq -r '.relays[] | select(.peer_id == "web-relay" and .status == "online") | .id' | head -1)
    [[ -n "$RELAY_ID" ]] && break
    sleep 2
done
[[ -n "$RELAY_ID" ]] || fail "the relay did not come online"
ok "web-relay online ($RELAY_ID)"

step "website checks"
# check NAME URL [PARAMS]: a website check of the server, every 10 s.
check() {
    local params=${3:-}
    params=$(jq -c --arg u "$2" '. + {url: $u}' <<<"${params:-"{}"}")
    send POST "/api/servers/$SERVER/sensor_configs" "$(jq -n --arg n "$1" --argjson p "$params" \
        '{kind: "http", name: $n, interval_ms: 10000, timeout_ms: 4000, params: $p}')" | expect 201 >/dev/null
}
# alert METRIC COMPARISON VALUE: a critical alert at once.
alert() {
    send POST "/api/servers/$SERVER/thresholds" "$(jq -n --arg m "monitorflow_$1" --arg c "$2" --argjson v "$3" \
        '{metric: $m, instance: null, comparison: $c, warning: null, critical: $v, duration_s: 0, hysteresis: 0, enabled: true}')" \
        | expect 201 >/dev/null
}
check http_valid https://web.test:8443/ '{"contains": "Welcome"}'
check http_expired https://web.test:8444/
check http_wrong https://web.test:8445/
check http_soon https://web.test:8446/
check http_self https://web.test:8447/
check http_missing https://web.test:8443/error '{"contains": "Welcome"}'
check http_refused http://web.test/error '{"not_contains": "Error"}'
check http_unavailable http://web.test/unavailable
check http_expected http://web.test/unavailable '{"status": "503"}'
check http_moved http://web.test/moved
check http_kept http://web.test/moved '{"follow_redirects": "false"}'
check http_loop http://web.test/loop
check http_slow http://web.test/slow
check http_head http://web.test/ '{"method": "HEAD"}'
check http_ipv4 http://web.test/ '{"ip": "4"}'
check http_private https://web.test:8443/private '{"username": "monitor", "password": "s3cret-page"}'
check http_wrong_password https://web.test:8443/private '{"username": "monitor", "password": "not-it"}'
check http_withheld https://web.test:8447/private '{"username": "monitor", "password": "s3cret-page"}'
check http_toggle http://web.test/toggle
alert http_toggle_up "<" 1
# Run by the central server, which reaches the sites at 127.0.0.1. The
# rows are marked with the probe column of ADR-0148 directly until the
# API sets it.
check http_central https://127.0.0.1:18443/ '{"contains": "Welcome"}'
check http_central_expired https://127.0.0.1:18444/
check http_central_toggle http://127.0.0.1:18080/toggle
alert http_central_toggle_up "<" 1
# Run by the relay, which reaches the sites as web.test like the agent;
# the central server does not know that name.
check http_relay https://web.test:8443/ '{"contains": "Welcome"}'
check http_relay_wrong https://web.test:8445/
check http_relay_toggle http://web.test/toggle
alert http_relay_toggle_up "<" 1
check http_central_name https://web.test:8443/
psql "$ADMIN_URL/$DB_NAME" -qc "UPDATE sensor_configs SET probe = 'central' WHERE name LIKE 'http_central%'" \
    -c "UPDATE sensor_configs SET probe = 'relay', probe_relay_id = '$RELAY_ID' WHERE name LIKE 'http_relay%'" >/dev/null
# The password is sealed: not in the row, never in an answer.
psql "$ADMIN_URL/$DB_NAME" -Atc "SELECT secret FROM sensor_configs WHERE name = 'http_private'" | grep -q '^mfs1:' \
    || fail "the password is not sealed"
get "/api/servers/$SERVER/sensor_configs" | grep -q s3cret && fail "a password came back from the API"
ok "checks added; passwords sealed and never returned"

# Waits until every check ran at least once.
CHECKS=""
for _ in $(seq 30); do
    CHECKS=$(get "/api/servers/$SERVER/website-checks")
    [[ $(jq '[.checks[] | select(.last_at == null)] | length' <<<"$CHECKS") == 0 ]] && break
    sleep 3
done
echo "$CHECKS" >"$WORK/website-checks.json"
[[ $(jq '[.checks[] | select(.last_at == null)] | length' <<<"$CHECKS") == 0 ]] \
    || { jq -c '.checks[] | select(.last_at == null) | .name' <<<"$CHECKS"; fail "checks without a run"; }
# is NAME JQ-CONDITION WHAT
is() {
    jq -e --arg n "$1" ".checks[] | select(.name == \$n) | $2" <<<"$CHECKS" >/dev/null \
        || { jq -c --arg n "$1" '.checks[] | select(.name == $n)' <<<"$CHECKS"; fail "$1: $3"; }
    ok "$1: $3"
}
is http_valid '.up and .status_code == 200 and .content_ok and .certificate.ok and .certificate.days_left > 88 and .certificate.issuer == "MonitorFlow website test CA" and .run_by == "agent"' "answers, text found, certificate valid for 89 days"
is http_expired '.up and .certificate.ok == false and .certificate.reason == "expired" and .certificate.days_left < -1' "page read, certificate expired"
is http_wrong '.certificate.reason == "name" and .certificate.name_ok == false' "certificate for another name"
is http_soon '.certificate.ok and .certificate.days_left > 4 and .certificate.days_left <= 5' "certificate valid, 4 to 5 days left"
is http_self '.certificate.reason == "self_signed"' "self-signed certificate"
is http_missing '.up == false and .reason == "content" and .content_ok == false' "text missing"
is http_refused '.up == false and .reason == "content"' "refused text present"
is http_unavailable '.up == false and .reason == "status" and .status_code == 503' "503 is a failure"
is http_expected '.up and .status_code == 503' "503 expected is fine"
is http_moved '.up and .redirects == 1 and .final_url == "http://web.test/"' "redirect followed to the final URL"
is http_kept '.up and .status_code == 302' "redirect not followed is the answer"
is http_loop '.up == false and .reason == "redirects"' "redirect loop"
is http_slow '.up == false and .reason == "timeout"' "slow page times out"
is http_head '.up and .status_code == 200 and .content_ok == null' "HEAD"
is http_ipv4 '.up' "IPv4 only"
is http_private '.up and .status_code == 200' "basic authentication"
is http_wrong_password '.up == false and .status_code == 401' "wrong password refused by the site"
is http_withheld '.up == false and .reason == "certificate" and .status_code == null' "password withheld from a self-signed site"
is http_central '.up and .run_by == "central" and .certificate.ok' "run by the central server"
is http_central_expired '.run_by == "central" and .certificate.reason == "expired"' "central server sees the expired certificate"
is http_relay '.up and .run_by == "relay" and .relay == "web-relay" and .certificate.ok' "run by the relay"
is http_relay_wrong '.run_by == "relay" and .certificate.reason == "name"' "relay sees the wrong name"
is http_central_name '.run_by == "central" and .up == false and .reason == "dns"' "the central server does not know web.test, the relay does"
# The response time is a metric of its own, in VictoriaMetrics.
curl -sf "http://127.0.0.1:18428/api/v1/query" --data-urlencode 'query=monitorflow_http_valid_response_seconds' \
    | jq -e '.data.result[0].value[1] | tonumber > 0' >/dev/null || fail "no response time in VictoriaMetrics"
ok "response time stored"

step "alerts open and resolve"
# incident METRIC STATE: waits for an incident of METRIC in STATE.
incident() {
    for _ in $(seq 40); do
        get "/api/incidents?limit=100" | jq -e --arg m "monitorflow_$1" --arg s "$2" \
            '.incidents[] | select(.metric == $m and .state == $s)' >/dev/null && return 0
        sleep 3
    done
    get "/api/incidents?limit=100" | jq -c '.incidents[] | {metric, state}' >&2
    fail "no $2 incident on $1"
}
incident http_toggle_up open
incident http_central_toggle_up open
incident http_relay_toggle_up open
ok "the toggled page opened alerts, from the agent, the central server and the relay"
echo "<p>back</p>" >"$P/toggle.html"
chmod a+r "$P/toggle.html"
incident http_toggle_up resolved
incident http_central_toggle_up resolved
incident http_relay_toggle_up resolved
ok "the alerts resolved once the page answered"

step "the server page in Chromium"
CHROMIUM=${MF_TEST_CHROMIUM:-$(command -v chromium || command -v google-chrome || command -v chromium-browser || true)}
if [[ -z "$CHROMIUM" ]]; then
    echo "skipped: no Chromium (set MF_TEST_CHROMIUM)"
else
    if [[ "${MF_TEST_SKIP_WEB_BUILD:-}" != 1 || ! -f "$MF_WEB_OUT/index.html" ]]; then
        (cd "$REPO/web" && npm ci --no-audit --no-fund --silent && npm run --silent build >/dev/null)
    fi
    compose up -d --quiet-pull ui >/dev/null
    for _ in $(seq 30); do curl -sf "http://127.0.0.1:$MF_UI_PORT/login/" >/dev/null && break; sleep 1; done
    npm install --silent --no-audit --no-fund --prefix "$WORK/pw" playwright-core@1.64.1 >/dev/null
    MF_WEBSITE_PW=$WORK/pw MF_WEBSITE_WEB=http://localhost:$MF_UI_PORT MF_WEBSITE_SERVER=$SERVER \
        MF_WEBSITE_PASSWORD=$PASSWORD MF_WEBSITE_CHROMIUM=$CHROMIUM node "$HERE/browser.mjs"
    ok "screenshots in $MF_WEBSITE_OUT"
fi

echo
echo "All passed."
