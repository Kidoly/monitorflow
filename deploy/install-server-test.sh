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
# updater, from an install older than the mirror tools; --uninstall, with and without the data; MF_PUBLIC_NAME; the browser certificate;
# --restore on a new install, with the public name typed on a terminal.
#
# The browser certificate (ADR-0068): the internal CA is tested for real,
# from the installer's output to a verified HTTPS request. No public name
# exists here, so for ACME a Pebble server in the host's network namespace
# plays the public CA, and validates port 80 of the host for real. Two
# stand-ins, neither of them in production: the name resolves publicly only
# through the host's /etc/hosts, and the test release's web image is the
# release's with two lines more in its Caddyfile, the ACME directory and its
# root. What this cannot show is a real public CA reaching a real host.
#
# The images must exist and trust MF_TEST_SIGNING_KEY, as for
# deploy/update-test.sh, which CI runs first:
# monitorflow-{server,web,updater,mirror}:e2e-base and
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
    docker rm -f "$host-registry" "$host-files" "$host-pebble" >/dev/null 2>&1 || true
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
# --privileged hands this container the host's /dev, its virtual consoles
# included: a getty on tty1 there would hang up the desktop session of
# whoever runs the test. Nothing here needs a console login.
RUN systemctl mask getty.target getty@.service autovt@.service serial-getty@.service \
    console-getty.service container-getty@.service systemd-vconsole-setup.service
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
release() { # directory version server web updater mirror
    local dir=$work/serve/$1
    mkdir -p "$dir"
    printf 'agent %s\n' "$2" >"$dir/monitorflow-agent-linux-x86_64"
    jq -n --arg v "$2" --arg s "$3" --arg w "$4" --arg u "$5" --arg m "$6" \
        '{version: $v, images: {server: $s, web: $w, updater: $u, mirror: $m}}' >"$dir/images.json"
    MF_SIGNING_KEY=$key MF_RELEASE_KEYS=$keys MF_RELEASE_VERSION=$2 "$repo/deploy/release-files.sh" "$dir" >/dev/null
}
base_server=$(push monitorflow-server:e2e-base monitorflow-server "$BASE")
base_web=$(push monitorflow-web:e2e-base monitorflow-web "$BASE")
base_updater=$(push monitorflow-updater:e2e-base monitorflow-updater "$BASE")
base_mirror=$(push monitorflow-mirror:e2e-base monitorflow-mirror "$BASE")
next_server=$(push monitorflow-server:e2e-next monitorflow-server "$NEXT")
next_updater=$(push monitorflow-updater:e2e-next monitorflow-updater "$NEXT")
# Another digest, so a check of the image proves an update.
printf 'FROM monitorflow-mirror:e2e-base\nLABEL release=next\n' \
    | DOCKER_BUILDKIT=0 docker build -q -t monitorflow-mirror:e2e-next - >/dev/null
next_mirror=$(push monitorflow-mirror:e2e-next monitorflow-mirror "$NEXT")
release base "$BASE" "$base_server" "$base_web" "$base_updater" "$base_mirror"
release next "$NEXT" "$next_server" "$base_web" "$next_updater" "$next_mirror"
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
# shellcheck source=deploy/db-role-test.sh
. "$repo/deploy/db-role-test.sh"
env_value() { in_host sed -n "s/^$1=//p" /opt/monitorflow/deploy/.env; }
absent() { ! in_host test -e "$1"; }
server_version() { compose exec -T server monitorflow-server --version; }
ca() { compose exec -T server sha256sum /var/lib/monitorflow/server/ca/ca-key.pem; }
api() { in_host curl -fsSk -b /tmp/cookies "https://$(env_value DOMAIN)/api/$1"; }
containers() { in_host docker ps -aq --filter label=com.docker.compose.project=monitorflow; }
volume_exists() { in_host docker volume inspect monitorflow_pgdata; }
# The mirror service runs the image $1, is healthy and answers the server.
mirror_runs() {
    local id
    id=$(compose ps -q mirror)
    [ "$(in_host docker inspect --format '{{.Config.Image}} {{.State.Health.Status}}' "$id")" = "$1 healthy" ] \
        && api mirrors/catalog | jq -e '.tools.available'
}
# The fingerprint in the output $1 of the installer.
printed_fingerprint() { grep -Eo '([0-9A-F]{2}:){31}[0-9A-F]{2}' "$1" | tail -n 1; }
# The fingerprint of the certificate $1 of the host, computed here.
fingerprint_of() { in_host cat "$1" | openssl x509 -noout -fingerprint -sha256 | cut -d = -f 2; }
# Downloads the CA from the page at the address $1 for the host's curl.
trust_ca() { in_host curl -fs -o /tmp/ca.crt "http://$1/trust/monitorflow-ca.crt"; }
# An HTTPS request for the name $1 on this host, verified against $2.
verified() { in_host curl -fs --cacert "$2" --connect-to ::127.0.0.1 -o /dev/null "https://$1/api/health/ready"; }
# Port 80 of the host sends /trust/ to HTTPS: no trust page.
no_trust_page() {
    [ "$(in_host curl -s -o /dev/null -w '%{http_code}' -H "host: $1" http://127.0.0.1/trust/)" = 308 ]
}
hsts() { in_host curl -fsI --cacert "$2" --connect-to ::127.0.0.1 "https://$1/" | grep -qi '^strict-transport-security'; }
served_serial() {
    in_host sh -c "openssl s_client -connect 127.0.0.1:443 -servername $1 </dev/null 2>/dev/null | openssl x509 -noout -serial"
}
# deploy/ as an install from before the mirror tools has it: a compose file
# without them, and no pin for their image.
older_install() {
    compose rm -sf mirror
    in_host cat /opt/monitorflow/deploy/docker-compose.yml | awk '
        /^  mirror:$/ && !seen { seen = 1; skip = 1 }
        skip { if (/restart: unless-stopped/) skip = 0; next }
        !/mirrors:\/(var|serverdata)|MIRROR_P|:7480"|MONITORFLOW_CERTIFICATE|MONITORFLOW_HTTPS_PORT/ { print }' \
        | in_host sh -c 'cat >/tmp/old.yml && mv /tmp/old.yml /opt/monitorflow/deploy/docker-compose.yml'
    in_host sed -i '/^MIRROR_IMAGE=/d; /^WEB_CERTIFICATE=/d' /opt/monitorflow/deploy/.env
    # And from before the server's own database role (ADR-0109).
    in_host sed -i '/^POSTGRES_APP_PASSWORD=/d' /opt/monitorflow/deploy/.env
    # shellcheck disable=SC2016 # compose expands it
    in_host sed -i -e '/^  # One-shot, before every start of the server/,/^$/d' -e '/db-migrate: {/d' \
        -e '/MONITORFLOW_DATABASE_REQUIRE_RESTRICTED_ROLE/d' -e '/^    labels:$/{N;/pg-hba/d;}' \
        -e 's#MONITORFLOW_DATABASE_URL: \*database-url#MONITORFLOW_DATABASE_URL: "postgres://monitorflow:${POSTGRES_PASSWORD}@postgres:5432/monitorflow?sslmode=verify-full\&sslrootcert=/certs/ca.pem"#' \
        /opt/monitorflow/deploy/docker-compose.yml
    printf 'local    all  all                 trust\nhostssl  all  all  all            scram-sha-256\n' \
        | in_host sh -c 'cat >/opt/monitorflow/deploy/docker/pg_hba.conf'
    compose up -d --no-build --force-recreate postgres >/dev/null
    compose up -d --no-build --wait --wait-timeout 300 >/dev/null
}
report_cleanly() { grep -q 'is running at https://' "$1" && ! grep -q 'error:' "$1"; }

echo "== which names can have a public certificate"
in_host sh -c 'cat >>/etc/hosts' <<'EOF'
203.0.113.10 monitor.example.com public.lan single
10.1.2.3 private.example.com
100.100.1.1 tailnet.example.com
EOF
functions=$(sed -n '/^acme_name()/,/^}/p; /^resolves_publicly()/,/^}/p; /^public_name()/p' "$repo/deploy/install-server.sh")
# shellcheck disable=SC2016 # expanded on the host
public_name() { in_host sh -c "$functions"$'\n''public_name "$1"' sh "$1"; }
not_public_name() { ! public_name "$1"; }
check "public address: public" public_name monitor.example.com
check "whatever the case" public_name Monitor.Example.COM
for name in 203.0.113.10 10.1.2.3 public.lan PUBLIC.LAN public.lan. single private.example.com \
    tailnet.example.com unresolved.example.invalid; do
    check "$name: not public" not_public_name "$name"
done

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
check "the mirror image too" test "$(env_value MIRROR_IMAGE)" = "$base_mirror"
check "the release URL follows the source" test "$(env_value RELEASE_URL)" = "$files/base"
password=$(env_value POSTGRES_PASSWORD)
check "a password was generated" test "${#password}" = 48
app_password=$(env_value POSTGRES_APP_PASSWORD)
check "and another for the server's database role" test "${#app_password}" = 48 -a "$app_password" != "$password"
check "the server runs on that role" test "$(psql "$SERVER_RUNS_AS_APP")" = t
check "which owns nothing and cannot change the audit log" test "$(psql "$APP_ROLE_RESTRICTED")" = t
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
check "the mirror tools run after an install" mirror_runs "$base_mirror"
serverdata_ca=$(ca)
fingerprint=$(printed_fingerprint "$work/install.log")
trust_ca "$guessed"
check "it chose the internal CA and says why" grep -q "this server's own CA, as public CAs do not issue for $guessed" "$work/install.log"
check "and remembers it" test "$(env_value WEB_CERTIFICATE)" = internal
check "it prints the trust page" grep -q "http://$guessed/trust/" "$work/install.log"
check "the trust page comes before the setup link" bash -c \
    "grep -n -e /trust/ -e /setup/ '$work/install.log' | head -n 1 | grep -q /trust/"
check "the printed fingerprint is the CA's" test "$(fingerprint_of /tmp/ca.crt)" = "$fingerprint"
check "the page shows the same one" grep -qF "$fingerprint" <<<"$(in_host curl -fs "http://$guessed/trust/")"
check "HTTPS verifies against the page's certificate" verified "$guessed" /tmp/ca.crt
check "with no HSTS" bash -c "! docker exec $host curl -fsI --cacert /tmp/ca.crt https://$guessed/ | grep -qi strict-transport"

echo "== second run"
server_id=$(compose ps -q server)
web_id=$(compose ps -q web)
installer base | tee "$work/rerun.log"
check "it reports the URL" report_cleanly "$work/rerun.log"
check "it keeps the secrets" test "$(env_value POSTGRES_PASSWORD)" = "$password"
check "the server was not recreated" test "$(compose ps -q server)" = "$server_id"
check "nor the web interface" test "$(compose ps -q web)" = "$web_id"
check "the same CA, the same fingerprint" test "$(printed_fingerprint "$work/rerun.log")" = "$fingerprint"
check "no setup link once a user exists" bash -c "! grep -q /setup/ '$work/rerun.log'"
check "the session and the data are kept" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1

echo "== upgrade by running it again, from before the mirror tools"
older_install
check "the older install has no mirror service" test -z "$(compose ps -aq mirror 2>/dev/null)"
installer next | tee "$work/upgrade.log"
check "the server runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"
check ".env pins the new images" test "$(env_value SERVER_IMAGE)" = "$next_server"
check ".env pins the mirror image" test "$(env_value MIRROR_IMAGE)" = "$next_mirror"
check "the mirror tools run after an upgrade" mirror_runs "$next_mirror"
check "the certificate setting is added" test "$(env_value WEB_CERTIFICATE)" = internal
new_app_password=$(env_value POSTGRES_APP_PASSWORD)
check "a password for the server's database role is added" test "${#new_app_password}" = 48
check "the server moved to that role" test "$(psql "$SERVER_RUNS_AS_APP")" = t
check "PostgreSQL reads the release's pg_hba.conf" compose exec -T postgres grep -q monitorflow_app /etc/postgresql/pg_hba.conf
check "which owns nothing and cannot change the audit log" test "$(psql "$APP_ROLE_RESTRICTED")" = t
check "the same CA, the same fingerprint" test "$(printed_fingerprint "$work/upgrade.log")" = "$fingerprint"
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
check "and the mirror tools" mirror_runs "$next_mirror"

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
check "an internal name gets the internal CA" test "$(env_value WEB_CERTIFICATE)" = internal
check "without trying ACME" bash -c "! grep -q acme_client <<<\"\$1\"" sh "$(compose logs web)"
trust_ca "$guessed"
check "HTTPS verifies against the page's certificate" verified monitor.internal /tmp/ca.crt
installer next --uninstall --purge >/dev/null

echo "== ACME, against a local ACME server"
public=monitor.example.com
acme=$work/acme
mkdir "$acme"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=pebble \
    -addext "subjectAltName=IP:$guessed" -keyout "$acme/key.pem" -out "$acme/pebble.pem" 2>/dev/null
printf '127.0.0.1 localhost %s\n' "$public" >"$acme/hosts"
jq -n '{pebble: {listenAddress: "0.0.0.0:14000", managementListenAddress: "0.0.0.0:15000",
    certificate: "/acme/pebble.pem", privateKey: "/acme/key.pem", httpPort: 80, tlsPort: 443}}' >"$acme/pebble.json"
chmod -R a+rX "$acme"
docker run -d --name "$host-pebble" --network "container:$host" -e PEBBLE_VA_NOSLEEP=1 -v "$acme:/acme:ro" \
    -v "$acme/hosts:/etc/hosts:ro" ghcr.io/letsencrypt/pebble:2.10.1 -config /acme/pebble.json >/dev/null
pebble_ready() { for _ in $(seq 30); do in_host curl -fsk -o /dev/null https://127.0.0.1:14000/dir && break; sleep 1; done; }
pebble_ready
docker run --rm --entrypoint cat monitorflow-web:e2e-base /etc/caddy/Caddyfile \
    | sed "/default_sni/a\\\tacme_ca https://$guessed:14000/dir\n\tacme_ca_root /etc/caddy/pebble.pem" >"$acme/Caddyfile"
printf 'FROM monitorflow-web:e2e-base\nCOPY Caddyfile pebble.pem /etc/caddy/\n' >"$acme/Dockerfile"
DOCKER_BUILDKIT=0 docker build -q -t monitorflow-web:e2e-acme "$acme" >/dev/null
release acme "$NEXT" "$next_server" "$(push monitorflow-web:e2e-acme monitorflow-web acme)" "$next_updater" "$next_mirror"
chmod -R a+rX "$work/serve"
# The root of what Pebble issues, new at each of its starts.
acme_root() { in_host sh -c 'curl -fsk https://127.0.0.1:15000/roots/0 >/tmp/acme-root.pem'; }

VARS=MF_PUBLIC_NAME=$public installer acme | tee "$work/acme.log"
acme_root
check "it reports the URL" report_cleanly "$work/acme.log"
check "it chose ACME and says why" grep -q "a public one by ACME, as $public resolves publicly" "$work/acme.log"
check "and remembers it" test "$(env_value WEB_CERTIFICATE)" = acme
check "no trust page to open" bash -c "! grep -q -e /trust/ -e fingerprint '$work/acme.log'"
check "the challenge came through port 80" grep -q 'served key authentication' <<<"$(compose logs web)"
check "HTTPS verifies against the ACME CA" verified "$public" /tmp/acme-root.pem
check "with HSTS" hsts "$public" /tmp/acme-root.pem
check "port 80 has no trust page" no_trust_page "$public"
serial=$(served_serial "$public")
installer acme | tee "$work/acme-rerun.log"
check "a re-run keeps the certificate" test "$(served_serial "$public")" = "$serial"
in_host sh -c "grep -v $public /etc/hosts >/tmp/hosts && cat /tmp/hosts >/etc/hosts"
installer acme | tee "$work/acme-dns.log"
check "a name that stops resolving stays on ACME" test "$(env_value WEB_CERTIFICATE)" = acme
check "with the same certificate" test "$(served_serial "$public")" = "$serial"
in_host sh -c "echo '203.0.113.10 $public' >>/etc/hosts"
installer acme --uninstall --purge >/dev/null

echo "== ACME that fails"
docker stop "$host-pebble" >/dev/null
VARS=MF_PUBLIC_NAME=$public installer acme | tee "$work/acme-failed.log"
check "it reports the URL" report_cleanly "$work/acme-failed.log"
check "it says so" grep -q 'no certificate from a public CA' "$work/acme-failed.log"
check "it falls back to the internal CA" test "$(env_value WEB_CERTIFICATE)" = internal
trust_ca "$guessed"
check "it prints the trust page" grep -q "http://$public/trust/" "$work/acme-failed.log"
check "the printed fingerprint is the CA's" test "$(fingerprint_of /tmp/ca.crt)" = "$(printed_fingerprint "$work/acme-failed.log")"
check "HTTPS verifies against the page's certificate" verified "$public" /tmp/ca.crt
docker start "$host-pebble" >/dev/null
pebble_ready
installer acme | tee "$work/acme-again.log"
acme_root
check "the next run moves to ACME" test "$(env_value WEB_CERTIFICATE)" = acme
check "and HTTPS verifies against the ACME CA" verified "$public" /tmp/acme-root.pem
installer acme --uninstall --purge >/dev/null
docker rm -f "$host-pebble" >/dev/null

echo "== --restore on a new install, on a terminal"
in_host curl -fsSL -o /root/install-server.sh "$files/base/install-server.sh"
printf 'restored.internal\n' | in_host script -qefc \
    "env MF_DOWNLOAD_URL=$files/next sh /root/install-server.sh --restore /root/backup.tar.gz" /dev/null | tee "$work/restore.log"
check "the name was typed" test "$(env_value DOMAIN)" = restored.internal
check "the data is the backup's" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
check "the restored database has the server's role again" test "$(psql "$APP_ROLE_RESTRICTED")" = t
check "and the server runs on it" test "$(psql "$SERVER_RUNS_AS_APP")" = t
check "the CA is the same" test "$(ca)" = "$serverdata_ca"
check "no setup link: the backup has the administrator" bash -c "! grep -q /setup/ '$work/restore.log'"
check "the server runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"

if [ "$failed" -ne 0 ]; then
    compose ps -a || true
    compose logs --tail 40 server web updater mirror || true
    exit 1
fi
echo "server install test passed"
