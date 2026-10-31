#!/usr/bin/env bash
# Starts deploy/docker-compose.yml under its own project name and ports,
# checks that each part answers and that the server's database link uses TLS
# (ADR-0051) and that package mirrors work (ADR-0067), then removes
# everything it created. Needs docker with compose, curl, jq and openssl. CI
# runs it on every pull request.
set -euo pipefail
cd "$(dirname "$0")"

# An IP address as DOMAIN: clients send no SNI to it, the harder case.
export DOMAIN=127.0.0.1 SERVER_NAMES=localhost
POSTGRES_PASSWORD=$(openssl rand -hex 16)
export POSTGRES_PASSWORD
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
# No port of the service is bound on the host.
not_published() {
    local ports
    ports=$(docker inspect -f '{{json .NetworkSettings.Ports}}' "$(compose ps -q "$1")")
    ! grep -q HostPort <<<"$ports"
}

check "API ready behind Caddy" curl -fsk "https://127.0.0.1:$HTTPS_PORT/api/health/ready"
check "web interface served" curl -fsk -o /dev/null "https://127.0.0.1:$HTTPS_PORT/"
check "enrollment listener: certificate" cert_names_localhost "$ENROLL_PORT"
check "gRPC listener: certificate" cert_names_localhost "$GRPC_PORT"
check "database connections use TLS" db_link_uses_tls
check "PostgreSQL not published" not_published postgres
check "VictoriaMetrics not published" not_published victoriametrics
check "VictoriaLogs not published" not_published victorialogs

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
add() { compose exec -T server monitorflow-server mirror add "$@" >/dev/null; }
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
    [ "$code" = 202 ] || { echo "sync refused: $(cat "$work/answer")" >&2; return 1; }
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

if [ "$failed" -ne 0 ]; then
    for out in "$work"/s_client.*; do
        [ -e "$out" ] && { echo "== $out"; head -40 "$out"; }
    done
    compose logs --tail 50 server web postgres mirror
    for out in "$work"/mirror-*; do
        [ -e "$out" ] && { echo "== $out"; jq -r '.runs[0].output // .' "$out" | tail -40; }
    done
    exit 1
fi
echo "smoke test passed"
