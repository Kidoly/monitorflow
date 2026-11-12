#!/usr/bin/env bash
# Starts deploy/docker-compose.yml under its own project name and ports,
# checks that each part answers and that the server's database link uses TLS
# (ADR-0051), what port 80 serves (ADR-0068) and that package mirrors work
# (ADR-0067), then removes everything it created. Needs docker with compose, curl, jq and openssl. CI
# runs it on every pull request.
set -euo pipefail
cd "$(dirname "$0")"

# An IP address as DOMAIN: clients send no SNI to it, the harder case.
export DOMAIN=127.0.0.1 SERVER_NAMES=localhost WEB_CERTIFICATE=internal
POSTGRES_PASSWORD=$(openssl rand -hex 16)
POSTGRES_APP_PASSWORD=$(openssl rand -hex 16)
export POSTGRES_PASSWORD POSTGRES_APP_PASSWORD
# shellcheck source=deploy/db-role-test.sh
. ./db-role-test.sh
export HTTPS_PORT=18443 HTTP_PORT=18080 GRPC_PORT=17443 ENROLL_PORT=17445 MIRROR_PORT=17480

# --env-file /dev/null: a real deploy/.env must not leak into the test.
compose() { docker compose -p monitorflow-smoke --env-file /dev/null -f docker-compose.yml "$@"; }
work=$(mktemp -d)
fixture=monitorflow-smoke-fixture
trap 'docker rm -f "$fixture" >/dev/null 2>&1 || true; compose down -v --remove-orphans >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

if ! compose up -d --build --wait --wait-timeout 600; then
    compose ps -a
    compose logs --tail 50 server web postgres mirror
    exit 1
fi

failed=0
check() {
    printf '%-45s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}

# The certificate a TLS listener presents names localhost. No client
# certificate is sent: the gRPC listener still shows its own first, then
# ends the handshake with an alert. -prexit prints the session on exit
# too, as s_client may read that alert before printing it otherwise. That
# race still loses now and then on CI runners, so three tries; the last
# output is kept for the log.
cert_names_localhost() {
    for _ in 1 2 3; do
        openssl s_client -connect "localhost:$1" -alpn h2 -prexit </dev/null >"$work/s_client.$1" 2>&1 || true
        openssl x509 -noout -ext subjectAltName <"$work/s_client.$1" 2>/dev/null | grep -q 'DNS:localhost' && return 0
        sleep 1
    done
    return 1
}
# Every connection of the server's pool, seen by PostgreSQL, is encrypted.
db_link_uses_tls() {
    [ "$(compose exec -T postgres psql -U monitorflow -d monitorflow -tAc \
        "SELECT count(*) > 0 AND bool_and(s.ssl) FROM pg_stat_ssl s JOIN pg_stat_activity a USING (pid)
         WHERE a.backend_type = 'client backend' AND a.pid <> pg_backend_pid()")" = t ]
}
# The server's database role (ADR-0109).
psql() { compose exec -T postgres psql -U monitorflow -d monitorflow -tAc "$1"; }
# As that role, through the container's socket: refused by its rights.
# psql fails on the statement: keep its output, not its status.
app_refused() {
    local out
    out=$(compose exec -T postgres psql -U monitorflow_app -d monitorflow -c "$1" 2>&1) && return 1
    grep -q "$2" <<<"$out"
}
# No port of the service is bound on the host.
not_published() {
    local ports
    ports=$(docker inspect -f '{{json .NetworkSettings.Ports}}' "$(compose ps -q "$1")")
    ! grep -q HostPort <<<"$ports"
}

# Every container of the stack drops all capabilities and setuid gains,
# and only the root ones add capabilities back (ADR-0110).
hardened() {
    local id
    for id in $(compose ps -aq); do
        [ "$(docker inspect -f '{{json .HostConfig.CapDrop}} {{json .HostConfig.SecurityOpt}}' "$id")" \
            = '["ALL"] ["no-new-privileges:true"]' ] || return 1
    done
}
# The processes of the service run as uid $2, with no effective capability.
runs_as() {
    local status
    status=$(compose exec -T "$1" sh -c 'cat /proc/1/status') || return 1
    grep -qE "^Uid:[[:space:]]+$2[[:space:]]" <<<"$status" && grep -qE '^CapEff:[[:space:]]+0+$' <<<"$status"
}
check "every container is hardened" hardened
check "Caddy runs as its own user, no capability" runs_as web 10002
check "the server too" runs_as server 10001
check "web's image is read-only" compose exec -T web sh -c '! touch /srv/monitorflow/web/x'

check "API ready behind Caddy" curl -fsk "https://127.0.0.1:$HTTPS_PORT/api/health/ready"
check "web interface served" curl -fsk -o /dev/null "https://127.0.0.1:$HTTPS_PORT/"

# Port 80 sends $1 to HTTPS with its content unseen.
plain=http://127.0.0.1:$HTTP_PORT
redirected() {
    case $(curl --path-as-is -s -o "$work/plain" -X "${2:-GET}" -w '%{http_code} %{redirect_url}' "$plain$1") in
        "308 https://127.0.0.1:$HTTPS_PORT/"*) ! grep -q . "$work/plain" ;;
        *) return 1 ;;
    esac
}
for _ in $(seq 30); do curl -fs -o /dev/null "$plain/trust/" && break; sleep 1; done
curl -fs -o "$work/ca.crt" "$plain/trust/monitorflow-ca.crt" || true
fingerprint=$(openssl x509 -in "$work/ca.crt" -noout -fingerprint -sha256 | cut -d = -f 2) || true
# The headers of the page and of the certificate.
trust_header() { curl -fsI "$plain$1" | grep -qi "^$2"; }
# A response of the HTTPS site that has no HSTS: it would close port 80.
no_hsts() { ! curl -fsIk "https://127.0.0.1:$HTTPS_PORT/" | grep -qi '^strict-transport-security'; }

check "trust page shows the CA's fingerprint" grep -qF "$fingerprint" <(curl -fs "$plain/trust/")
check "trust page has a Content-Security-Policy" trust_header /trust/ content-security-policy
check "the CA certificate installs in browsers" trust_header /trust/monitorflow-ca.crt 'content-type: application/x-x509-ca-cert'
check "the page's certificate verifies HTTPS" curl -fs --cacert "$work/ca.crt" "https://127.0.0.1:$HTTPS_PORT/api/health/ready"
check "and so does the page, over HTTPS" curl -fs --cacert "$work/ca.crt" "https://127.0.0.1:$HTTPS_PORT/trust/"
check "no HSTS with the internal CA" no_hsts
for path in / /login/ /setup/ /_next/x.js /api/health/ready /api/setup /api/auth/me /agent/latest/install.sh \
    /trust/x /trust/../api/health/ready /ca-fingerprint /run/ca-fingerprint /run/public/trust/index.html; do
    check "port 80: $path goes to HTTPS" redirected "$path"
done
check "port 80: POST /api/auth/login too" redirected /api/auth/login POST
check "enrollment listener: certificate" cert_names_localhost "$ENROLL_PORT"
check "gRPC listener: certificate" cert_names_localhost "$GRPC_PORT"
check "database connections use TLS" db_link_uses_tls
check "the server runs on its own database role" test "$(psql "$SERVER_RUNS_AS_APP")" = t
check "which owns nothing and cannot change the audit log" test "$(psql "$APP_ROLE_RESTRICTED")" = t
check "it cannot switch off the audit log's triggers" app_refused 'ALTER TABLE audit_log DISABLE TRIGGER USER' 'must be owner of table audit_log'
check "nor delete from it" app_refused 'DELETE FROM audit_log' 'permission denied for table audit_log'
check "nor run a program in the database container" app_refused "COPY audit_log (actor_id) FROM PROGRAM 'id'" 'permission denied'
check "the migration step ran and ended" test "$(docker inspect --format '{{.State.Status}} {{.State.ExitCode}}' "$(compose ps -aq db-migrate)")" = "exited 0"
check "PostgreSQL not published" not_published postgres
check "VictoriaMetrics not published" not_published victoriametrics
check "VictoriaLogs not published" not_published victorialogs

# The links to the stores (ADR-0087), seen from the server's container:
# TLS with a certificate of the server's CA that the stores read again
# when it is renewed, and a password.
ca=/var/lib/monitorflow/server/tls/ca.pem
in_server() { compose exec -T server bash -c "$1"; }
# The first line of the answer of $1:$2 to a request without a password.
plain_answer() { in_server "exec 3<>/dev/tcp/$1/$2 && printf 'GET /api/v1/query?query=1 HTTP/1.0\r\n\r\n' >&3 && head -n1 <&3"; }
tls_answer() {
    in_server "printf 'GET /api/v1/query?query=1 HTTP/1.0\r\nHost: $1\r\n\r\n' \
        | openssl s_client -quiet -verify_return_error -CAfile $ca -servername $1 -connect $1:$2 2>/dev/null | head -n1"
}
# The serial of the certificate $1:$2 shows, verified against the CA.
serial_of() {
    in_server "openssl s_client -verify_return_error -CAfile $ca -servername $1 -connect $1:$2 </dev/null 2>/dev/null \
        | openssl x509 -noout -serial"
}
# The output of the rest of the arguments holds $1.
answers() { local want=$1; shift; "$@" | grep -qF "$want"; }
check "the server reaches both stores over TLS" in_server "monitorflow-server storage-check"
for store in victoriametrics:8428 victorialogs:9428; do
    host=${store%:*} port=${store#*:}
    check "$host: certificate from the server's CA" serial_of "$host" "$port"
    check "$host: plain HTTP is refused" answers ' 400 ' plain_answer "$host" "$port"
    check "$host: no password, no answer" answers ' 401 ' tls_answer "$host" "$port"
done
renewed() {
    local before=$1 after
    for _ in $(seq 10); do
        after=$(serial_of victoriametrics 8428) && [ "$after" != "$before" ] && return 0
        sleep 1
    done
    return 1
}
before=$(serial_of victoriametrics 8428 || true)
compose exec -T server rm /var/lib/monitorflow/storage-tls/cert.pem
compose run --rm -T storage-certs >/dev/null
check "a renewed certificate is served without a restart" renewed "$before"
check "and the server still reaches both stores" in_server "monitorflow-server storage-check"

# Package mirrors (ADR-0067). The first check is the catalog as the
# interface shows it, with the real tools. No distribution fits a CI disk:
# what syncs here is a small signed upstream, in its own container on the
# mirror tools' network, and real apt and dnf clients install from the
# result.
api=https://127.0.0.1:$HTTPS_PORT/api
password=$(openssl rand -hex 16)
compose exec -T server monitorflow-server user add --username admin --role admin <<<"$password" >/dev/null
curl -fsSk -c "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg p "$password" '{username: "admin", password: $p}')" "$api/auth/login" >/dev/null
get() { curl -fsSk -b "$work/cookies" "$api/$1"; }
post() { curl -sSk -b "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' -o "$work/answer" -w '%{http_code}' -d "$2" "$api/$1"; }
get mirrors/catalog >"$work/catalog" || true
check "mirror tools answer" jq -e '.tools.available' "$work/catalog"
check "the catalog lists distributions with a size" jq -e '.distributions | length > 0 and all(.size_bytes > 0)' "$work/catalog"
check "the disk's free space is known" jq -e '.disk.free_bytes > 0' "$work/catalog"
# Only where it does not fit: elsewhere this would start a real sync.
if jq -e '.disk.free_bytes < (.distributions[] | select(.id == "debian-13").size_bytes)' "$work/catalog" >/dev/null; then
    code=$(post mirrors '{"distribution": "debian-13"}')
    check "a distribution too big for the disk is refused" test "$code" = 409
    check "and the answer says so" jq -e '.error | test("needs about [0-9]+ GiB and this server has [0-9]+ GiB free")' "$work/answer"
fi
not_mounted() {
    ! docker inspect -f '{{json .Mounts}}' "$(compose ps -q mirror)" | grep -q serverdata
}
check "the mirror tools do not see the server's keys" not_mounted

docker build -q -t monitorflow-mirror-fixture mirror-fixture >/dev/null
docker run -d --name "$fixture" --network monitorflow-smoke_mirror --network-alias fixture \
    monitorflow-mirror-fixture >/dev/null
fixture_up() { docker exec "$fixture" test -s /srv/upstream-key.gpg; }
until_true() {
    local deadline=$((SECONDS + $1))
    shift
    until "$@"; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 2
    done
}
check "the fixture upstream starts" until_true 60 fixture_up
docker exec "$fixture" cat /srv/upstream-key.gpg | compose exec -T mirror gpg --batch --quiet \
    --no-default-keyring --keyring /var/lib/monitorflow/server/mirrors/upstream-keyring.gpg --import
# Manual: the scheduler syncs a new mirror at its next poll, and the sync
# below is then refused as busy if it starts in that moment.
add() { compose exec -T server monitorflow-server mirror add --interval manual "$@" >/dev/null; }
add --name fixture-apt --family apt --url http://fixture:8000/apt --distribution fixture \
    --component main --component empty --arch amd64
# With an architecture: the noarch package must still come.
add --name fixture-dnf --family dnf --url http://fixture:8000/dnf --arch x86_64
compose exec -T server monitorflow-server mirror key generate >/dev/null
run_ended() { # mirror-id mirror-name
    get "mirrors/$1" >"$work/mirror-$2" && jq -e '.last_run.state != "running"' "$work/mirror-$2" >/dev/null
}
# Starts a sync of $1 through the API and waits for its end.
synced() {
    local id code
    id=$(get mirrors | jq -r --arg n "$1" '.mirrors[] | select(.name == $n) | .id')
    code=$(post "mirrors/$id/sync" '')
    [ "$code" = 202 ] || { echo "$1: sync refused ($code): $(cat "$work/answer")" >>"$work/refused"; return 1; }
    until_true 180 run_ended "$id" "$1" || return 1
    jq -e '.last_run.state == "succeeded" and .current_snapshot != null' "$work/mirror-$1" >/dev/null
}
check "an apt mirror syncs and publishes" synced fixture-apt
check "a dnf mirror syncs and publishes" synced fixture-dnf
check "a second sync publishes again" synced fixture-dnf

mirror=http://127.0.0.1:$MIRROR_PORT
apt_installs() {
    docker run --rm --network host debian:trixie-slim sh -ec "
        apt-get update -qq -o APT::Update::Error-Mode=any >/dev/null 2>&1 || true
        apt-get install -y -qq --no-install-recommends curl >/dev/null
        curl -fsS $mirror/keys/monitorflow-archive.asc >/etc/apt/mirror.asc
        rm -f /etc/apt/sources.list /etc/apt/sources.list.d/*
        echo 'deb [signed-by=/etc/apt/mirror.asc] $mirror/apt/fixture-apt/current fixture main' >/etc/apt/sources.list
        apt-get update -o APT::Update::Error-Mode=any
        apt-get install -y mf-fixture
        test -x /usr/bin/mf-fixture"
}
dnf_installs() {
    docker run --rm --network host rockylinux:9 sh -ec "
        printf '[f]\nname=f\nbaseurl=$mirror/dnf/fixture-dnf/current/\ngpgcheck=0\nrepo_gpgcheck=1\ngpgkey=$mirror/keys/monitorflow-archive.asc\nenabled=1\n' >/etc/yum.repos.d/f.repo
        dnf install -y --disablerepo='*' --enablerepo=f mf-fixture
        test -x /usr/bin/mf-fixture"
}
keeps_groups() { curl -fsS "$mirror/dnf/fixture-dnf/current/repodata/repomd.xml" | grep -q 'type="group"'; }
check "a dnf mirror keeps upstream groups" keeps_groups
check "apt verifies and installs from the mirror" apt_installs
check "dnf verifies and installs from the mirror" dnf_installs

# Retention (ADR-0074), with the real tools: upstream replaces the package,
# and once the snapshots holding the old one are past the seven kept, a
# pass deletes it for good while clients still install from `current`.
mirrors=/var/lib/monitorflow/server/mirrors
in_mirror() { compose exec -T mirror sh -c "$1"; }
old_packages() { in_mirror "find $mirrors/aptly/pool $mirrors/dnf -name '*mf-fixture?1.0*' | wc -l"; }
disk_kib() { in_mirror "du -sk $mirrors/aptly $mirrors/dnf | awk '{ total += \$1 } END { print total }'"; }
snapshot_count() { get "mirrors/$(get mirrors | jq -r --arg n "$1" '.mirrors[] | select(.name == $n) | .id')" | jq '[.snapshots[] | select(.state == "published")] | length'; }
docker exec "$fixture" sh -c 'rm -rf /srv/apt /srv/dnf && mv /srv/next/apt /srv/next/dnf /srv/'
for _ in 1 2 3 4 5 6 7 8; do
    check "a sync publishes the upgraded apt upstream" synced fixture-apt
    check "a sync publishes the upgraded dnf upstream" synced fixture-dnf
done
check "old snapshots still hold the old packages" test "$(old_packages)" -gt 0
disk_before=$(disk_kib)
check "retention removes old snapshots" compose exec -T server monitorflow-server mirror prune
disk_after=$(disk_kib)
check "their packages are gone from the disk" test "$(old_packages)" = 0
check "and the disk use went down ($disk_before to $disk_after KiB)" test "$disk_after" -lt "$disk_before"
check "seven apt snapshots stay" test "$(snapshot_count fixture-apt)" = 7
check "seven dnf snapshots stay" test "$(snapshot_count fixture-dnf)" = 7
check "apt still installs from the mirror" apt_installs
check "dnf still installs from the mirror" dnf_installs

# Deleting a mirror (ADR-0075), with the real tools.
in_aptly() { compose exec -T mirror aptly "$@" -config="$mirrors/aptly.conf" -raw; }
index_says_removed() { curl -fsS "$mirror/index.json" | jq -e --arg n "$1" 'any(.removed[]; .name == $n)' >/dev/null; }
mirror_gone() { [ "$(curl -sSk -b "$work/cookies" -o /dev/null -w '%{http_code}' "$api/mirrors/$1")" = 404 ]; }
deleted() {
    local id code
    id=$(get mirrors | jq -r --arg n "$1" '.mirrors[] | select(.name == $n) | .id')
    code=$(curl -sSk -b "$work/cookies" -H 'x-monitorflow-csrf: 1' -X DELETE -o "$work/answer" -w '%{http_code}' "$api/mirrors/$id")
    [ "$code" = 202 ] || { echo "delete refused: $(cat "$work/answer")" >&2; return 1; }
    until_true 120 mirror_gone "$id"
}
disk_before=$(disk_kib)
check "an apt mirror is deleted" deleted fixture-apt
check "a dnf mirror is deleted" deleted fixture-dnf
disk_after=$(disk_kib)
check "their disk space came back ($disk_before to $disk_after KiB)" test "$disk_after" -lt "$disk_before"
check "no package is left" test "$(in_mirror "find $mirrors/aptly/pool $mirrors/dnf $mirrors/dnf-work -name 'mf-fixture?[0-9]*' | wc -l")" = 0
check "aptly holds nothing of them" test -z "$(in_aptly mirror list; in_aptly snapshot list; in_aptly publish list)"
check "nothing is served any more" test "$(curl -s -o /dev/null -w '%{http_code}' "$mirror/apt/fixture-apt/current/dists/fixture/Release")" = 404
check "the index tells relays" index_says_removed fixture-dnf

if [ "$failed" -ne 0 ]; then
    for out in "$work"/s_client.*; do
        [ -e "$out" ] && { echo "== $out"; head -40 "$out"; }
    done
    compose logs --tail 50 server web postgres mirror
    [ -e "$work/refused" ] && cat "$work/refused"
    for out in "$work"/mirror-*; do
        [ -e "$out" ] && { echo "== $out"; jq -r '.runs[0].output // .' "$out" | tail -40; }
    done
    exit 1
fi
echo "smoke test passed"
