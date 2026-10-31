#!/usr/bin/env bash
# End-to-end test of deploy/install-server.sh (ADR-0065), on a fresh host: a
# systemd container without Docker, whose own Docker the installer installs
# with Docker's real script. There are no published images to pull yet, so a
# local registry holds the e2e images and a local HTTPS directory holds
# release files made by deploy/release-files.sh, signed by a key that the
# release's installer carries, as a real release's does. Both run beside the
# host, in its network namespace, as localhost. The installer has no test
# path: MF_DOWNLOAD_URL is the self-hosted source, and a test release names
# its registry in images.json like any other.
#
# Checks: tampered and foreign releases change nothing; the one-liner run
# without a terminal; the setup link; a re-run; an upgrade through the
# updater; --uninstall, with and without the data; MF_PUBLIC_NAME; --restore
# on a new install, with the public name typed on a terminal.
#
# The images must exist and trust MF_TEST_SIGNING_KEY, as for
# deploy/update-test.sh, which CI runs first:
# monitorflow-{server,web,updater}:e2e-base and
# monitorflow-{server,updater}:e2e-next, built as 99.0.0. Needs docker with
# compose, curl, jq and openssl, and Internet access from the test host.
#   MF_TEST_SIGNING_KEY=key.pem deploy/install-server-test.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
key=$(realpath "${MF_TEST_SIGNING_KEY:?MF_TEST_SIGNING_KEY must name the release signing key the images trust}")
BASE=$(awk -F '"' '/^\[workspace.package\]/ { ws = 1 } ws && /^version = / { print $2; exit }' "$repo/Cargo.toml")
NEXT=99.0.0
host=monitorflow-fresh-host
PORT=${MF_TEST_REGISTRY_PORT:-5556}
REGISTRY=localhost:$PORT
files=https://localhost:8444
work=$(mktemp -d)

cleanup() {
    docker rm -f "$host-registry" "$host-files" >/dev/null 2>&1 || true
    docker rm -fv "$host" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

failed=0
check() {
    printf '%-55s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}

# The fresh host: what a minimal server has, without Docker.
docker build -q -t "$host" - >/dev/null <<'EOF'
FROM debian:trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
    systemd systemd-sysv dbus ca-certificates curl openssl iproute2 procps bsdextrautils
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
EOF
docker run -d --name "$host" --hostname fresh --privileged --cgroupns=private \
    --tmpfs /run --tmpfs /run/lock -v /var/lib/docker -v /var/lib/containerd \
    -p "$PORT:$PORT" "$host" >/dev/null
in_host() { docker exec -i "$host" "$@"; }
# Not is-system-running: some units never settle in a container.
for _ in $(seq 120); do in_host systemctl is-active -q basic.target 2>/dev/null && break; sleep 1; done
if ! in_host systemctl is-active -q basic.target; then
    in_host systemctl list-jobs --no-pager || true
    in_host systemctl --failed --no-pager || true
    docker logs --tail 40 "$host" || true
    echo "the fresh host's systemd did not start" >&2
    exit 1
fi

docker run -d --name "$host-registry" --network "container:$host" -e "REGISTRY_HTTP_ADDR=:$PORT" registry:2.8.3 >/dev/null
for _ in $(seq 30); do curl -fs "http://$REGISTRY/v2/" >/dev/null && break; sleep 1; done
# Pushes a local image to the registry; prints it by digest.
push() {
    docker tag "$1" "$REGISTRY/$2:$3"
    docker push -q "$REGISTRY/$2:$3" >/dev/null
    docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$REGISTRY/$2:$3" \
        | grep -m1 "^$REGISTRY/$2@sha256:"
}

# Signed release directories, as the release workflow makes them, with a
# stand-in agent binary. The release's installer carries the test key.
keys=$work/keys.pem
{ cat "$repo/deploy/release-keys.pem"; openssl pkey -in "$key" -pubout; } >"$keys"
release() { # directory version server web updater
    local dir=$work/serve/$1
    mkdir -p "$dir"
    printf 'agent %s\n' "$2" >"$dir/monitorflow-agent-linux-x86_64"
    jq -n --arg v "$2" --arg s "$3" --arg w "$4" --arg u "$5" \
        '{version: $v, images: {server: $s, web: $w, updater: $u}}' >"$dir/images.json"
    MF_SIGNING_KEY=$key MF_RELEASE_KEYS=$keys MF_RELEASE_VERSION=$2 "$repo/deploy/release-files.sh" "$dir" >/dev/null
}
base_server=$(push monitorflow-server:e2e-base monitorflow-server "$BASE")
base_web=$(push monitorflow-web:e2e-base monitorflow-web "$BASE")
base_updater=$(push monitorflow-updater:e2e-base monitorflow-updater "$BASE")
next_server=$(push monitorflow-server:e2e-next monitorflow-server "$NEXT")
next_updater=$(push monitorflow-updater:e2e-next monitorflow-updater "$NEXT")
release base "$BASE" "$base_server" "$base_web" "$base_updater"
release next "$NEXT" "$next_server" "$base_web" "$next_updater"
# Copies with a checksum that does not match (signed again, so only the
# checksum is wrong), and a signature by another key.
openssl genpkey -algorithm ed25519 -out "$work/other.pem" 2>/dev/null
cp -r "$work/serve/base" "$work/serve/tampered"
cp -r "$work/serve/base" "$work/serve/foreign"
awk -v zero="$(printf '%064d' 0)" '$2 == "monitorflow-server-compose.tar.gz" { $1 = zero } 1' \
    "$work/serve/base/SHA256SUMS" >"$work/serve/tampered/SHA256SUMS"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$work/serve/tampered/SHA256SUMS" \
    -out "$work/serve/tampered/SHA256SUMS.sig"
openssl pkeyutl -sign -inkey "$work/other.pem" -rawin -in "$work/serve/base/SHA256SUMS" \
    -out "$work/serve/foreign/SHA256SUMS.sig"

# HTTPS with a CA of its own, which the host trusts: its curl also reaches
# get.docker.com.
mkdir "$work/tls"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=localhost \
    -addext subjectAltName=DNS:localhost -keyout "$work/tls/key.pem" -out "$work/tls/cert.pem" 2>/dev/null
docker cp -q "$work/tls/cert.pem" "$host:/usr/local/share/ca-certificates/e2e.crt"
in_host update-ca-certificates >/dev/null
chmod -R a+rX "$work"
cat >"$work/tls/serve.py" <<'EOF'
import functools, http.server, ssl
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory="/srv")
server = http.server.ThreadingHTTPServer(("127.0.0.1", 8444), handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/tls/cert.pem", "/tls/key.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
EOF
docker run -d --name "$host-files" --network "container:$host" -v "$work/serve:/srv:ro" -v "$work/tls:/tls:ro" \
    python:3.13-alpine python3 /tls/serve.py >/dev/null
for _ in $(seq 30); do in_host curl -fs -o /dev/null "$files/base/VERSION" && break; sleep 1; done

# Runs the one-liner on the host, from a release directory: installer
# directory [options]. VARS holds settings for its environment.
installer() {
    local dir=$1
    shift
    # shellcheck disable=SC2016 # expanded on the host
    in_host sh -c 'url=$1; vars=$2; shift 2; curl -fsSL "${url%/*}/base/install-server.sh" \
        | env MF_DOWNLOAD_URL="$url" $vars sh -s -- "$@"' sh "$files/$dir" "${VARS:-}" "$@" 2>&1
}
refuses() { ! installer "$1"; }
compose() { in_host sh -c 'cd /opt/monitorflow && docker compose -f deploy/docker-compose.yml "$@"' sh "$@"; }
psql() { compose exec -T postgres psql -U monitorflow -d monitorflow -tAc "$1"; }
env_value() { in_host sed -n "s/^$1=//p" /opt/monitorflow/deploy/.env; }
absent() { ! in_host test -e "$1"; }
server_version() { compose exec -T server monitorflow-server --version; }
ca() { compose exec -T server sha256sum /var/lib/monitorflow/server/ca/ca-key.pem; }
api() { in_host curl -fsSk -b /tmp/cookies "https://$(env_value DOMAIN)/api/$1"; }
containers() { in_host docker ps -aq --filter label=com.docker.compose.project=monitorflow; }
volume_exists() { in_host docker volume inspect monitorflow_pgdata; }
report_cleanly() { grep -q 'is running at https://' "$1" && ! grep -q 'error:' "$1"; }

echo "== refused releases"
check "a checksum mismatch is refused" refuses tampered
check "another key's signature is refused" refuses foreign
check "and nothing changed, Docker included" bash -c "! docker exec $host sh -c 'command -v docker || test -e /opt/monitorflow'"

echo "== first install, no terminal, no setting"
installer base | tee "$work/install.log"
check "it says it installs Docker" grep -q 'Docker is not installed' "$work/install.log"
check "Docker runs" in_host docker info
check "the release signature was verified" grep -q 'release signature verified' "$work/install.log"
check "it reports the URL" report_cleanly "$work/install.log"
guessed=$(in_host sh -c "ip -4 route get 1.1.1.1 | sed -n 's/.* src \([0-9.]*\).*/\1/p'")
check "the name is guessed: the address, as no name resolves" test "$(env_value DOMAIN)" = "$guessed"
check "secrets file root 0600" test "$(in_host stat -c '%U %a' /opt/monitorflow/deploy/.env)" = "root 600"
check "images are pinned by digest" test "$(env_value SERVER_IMAGE)" = "$base_server"
check "the release URL follows the source" test "$(env_value RELEASE_URL)" = "$files/base"
password=$(env_value POSTGRES_PASSWORD)
check "a password was generated" test "${#password}" = 48
check "setup is pending" bash -c "docker exec $host curl -fsSk https://$guessed/api/setup | jq -e .pending"
link=$(grep -o 'https://[^ ]*/setup/#[^ ]*' "$work/install.log" | tail -n 1)
check "the setup link names the public name" grep -q "^https://$guessed/setup/#" <<<"$link"
admin=$(openssl rand -hex 16)
in_host curl -fsSk -c /tmp/cookies -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg t "${link#*#}" --arg p "$admin" '{token: $t, username: "admin", password: $p}')" \
    "https://$guessed/api/setup" >/dev/null
check "the link creates the administrator" api auth/me
compose exec -T server monitorflow-server token create --role agent --id ci --site kept --create-site >/dev/null
check "the page's command works in /opt/monitorflow" compose exec -T server monitorflow-server setup-link
serverdata_ca=$(ca)

echo "== second run"
server_id=$(compose ps -q server)
installer base | tee "$work/rerun.log"
check "it reports the URL" report_cleanly "$work/rerun.log"
check "it keeps the secrets" test "$(env_value POSTGRES_PASSWORD)" = "$password"
check "the server was not recreated" test "$(compose ps -q server)" = "$server_id"
check "no setup link once a user exists" bash -c "! grep -q /setup/ '$work/rerun.log'"
check "the session and the data are kept" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1

echo "== upgrade by running it again"
installer next | tee "$work/upgrade.log"
check "the server runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"
check ".env pins the new images" test "$(env_value SERVER_IMAGE)" = "$next_server"
check "it keeps the secrets" test "$(env_value POSTGRES_PASSWORD)" = "$password"
check "the data is kept" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
check "the updater was replaced" test "$(in_host docker inspect --format '{{.Config.Image}}' "$(compose ps -q updater)")" = "$next_updater"
check "the agent release is cached" grep -qx "$NEXT" <<<"$(compose exec -T server monitorflow-server agent-release list)"
backup=$(api backups | jq -r '.files[0].name')
check "a backup was taken" grep -q '^monitorflow-backup-.*-update\.tar\.gz$' <<<"$backup"
api "backups/$backup" | in_host sh -c 'cat >/root/backup.tar.gz'
installer base >"$work/older.log"
check "an older release is not installed over it" grep -q 'nothing to update' "$work/older.log"

echo "== uninstall, keeping the data"
installer next --uninstall
check "the containers are gone" test -z "$(containers)"
check "the data is kept" volume_exists
check "the settings are kept" test "$(env_value POSTGRES_PASSWORD)" = "$password"
check "the compose files are gone" absent /opt/monitorflow/deploy/docker-compose.yml
installer next | tee "$work/back.log"
check "running it again brings all of it back" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
check "with the same CA" test "$(ca)" = "$serverdata_ca"

echo "== data without settings"
in_host mv /opt/monitorflow/deploy/.env /root/env
installer next --uninstall >/dev/null
check "no new password over an old database" refuses next
in_host mv /root/env /opt/monitorflow/deploy/.env

echo "== uninstall with the data"
installer next --uninstall --purge
check "the containers are gone" test -z "$(containers)"
check "the data is gone" bash -c "! docker exec $host docker volume inspect monitorflow_pgdata"
check "the files are gone" absent /opt/monitorflow

echo "== MF_PUBLIC_NAME"
VARS=MF_PUBLIC_NAME=monitor.internal installer next | tee "$work/named.log"
check "it is the public name" test "$(env_value DOMAIN)" = monitor.internal
check "agents can use it or the address" test "$(env_value SERVER_NAMES)" = "monitor.internal,$guessed"
check "the setup link names it" grep -q 'https://monitor.internal/setup/#' "$work/named.log"
installer next --uninstall --purge >/dev/null

echo "== --restore on a new install, on a terminal"
in_host curl -fsSL -o /root/install-server.sh "$files/base/install-server.sh"
printf 'restored.internal\n' | in_host script -qefc \
    "env MF_DOWNLOAD_URL=$files/next sh /root/install-server.sh --restore /root/backup.tar.gz" /dev/null | tee "$work/restore.log"
check "the name was typed" test "$(env_value DOMAIN)" = restored.internal
check "the data is the backup's" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
check "the CA is the same" test "$(ca)" = "$serverdata_ca"
check "no setup link: the backup has the administrator" bash -c "! grep -q /setup/ '$work/restore.log'"
check "the server runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"

if [ "$failed" -ne 0 ]; then
    compose ps -a || true
    compose logs --tail 40 server updater || true
    exit 1
fi
echo "server install test passed"
