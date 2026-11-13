#!/usr/bin/env bash
# End-to-end test of adding a central server (ADR-0100): two fresh hosts,
# systemd containers without Docker as for deploy/install-server-test.sh,
# side by side on Docker's default network. The first is installed with the
# one-liner; --add-server is refused without a license, and works once
# one is installed; the second joins with --join and the file the first
# wrote. Then: one database for both, a third central server refused over
# the license's count, an added server that restarts after expiry, and the
# second leading when the first's server stops.
#
# Each host reaches a local registry and a local HTTPS directory of release
# files as localhost, in its own network namespace; both hosts' registries
# share one storage volume. The images must trust MF_TEST_SIGNING_KEY,
# as for deploy/update-test.sh, and MF_TEST_LICENSE_KEY, a throwaway
# license signing key whose public key was in ee/license-keys.pem when
# monitorflow-{server,web,updater,mirror}:e2e-base were built. Needs docker,
# curl, jq and openssl, and Internet access from the test hosts.
#   MF_TEST_SIGNING_KEY=key.pem MF_TEST_LICENSE_KEY=license.pem deploy/join-test.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
key=$(realpath "${MF_TEST_SIGNING_KEY:?MF_TEST_SIGNING_KEY must name the release signing key the images trust}")
license_key=$(realpath "${MF_TEST_LICENSE_KEY:?MF_TEST_LICENSE_KEY must name the license signing key the images trust}")
BASE=$(awk -F '"' '/^\[workspace.package\]/ { ws = 1 } ws && /^version = / { print $2; exit }' "$repo/Cargo.toml")
host=monitorflow-join
PORT=${MF_TEST_REGISTRY_PORT:-5557}
REGISTRY=localhost:$PORT
files=https://localhost:8444
work=$(mktemp -d)

cleanup() {
    for h in "$host-1" "$host-2"; do
        docker rm -f "$h-registry" "$h-files" >/dev/null 2>&1 || true
        docker rm -fv "$h" >/dev/null 2>&1 || true
    done
    docker volume rm "$host-registry" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

failed=0
check() {
    printf '%-60s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}

docker build -q -t "$host" - >/dev/null <<'EOF'
FROM debian:trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
    systemd systemd-sysv dbus ca-certificates curl openssl iproute2 procps
# --privileged hands this container the host's /dev, its virtual consoles
# included: a getty on tty1 there would hang up the desktop session of
# whoever runs the test. Nothing here needs a console login.
RUN systemctl mask getty.target getty@.service autovt@.service serial-getty@.service \
    console-getty.service container-getty@.service systemd-vconsole-setup.service
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
EOF

# HTTPS for localhost with a CA of its own, which both hosts trust.
mkdir -p "$work/tls" "$work/serve"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=localhost \
    -addext subjectAltName=DNS:localhost -keyout "$work/tls/key.pem" -out "$work/tls/cert.pem" 2>/dev/null
cat >"$work/tls/serve.py" <<'EOF'
import functools, http.server, ssl
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory="/srv")
server = http.server.ThreadingHTTPServer(("127.0.0.1", 8444), handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/tls/cert.pem", "/tls/key.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
EOF

# A fresh host named $1, with its own registry and release directory.
start_host() {
    local h=$host-$1 publish=()
    [ "$1" != 1 ] || publish=(-p "$PORT:$PORT")
    docker run -d --name "$h" --hostname "fresh$1" --privileged --cgroupns=private \
        --tmpfs /run --tmpfs /run/lock -v /var/lib/docker -v /var/lib/containerd \
        "${publish[@]}" "$host" >/dev/null
    for _ in $(seq 120); do docker exec "$h" systemctl is-active -q basic.target 2>/dev/null && break; sleep 1; done
    docker exec "$h" systemctl is-active -q basic.target || { echo "$h: systemd did not start" >&2; exit 1; }
    docker cp -q "$work/tls/cert.pem" "$h:/usr/local/share/ca-certificates/e2e.crt"
    docker exec "$h" update-ca-certificates >/dev/null
    # The public name reaches each host's own web interface from inside it.
    docker exec "$h" sh -c 'echo "127.0.0.1 monitor.internal" >>/etc/hosts'
    docker run -d --name "$h-registry" --network "container:$h" -e "REGISTRY_HTTP_ADDR=:$PORT" \
        -v "$host-registry:/var/lib/registry" registry:2.8.3 >/dev/null
    docker run -d --name "$h-files" --network "container:$h" -v "$work/serve:/srv:ro" -v "$work/tls:/tls:ro" \
        python:3.13-alpine python3 /tls/serve.py >/dev/null
}
start_host 1
start_host 2
on() { local h=$host-$1; shift; docker exec -i "$h" "$@"; }
for _ in $(seq 30); do curl -fs "http://$REGISTRY/v2/" >/dev/null && break; sleep 1; done

push() {
    docker tag "$1" "$REGISTRY/$2:$3"
    docker push -q "$REGISTRY/$2:$3" >/dev/null
    docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$REGISTRY/$2:$3" \
        | grep -m1 "^$REGISTRY/$2@sha256:"
}
keys=$work/keys.pem
{ cat "$repo/deploy/release-keys.pem"; openssl pkey -in "$key" -pubout; } >"$keys"
dir=$work/serve/base
mkdir -p "$dir"
printf 'agent %s\n' "$BASE" >"$dir/monitorflow-agent-linux-x86_64"
jq -n --arg v "$BASE" --arg s "$(push monitorflow-server:e2e-base monitorflow-server "$BASE")" \
    --arg w "$(push monitorflow-web:e2e-base monitorflow-web "$BASE")" \
    --arg u "$(push monitorflow-updater:e2e-base monitorflow-updater "$BASE")" \
    --arg m "$(push monitorflow-mirror:e2e-base monitorflow-mirror "$BASE")" \
    '{version: $v, images: {server: $s, web: $w, updater: $u, mirror: $m}}' >"$dir/images.json"
MF_SIGNING_KEY=$key MF_RELEASE_KEYS=$keys MF_RELEASE_VERSION=$BASE "$repo/deploy/release-files.sh" "$dir" >/dev/null
chmod -R a+rX "$work/serve" "$work/tls"
for n in 1 2; do
    for _ in $(seq 30); do on "$n" curl -fs -o /dev/null "$files/base/VERSION" && break; sleep 1; done
done

# Runs the one-liner on host $1 with the options that follow.
installer() {
    local n=$1
    shift
    # shellcheck disable=SC2016 # expanded on the host
    on "$n" sh -c 'url=$1; shift; cd /root && curl -fsSL "$url/install-server.sh" \
        | env MF_DOWNLOAD_URL="$url" MF_PUBLIC_NAME=monitor.internal sh -s -- "$@"' sh "$files/base" "$@" 2>&1
}
compose() {
    local n=$1
    shift
    # shellcheck disable=SC2016 # expanded on the host
    on "$n" sh -c 'cd /opt/monitorflow/deploy && f=docker-compose.yml
        grep -q "^JOINED=1$" .env && f=docker-compose.joined.yml
        grep -q "^SHARE_ADDRESS=." .env && set -- -f docker-compose.shared.yml "$@"
        docker compose -f "$f" "$@"' sh "$@"
}
psql() { compose 1 exec -T postgres psql -U monitorflow -d monitorflow -tAc "$1"; }
# shellcheck source=deploy/db-role-test.sh
. "$(dirname "$0")/db-role-test.sh"
env_value() { on "$1" sed -n "s/^$2=//p" /opt/monitorflow/deploy/.env; }
api() { on "$1" curl -fsSk -b /tmp/cookies "https://monitor.internal/api/$2"; }
healthy() { [ "$(on "$1" docker inspect --format '{{.State.Health.Status}}' "$(compose "$1" ps -q server)")" = healthy ]; }
license() { # servers expires
    "$repo/scripts/license-sign.sh" --key "$license_key" --customer "Join test" --servers "$1" --expires "$2"
}

echo "== the first central server"
installer 1 | tee "$work/install.log"
first=$(env_value 1 DOMAIN)
first_ip=$(on 1 sh -c "ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p'")
link=$(grep -o 'https://[^ ]*/setup/#[^ ]*' "$work/install.log" | tail -n 1)
password=$(openssl rand -hex 16)
on 1 curl -fsSk -c /tmp/cookies -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg t "${link#*#}" --arg p "$password" '{token: $t, username: "admin", password: $p}')" \
    https://monitor.internal/api/setup >/dev/null
check "it runs, named $first" api 1 auth/me
check "it needs no license" bash -c "[ \"\$(docker exec $host-1 curl -fsSk -b /tmp/cookies https://monitor.internal/api/license | jq -r .state)\" = none ]"

echo "== --add-server without a license"
if installer 1 --add-server >"$work/refused.log"; then failed=1; echo "--add-server worked without a license: FAILED"; fi
check "it says a license is needed" grep -q 'needs a MonitorFlow Enterprise license' "$work/refused.log"
check "and how to get one" grep -q 'monitorflow#licence' "$work/refused.log"
check "nothing is shared" test -z "$(env_value 1 SHARE_ADDRESS)"
check "no join file" bash -c "! docker exec $host-1 test -e /root/monitorflow-join.tar"
check "the first central server still runs" healthy 1

echo "== a license for two central servers"
license 2 2099-12-31 >"$work/license.txt"
on 1 curl -fsSk -b /tmp/cookies -X PUT -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --rawfile t "$work/license.txt" '{text: $t}')" https://monitor.internal/api/license >/dev/null
check "it is valid" bash -c "[ \"\$(docker exec $host-1 curl -fsSk -b /tmp/cookies https://monitor.internal/api/license | jq -r .state)\" = valid ]"
installer 1 --add-server | tee "$work/add.log"
check "it shares the stores on its address" test "$(env_value 1 SHARE_ADDRESS)" = "$first_ip"
check "PostgreSQL answers there" on 1 bash -c "exec 3<>/dev/tcp/$first_ip/5432"
check "the join file is root's only" test "$(on 1 stat -c '%U %a' /root/monitorflow-join.tar)" = "root 600"
join_env() { on 1 tar -xOf /root/monitorflow-join.tar ./env; }
check "it holds the password of the server's database role" grep -q '^POSTGRES_APP_PASSWORD=.' <<<"$(join_env)"
check "and not the owner's (ADR-0109)" bash -c "! grep -q '^POSTGRES_PASSWORD=' <<<'$(join_env)'"
check "it prints the command to run" grep -q -- '--join monitorflow-join.tar' "$work/add.log"

echo "== the second central server joins"
docker cp -q "$host-1:/root/monitorflow-join.tar" "$work/join.tar"
docker cp -q "$work/join.tar" "$host-2:/root/monitorflow-join.tar"
installer 2 --join monitorflow-join.tar | tee "$work/join.log"
check "it reports the URL" grep -q 'is running at https://' "$work/join.log"
check "it says to add it to the DNS name" grep -q 'add its address' "$work/join.log"
check "it is marked as added" test "$(env_value 2 JOINED)" = 1
check "it runs the server and the web interface only" test "$(compose 2 ps --services | sort | tr '\n' ' ')" = "server web "
check "its server is healthy" healthy 2
check "it has no password of the database owner" test -z "$(env_value 2 POSTGRES_PASSWORD)"
check "every server runs on its own database role" test "$(psql "$SERVER_RUNS_AS_APP")" = t
# The owner is refused from another machine, password or not.
owner_password=$(env_value 1 POSTGRES_PASSWORD)
as_owner_from_2() {
    compose 2 run --rm --no-deps -T -e MONITORFLOW_DATABASE_REQUIRE_RESTRICTED_ROLE=false \
        -e "MONITORFLOW_DATABASE_URL=postgres://monitorflow:$owner_password@postgres:5432/monitorflow?sslmode=verify-full&sslrootcert=/certs/ca.pem" \
        server customer list 2>&1
}
check "the database refuses its owner from the added machine" grep -q 'no pg_hba.conf entry' <<<"$(as_owner_from_2)"
check "two instances share the database" test "$(psql "SELECT count(*) FROM server_instances")" = 2
check "it was added under its name" test "$(psql "SELECT name FROM server_instance_admissions")" = fresh2
check "the addition is audited" test "$(psql "SELECT count(*) FROM audit_log WHERE action = 'server_instance.add'")" = 1
check "the same certificate authority" test \
    "$(compose 1 exec -T server sha256sum /var/lib/monitorflow/server/ca/ca-key.pem)" = \
    "$(compose 2 exec -T server sha256sum /var/lib/monitorflow/server/ca/ca-key.pem)"
check "the same secrets key (ADR-0125)" test \
    "$(compose 1 exec -T server sha256sum /var/lib/monitorflow/server/secrets/keys)" = \
    "$(compose 2 exec -T server sha256sum /var/lib/monitorflow/server/secrets/keys)"
check "readable by the server only" test \
    "$(compose 2 exec -T server stat -c %a /var/lib/monitorflow/server/secrets/keys)" = 600
# Through the runner: systemd mounts a tmpfs on the hosts' /tmp, which
# docker cp does not see.
on 1 curl -fs "http://monitor.internal/trust/monitorflow-ca.crt" | on 2 sh -c 'cat >/root/browser-ca.crt'
check "its HTTPS verifies against the first's browser CA" on 2 curl -fs --cacert /root/browser-ca.crt \
    --connect-to "::127.0.0.1" -o /dev/null "https://$first/api/health/ready"
on 2 curl -fsSk -c /tmp/cookies -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg p "$password" '{username: "admin", password: $p}')" https://monitor.internal/api/auth/login >/dev/null
check "the first's administrator signs in on it" api 2 auth/me
check "the License page lists it, running" bash -c \
    "docker exec $host-1 curl -fsSk -b /tmp/cookies https://monitor.internal/api/license | jq -e '.added[0] | .name == \"fresh2\" and .running'"

echo "== no third central server over the license's count"
if installer 1 --add-server >"$work/full.log"; then failed=1; echo "--add-server worked over the count: FAILED"; fi
check "it says the license is full" grep -q 'allows 2 central servers' "$work/full.log"

echo "== after expiry"
license 2 2020-01-31 >"$work/expired.txt"
expired=$(cat "$work/expired.txt")
# From stdin: psql does not substitute variables in -c.
echo "UPDATE license SET text = :'text';" \
    | compose 1 exec -T postgres psql -U monitorflow -d monitorflow -q -v ON_ERROR_STOP=1 -v "text=$expired" >/dev/null
compose 2 restart server >/dev/null
for _ in $(seq 60); do healthy 2 && break; sleep 2; done
check "the added central server starts again" healthy 2
check "administrators are told" bash -c \
    "docker exec $host-1 curl -fsSk -b /tmp/cookies https://monitor.internal/api/license | jq -e '.state == \"expired\" and (.added | length) == 1'"
installer 2 | tee "$work/rerun.log"
check "running the installer again on it works" grep -q 'is running at https://' "$work/rerun.log"

echo "== a central server added before ADR-0109 needs a new join file"
app_password=$(env_value 2 POSTGRES_APP_PASSWORD)
on 2 sed -i "/^POSTGRES_APP_PASSWORD=/d; \$a POSTGRES_PASSWORD=$owner_password" /opt/monitorflow/deploy/.env
if installer 2 >"$work/old-join.log"; then failed=1; echo "the installer ran without a role of its own: FAILED"; fi
check "it says to make a new join file" grep -q -- '--add-server again' "$work/old-join.log"
check "and changed nothing" test "$(env_value 2 POSTGRES_PASSWORD)" = "$owner_password"
installer 2 --join monitorflow-join.tar | tee "$work/rejoin.log"
check "joining again with the file works" grep -q 'is running at https://' "$work/rejoin.log"
check "it has the role's password" test "$(env_value 2 POSTGRES_APP_PASSWORD)" = "$app_password"
check "and no longer the owner's" test -z "$(env_value 2 POSTGRES_PASSWORD)"
check "its server is healthy" healthy 2

echo "== a central server added before ADR-0125 needs a new join file"
compose 2 exec -T --user 0 server rm -rf /var/lib/monitorflow/server/secrets
if installer 2 >"$work/no-key.log"; then failed=1; echo "the installer ran without the secrets key: FAILED"; fi
check "it says to make a new join file" grep -q -- '--add-server again' "$work/no-key.log"
check "and its server still runs" healthy 2
installer 2 --join monitorflow-join.tar | tee "$work/key-rejoin.log"
check "joining again with the file works" grep -q 'is running at https://' "$work/key-rejoin.log"
check "it has the first one's secrets key" test \
    "$(compose 1 exec -T server sha256sum /var/lib/monitorflow/server/secrets/keys)" = \
    "$(compose 2 exec -T server sha256sum /var/lib/monitorflow/server/secrets/keys)"
check "its server is healthy" healthy 2

echo "== the second leads when the first's server stops"
compose 1 stop server >/dev/null
second=$(psql "SELECT id FROM server_instances WHERE hostname = 'fresh2'")
leads() { [ "$(psql "SELECT holder FROM cluster_leases WHERE name = 'leader' AND expires_at > now()")" = "$second" ]; }
for _ in $(seq 45); do leads && break; sleep 2; done
check "within the lease" leads
check "and still serves the web interface" api 2 auth/me
compose 1 start server >/dev/null

if [ "$failed" -ne 0 ]; then
    for n in 1 2; do compose "$n" ps -a || true; compose "$n" logs --tail 40 server web || true; done
    exit 1
fi
echo "join test passed"
