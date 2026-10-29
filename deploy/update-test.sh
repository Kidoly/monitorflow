#!/usr/bin/env bash
# End-to-end test of server updates (ADR-0056): starts the production
# compose from a copy of deploy/ with a base version of the images, pushes
# a newer "release" to a local registry and serves its signed release
# files, then updates through the API as the web interface does: check,
# update. Checks that the new version runs, the data is kept, the backup
# exists, the agent release is cached and the updater replaced itself.
# Then a release whose server image cannot start is rolled back, and a
# downgrade and a release signed by another key are refused. Last, a
# backup taken with "Back up now" is downloaded and restored with
# deploy/restore.sh (ADR-0058).
#
# The images must trust MF_TEST_SIGNING_KEY: build them with its public
# half in deploy/release-keys.pem. With MF_TEST_BUILD=0 the images are not
# built, and must exist: monitorflow-{server,web,updater}:e2e-base and
# monitorflow-{server,updater}:e2e-next, built with
# MONITORFLOW_BUILD_VERSION=99.0.0. Needs docker with compose (and buildx
# to build), curl, jq and openssl.
#   MF_TEST_SIGNING_KEY=key.pem deploy/update-test.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
key=$(realpath "${MF_TEST_SIGNING_KEY:?MF_TEST_SIGNING_KEY must name the release signing key the images trust}")
NEXT=99.0.0 BROKEN=99.1.0 UNTRUSTED=99.2.0
REGISTRY=localhost:${MF_TEST_REGISTRY_PORT:-5555}
HTTPS_PORT=38443
project=monitorflow-update
work=$(mktemp -d)

compose() {
    docker compose -p "$project" -f "$work/deploy/docker-compose.yml" -f "$work/deploy/docker-compose.test.yml" "$@"
}
cleanup() {
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    docker rm -f "$project-registry" >/dev/null 2>&1 || true
    # The updater wrote some files as root.
    rm -rf "$work" 2>/dev/null || docker run --rm -v "$work:/w" busybox:1.37 rm -rf /w/deploy /w/serve
    rm -rf "$work" 2>/dev/null || true
}
trap cleanup EXIT

failed=0
check() {
    printf '%-55s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}

# Images: the base version, and the next one built as 99.0.0.
build() {
    docker buildx build --load --build-arg "MONITORFLOW_BUILD_VERSION=$2" \
        -f "$repo/deploy/docker/$1.Dockerfile" -t "$3" "$repo"
}
if [ "${MF_TEST_BUILD:-1}" != 0 ]; then
    for image in server web updater; do build "$image" "" "monitorflow-$image:e2e-base"; done
    build server "$NEXT" monitorflow-server:e2e-next
    build updater "$NEXT" monitorflow-updater:e2e-next
fi
# A server image that never starts.
printf 'FROM monitorflow-server:e2e-next\nENTRYPOINT ["/bin/false"]\n' \
    | DOCKER_BUILDKIT=0 docker build -q -t monitorflow-server:e2e-broken - >/dev/null

docker run -d --name "$project-registry" -p "${REGISTRY#localhost:}:5000" registry:2.8.3 >/dev/null
for _ in $(seq 30); do curl -fs "http://$REGISTRY/v2/" >/dev/null && break; sleep 1; done
# Pushes a local image to the registry; prints it by digest.
push() {
    docker tag "$1" "$REGISTRY/$2:$3"
    docker push -q "$REGISTRY/$2:$3" >/dev/null
    docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$REGISTRY/$2:$3" \
        | grep -m1 "^$REGISTRY/$2@sha256:"
}
next_server=$(push monitorflow-server:e2e-next monitorflow-server "$NEXT")
next_web=$(push monitorflow-web:e2e-base monitorflow-web "$NEXT")
next_updater=$(push monitorflow-updater:e2e-next monitorflow-updater "$NEXT")
broken_server=$(push monitorflow-server:e2e-broken monitorflow-server "$BROKEN")

# Signed release directories, as the release workflow makes them, with a
# stand-in agent binary for the agent cache.
release() { # version server-image signing-key
    local dir=$work/releases/$1
    mkdir -p "$dir"
    printf 'agent %s\n' "$1" >"$dir/monitorflow-agent-linux-x86_64"
    jq -n --arg v "$1" --arg s "$2" --arg w "$next_web" --arg u "$next_updater" \
        '{version: $v, images: {server: $s, web: $w, updater: $u}}' >"$dir/images.json"
    local keys=$work/keys.pem
    { cat "$repo/deploy/release-keys.pem"; openssl pkey -in "$3" -pubout; } >"$keys"
    MF_SIGNING_KEY=$3 MF_RELEASE_KEYS=$keys MF_RELEASE_VERSION=$1 "$repo/deploy/release-files.sh" "$dir" >/dev/null
}
openssl genpkey -algorithm ed25519 -out "$work/other.pem" 2>/dev/null
mkdir -p "$work/serve"
release "$NEXT" "$next_server" "$key"
release "$BROKEN" "$broken_server" "$key"
release "$UNTRUSTED" "$next_server" "$work/other.pem"
# The release source serves one directory at a time, like a self-hosted copy.
serve() { find "$work/serve" -mindepth 1 -delete; cp "$work/releases/$1"/* "$work/serve/"; }

# The deployment: a copy of deploy/ with its .env, running the base images,
# and a plain HTTP file server as the release source.
cp -r "$repo/deploy" "$work/deploy"
rm -f "$work/deploy/.env"
cat >"$work/deploy/.env" <<EOF
DOMAIN=127.0.0.1
SERVER_NAMES=localhost
POSTGRES_PASSWORD=$(openssl rand -hex 16)
HTTPS_PORT=$HTTPS_PORT
HTTP_PORT=38080
GRPC_PORT=37443
ENROLL_PORT=37445
RELEASE_URL=http://files:8080
SERVER_IMAGE=monitorflow-server:e2e-base
WEB_IMAGE=monitorflow-web:e2e-base
UPDATER_IMAGE=monitorflow-updater:e2e-base
EOF
cat >"$work/deploy/docker-compose.test.yml" <<EOF
services:
  files:
    image: busybox:1.37
    command: ["httpd", "-f", "-p", "8080", "-h", "/srv"]
    volumes: ["$work/serve:/srv:ro"]
    networks: [front, default]
  updater:
    environment:
      MONITORFLOW_UPDATE_TIMEOUT: "120"
EOF
compose up -d --no-build --wait --wait-timeout 600

password=$(openssl rand -hex 16)
compose exec -T server monitorflow-server user add --username admin --role admin <<<"$password" >/dev/null
compose exec -T server monitorflow-server token create --role agent --id ci-update --site kept --create-site >/dev/null
api="https://127.0.0.1:$HTTPS_PORT/api"
curl -fsSk -c "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg p "$password" '{username: "admin", password: $p}')" "$api/auth/login" >/dev/null
get() { curl -fsSk -b "$work/cookies" "$api/$1"; }
post() { curl -sSk -b "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' -o "$work/answer" -w '%{http_code}' -d "$2" "$api/$1"; }
psql() { compose exec -T postgres psql -U monitorflow -d monitorflow -tAc "$1"; }

until_true() { # seconds command...
    local deadline=$((SECONDS + $1))
    shift
    until "$@" >/dev/null 2>&1; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 3
    done
}
updater_alive() { get server-update | jq -e '.updater.alive'; }
check "updater reports to the server" until_true 120 updater_alive

# Asks for an update as the web interface does; waits until it ends and
# prints its final state. Requests fail while the server restarts.
update() {
    serve "$1"
    post server-update/check '' >/dev/null
    jq -e --arg v "$1" '.latest.version == $v and .latest.newer' "$work/answer" >/dev/null \
        || { echo "check did not offer $1: $(cat "$work/answer")" >&2; return 1; }
    [ "$(post server-update "{\"version\": \"$1\"}")" = 202 ] \
        || { echo "update refused: $(cat "$work/answer")" >&2; return 1; }
    local id state deadline=$((SECONDS + 900))
    id=$(jq -r .id "$work/answer")
    while [ "$SECONDS" -lt "$deadline" ]; do
        sleep 5
        state=$(get server-update 2>/dev/null \
            | jq -r --arg id "$id" 'select(.update.id == $id and .update.finished_at != null) | .update.state' 2>/dev/null) || continue
        if [ -n "$state" ]; then echo "$state"; return 0; fi
    done
    echo timeout
}
server_version() { compose exec -T server monitorflow-server --version; }
pinned() { grep -qx "SERVER_IMAGE=$1" "$work/deploy/.env"; }

echo "updating to $NEXT"
state=$(update "$NEXT") || state=error
get server-update | jq '.update' >"$work/next.json" || true
check "update to $NEXT succeeded" test "$state" = succeeded
check "the server runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"
check "the API reports $NEXT" sh -c "curl -fsSk -b '$work/cookies' '$api/server-update' | jq -e '.running_version == \"$NEXT\"'"
check ".env pins the release's images" pinned "$next_server"
check "the session and the data are kept" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
check "the update is audited" test "$(psql "SELECT count(*) FROM audit_log WHERE action = 'server.update'")" = 1
backup=$(jq -r .backup "$work/next.json")
check "a backup was taken before the update" grep -q '^monitorflow-backup-.*-update\.tar\.gz$' <<<"$backup"
get "backups/$backup" >"$work/update-backup.tar.gz" || true
check "it holds the database and the server's CA" \
    sh -c "tar -tzf '$work/update-backup.tar.gz' | grep -qx 'serverdata/ca/ca-key.pem' && tar -tzf '$work/update-backup.tar.gz' | grep -qx database.dump"
check "the backups page lists it" sh -c "curl -fsSk -b '$work/cookies' '$api/backups' | jq -e --arg b '$backup' '.files | map(.name) | index(\$b)'"
check "the agent release is cached" sh -c "docker compose -p $project -f '$work/deploy/docker-compose.yml' -f '$work/deploy/docker-compose.test.yml' exec -T server monitorflow-server agent-release list | grep -qx $NEXT"
check "the release's installer is served for installs" \
    curl -fsSk -o /dev/null "https://127.0.0.1:$HTTPS_PORT/agent/$NEXT/install.sh"
updater_replaced() { [ "$(docker inspect --format '{{.Config.Image}}' "$(compose ps -q updater)")" = "$next_updater" ]; }
check "the updater replaced itself" until_true 120 updater_replaced
check "the new updater reports $NEXT" until_true 120 sh -c "curl -fsSk -b '$work/cookies' '$api/server-update' | jq -e '.updater.alive and .updater.version == \"$NEXT\"'"

check "a downgrade is refused" test "$(post server-update '{"version": "0.0.1"}')" = 400

echo "updating to $BROKEN, whose server does not start"
state=$(update "$BROKEN") || state=error
get server-update | jq '.update' >"$work/broken.json" || true
check "the broken release is rolled back" test "$state" = rolled_back
check "no migration ran, nothing to restore" jq -e '.migrated == false' "$work/broken.json"
check "the server runs $NEXT again" test "$(server_version)" = "monitorflow-server $NEXT"
check ".env pins $NEXT again" pinned "$next_server"
check "the API is ready again" curl -fsSk "$api/health/ready"

# A compromised server can only write requests: one for a release signed
# by another key changes nothing.
serve "$UNTRUSTED"
post server-update/check '' >/dev/null
check "a release signed by another key is not offered" jq -e '.latest == null and (.error | test("does not verify"))' "$work/answer"
compose exec -T server sh -c "printf '%s' '{\"id\": \"forged\", \"version\": \"$UNTRUSTED\", \"requested_by\": \"x\", \"requested_at\": \"2026-10-27T00:00:00Z\"}' >/var/lib/monitorflow/update/inbox/request.json"
forged_refused() { get server-update | jq -e '.update.id == "forged" and .update.state == "failed" and (.update.error | test("does not verify"))'; }
check "a forged request for it fails" until_true 60 forged_refused
check "and the server still runs $NEXT" test "$(server_version)" = "monitorflow-server $NEXT"

echo "backing up and restoring"
backup_now() {
    [ "$(post backups '')" = 202 ] || return 1
    jq -r .id "$work/answer" >"$work/backup-id"
}
check "Back up now is accepted" until_true 120 backup_now
backup_done() {
    get backups | jq -e --arg id "$(cat "$work/backup-id")" '.last.id == $id and .last.state == "succeeded"' >/dev/null
}
check "the backup succeeds" until_true 300 backup_done
name=$(get backups | jq -r .last.file)
get "backups/$name" >"$work/backup.tar.gz" || true
check "it downloads" tar -tzf "$work/backup.tar.gz"
check "the download is audited" test "$(psql "SELECT count(*) FROM audit_log WHERE action = 'server.backup.download'")" -ge 1
ca() { compose exec -T server sha256sum /var/lib/monitorflow/server/ca/ca-key.pem; }
ca_before=$(ca)
compose exec -T server monitorflow-server token create --role agent --id ci-lost --site lost --create-site >/dev/null
compose exec -T server sh -c 'echo stray >/var/lib/monitorflow/server/stray'
COMPOSE_PROJECT_NAME=$project COMPOSE_FILE="$work/deploy/docker-compose.yml:$work/deploy/docker-compose.test.yml" \
    "$work/deploy/restore.sh" "$work/backup.tar.gz" >"$work/restore.log" 2>&1 && restored=yes || restored=no
check "deploy/restore.sh succeeds" test "$restored" = yes
check "data written after the backup is gone" test "$(psql "SELECT count(*) FROM sites WHERE name = 'lost'")" = 0
check "data before it is back" test "$(psql "SELECT count(*) FROM sites WHERE name = 'kept'")" = 1
no_stray() { ! compose exec -T server test -e /var/lib/monitorflow/server/stray; }
check "the server data is the backup's" no_stray
check "the CA is the same" test "$(ca)" = "$ca_before"
check "the API is ready after the restore" curl -fsSk "$api/health/ready"

if [ "$failed" -ne 0 ]; then
    cat "$work/restore.log" 2>/dev/null || true
    cat "$work/next.json" "$work/broken.json" 2>/dev/null || true
    compose logs --tail 80 updater server
    exit 1
fi
echo "server update test passed"
