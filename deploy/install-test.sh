#!/usr/bin/env bash
# End-to-end test of deploy/install.sh (ADR-0052): starts
# deploy/docker-compose.yml, serves the release files from a local HTTPS
# directory through MF_DOWNLOAD_URL, installs the agent on this host with a
# token from the server CLI, and checks that it enrolls and shows online in
# the API. Then re-runs the installer, checks that a tampered checksum, a
# signature by another key and a missing signature fail closed (ADR-0053),
# and uninstalls. The release is signed with a throwaway key, which the
# server image is built to trust too.
#
# Then the `central` source (ADR-0054): the release is imported into the
# server's cache offline (a container without network), and the agent is
# installed again with the command the token page builds, from the
# stack's own HTTPS address, whose certificate this host does not trust.
#
# With a second agent binary, built with the same release keys and a newer
# version (MONITORFLOW_BUILD_VERSION), it also tests automatic updates and
# self-update (ADR-0053, ADR-0055): once the server caches the newer
# release, it updates the agent on its own, without any request, keeping
# its identity; then a broken release (/bin/false, signed) sent through
# the API is rolled back. The server is built with the newer version, as
# automatic updates never move a peer past the server.
# MF_TEST_SIGNING_KEY must then be the key whose public half the agents
# were built with (MONITORFLOW_RELEASE_KEYS); the server image gets it as
# a build argument.
#
# With MF_TEST_RELAY, the path of a static relay binary, it first installs
# a relay with MF_ROLE=relay (ADR-0055), checks that it enrolls and shows
# online and upgrades it in place, and the agent connects through it. With
# a newer agent, MF_TEST_NEWER_RELAY must be a relay of the same newer
# version: the relay updates to it through the API while its agent stays
# connected, then a broken relay release is rolled back.
#
# Changes this host's /usr/bin, /etc and systemd: meant for a throwaway CI
# runner. Needs root through sudo, systemd, docker with compose, curl, jq,
# openssl and python3.
#   deploy/install-test.sh AGENT_BINARY [NEWER_AGENT_BINARY]
set -euo pipefail
agent=$(realpath "${1:?usage: deploy/install-test.sh AGENT_BINARY [NEWER_AGENT_BINARY]}")
newer=${2:+$(realpath "$2")}
cd "$(dirname "$0")"

export DOMAIN=127.0.0.1 SERVER_NAMES=localhost
POSTGRES_PASSWORD=$(openssl rand -hex 16)
export POSTGRES_PASSWORD
export HTTPS_PORT=28443 HTTP_PORT=28080 GRPC_PORT=27443 ENROLL_PORT=27445
FILES_PORT=28444
# Self-updates download from here; the directory is filled later.
export AGENT_DOWNLOAD_URL=https://localhost:$FILES_PORT/next
PEER=ci-install-$RANDOM
RELAY=ci-relay-$RANDOM
relay=${MF_TEST_RELAY:+$(realpath "$MF_TEST_RELAY")}
newer_relay=${MF_TEST_NEWER_RELAY:+$(realpath "$MF_TEST_NEWER_RELAY")}
if [ -n "$relay" ] && [ -n "$newer" ] && [ -z "$newer_relay" ]; then
    echo "MF_TEST_NEWER_RELAY is needed with a relay and a newer agent" >&2
    exit 1
fi
new_version=
[ -z "$newer" ] || new_version=$("$newer" --version | cut -d ' ' -f 2)
export MF_TEST_SERVER_VERSION=$new_version
work=$(mktemp -d)

# The server image trusts the throwaway release key, so it imports the
# test release; install-test.compose.yml sets its version and polling.
compose() {
    docker compose -p monitorflow-install --env-file /dev/null -f docker-compose.yml \
        -f install-test.compose.yml -f "$work/release-keys.compose.yml" "$@"
}
cleanup() {
    [ -z "${files_pid:-}" ] || kill "$files_pid" 2>/dev/null || true
    sudo MF_UNINSTALL=1 MF_PURGE=1 sh install.sh >/dev/null 2>&1 || true
    sudo MF_ROLE=relay MF_UNINSTALL=1 MF_PURGE=1 sh install.sh >/dev/null 2>&1 || true
    sudo rm -rf /etc/systemd/system/monitorflow-agent.service.d /etc/systemd/system/monitorflow-relay.service.d \
        /etc/monitorflow/e2e-ca.pem
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

failed=0
check() {
    printf '%-50s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}
mode_is() { [ "$(sudo stat -c '%U:%G %a' "$1")" = "$2" ]; }
absent() { ! sudo test -e "$1"; }

# The release directory, as the release workflow builds it but signed with
# a throwaway key, plus copies with a checksum that does not match (signed
# again, so only the checksum is wrong), a signature by another key, and
# no signature.
if [ -n "${MF_TEST_SIGNING_KEY:-}" ]; then
    cp "$MF_TEST_SIGNING_KEY" "$work/signing.pem"
else
    [ -z "$newer" ] || { echo "MF_TEST_SIGNING_KEY is needed with a newer agent" >&2; exit 1; }
    openssl genpkey -algorithm ed25519 -out "$work/signing.pem"
fi
openssl genpkey -algorithm ed25519 -out "$work/other.pem"
{ cat release-keys.pem; openssl pkey -in "$work/signing.pem" -pubout; } >"$work/release-keys.pem"
export MF_RELEASE_KEYS=$work/release-keys.pem
MF_E2E_RELEASE_KEYS=$(cat "$work/release-keys.pem")
export MF_E2E_RELEASE_KEYS
cat >"$work/release-keys.compose.yml" <<'YAML'
services:
  db-certs:
    build:
      args: { RELEASE_KEYS: "${MF_E2E_RELEASE_KEYS}" }
  server:
    build:
      args: { RELEASE_KEYS: "${MF_E2E_RELEASE_KEYS}" }
YAML
mkdir -p "$work/files"
install -m 0755 "$agent" "$work/files/monitorflow-agent-linux-x86_64"
[ -z "$relay" ] || install -m 0755 "$relay" "$work/files/monitorflow-relay-linux-x86_64"
MF_SIGNING_KEY=$work/signing.pem ./release-files.sh "$work/files" >/dev/null
for copy in tampered foreign unsigned; do
    mkdir "$work/files/$copy"
    find "$work/files" -maxdepth 1 -type f -exec cp {} "$work/files/$copy/" \;
done
awk -v zero="$(printf '%064d' 0)" '$2 == "monitorflow-agent-linux-x86_64" { $1 = zero } 1' \
    "$work/files/SHA256SUMS" >"$work/files/tampered/SHA256SUMS"
openssl pkeyutl -sign -inkey "$work/signing.pem" -rawin -in "$work/files/tampered/SHA256SUMS" \
    -out "$work/files/tampered/SHA256SUMS.sig"
openssl pkeyutl -sign -inkey "$work/other.pem" -rawin -in "$work/files/SHA256SUMS" \
    -out "$work/files/foreign/SHA256SUMS.sig"
rm "$work/files/unsigned/SHA256SUMS.sig"

# HTTPS with a CA of its own, trusted by the installer's curl through
# CURL_CA_BUNDLE.
# A CA and a server certificate it signs: rustls, in the agent, refuses a
# CA certificate as the server's own.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 \
    -subj "/CN=MonitorFlow install test CA" -keyout "$work/ca-key.pem" -out "$work/ca.pem" 2>/dev/null
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=localhost \
    -keyout "$work/key.pem" -out "$work/server.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' >"$work/server.ext"
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.pem" -CAkey "$work/ca-key.pem" -CAcreateserial \
    -days 1 -extfile "$work/server.ext" -out "$work/server.pem" 2>/dev/null
python3 - "$work/files" "$FILES_PORT" "$work/server.pem" "$work/key.pem" <<'EOF' &
import functools, http.server, ssl, sys
directory, port, cert, key = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory)
server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()
EOF
files_pid=$!
base=https://localhost:$FILES_PORT

compose up -d --build --wait --wait-timeout 600
password=$(openssl rand -hex 16)
compose exec -T server monitorflow-server user add --username admin --role admin <<<"$password" >/dev/null
token=$(compose exec -T server monitorflow-server token create --role agent --id "$PEER" --site ci --create-site)

# Runs the installer as the one-liner does, piped from the file server.
installer() {
    curl -fsSL --cacert "$work/ca.pem" "$base/install.sh" \
        | sudo env CURL_CA_BUNDLE="$work/ca.pem" "$@" sh
}
api="https://127.0.0.1:$HTTPS_PORT/api"
curl -fsSk -c "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg p "$password" '{username: "admin", password: $p}')" "$api/auth/login" >/dev/null
# Whether the agent $1 ($PEER by default) shows online.
online() {
    for _ in $(seq 60); do
        if curl -fsSk -b "$work/cookies" "$api/servers" \
            | jq -e --arg peer "${1:-$PEER}" '.servers[] | select(.peer_id == $peer and .status == "online")' >/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}
relay_online() {
    for _ in $(seq 60); do
        if curl -fsSk -b "$work/cookies" "$api/relays" \
            | jq -e --arg peer "$RELAY" '.relays[] | select(.peer_id == $peer and .status == "online")' >/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# The agent's upstream: the relay when there is one (ADR-0025).
upstream=localhost:$GRPC_PORT
if [ -n "$relay" ]; then
    echo "== relay install"
    upstream=localhost:7443
    relay_token=$(compose exec -T server monitorflow-server token create --role relay --id "$RELAY" \
        --site ci --server-name localhost)
    installer MF_ROLE=relay MF_DOWNLOAD_URL="$base" MF_SERVER="localhost:$GRPC_PORT" MF_ENROLL="localhost:$ENROLL_PORT" \
        MF_TOKEN="$relay_token" | tee "$work/relay.log"
    check "relay shows online in the API" relay_online
    check "installer verified the release signature" grep -q 'release signature verified' "$work/relay.log"
    check "relay binary root:root 0755" mode_is /usr/bin/monitorflow-relay "root:root 755"
    check "relay data dir monitorflow-relay 0700" mode_is /var/lib/monitorflow/relay "monitorflow-relay:monitorflow-relay 700"
    check "relay.toml root:monitorflow-relay 0640" mode_is /etc/monitorflow/relay.toml "root:monitorflow-relay 640"
    check "relay sudoers root 0440" mode_is /etc/sudoers.d/monitorflow-relay "root:root 440"
    check "relay runs as monitorflow-relay" test "$(systemctl show -p User --value monitorflow-relay)" = monitorflow-relay
    check "relay token file deleted" absent /etc/monitorflow/relay-enroll.env
    check "relay token not in the journal" bash -c "! sudo journalctl -u monitorflow-relay --no-pager | grep -qF -- '$relay_token'"
    relay_identity=$(sudo sha256sum /var/lib/monitorflow/relay/tls/cert.pem /etc/monitorflow/relay.toml)
    installer MF_ROLE=relay MF_DOWNLOAD_URL="$base"
    check "relay identity and configuration kept" test "$(sudo sha256sum /var/lib/monitorflow/relay/tls/cert.pem /etc/monitorflow/relay.toml)" = "$relay_identity"
    check "relay online again" relay_online
    relay_id=$(curl -fsSk -b "$work/cookies" "$api/relays" | jq -r --arg peer "$RELAY" '.relays[] | select(.peer_id == $peer) | .id')
fi

echo "== first install"
installer MF_DOWNLOAD_URL="$base" MF_SERVER="$upstream" MF_ENROLL="localhost:$ENROLL_PORT" \
    MF_TOKEN="$token" | tee "$work/install.log"
check "agent shows online in the API" online "$PEER"
check "installer verified the release signature" grep -q 'release signature verified' "$work/install.log"
check "binary root:root 0755" mode_is /usr/bin/monitorflow-agent "root:root 755"
check "data dir monitorflow 0700" mode_is /var/lib/monitorflow/agent "monitorflow:monitorflow 700"
check "agent.toml root:monitorflow 0640" mode_is /etc/monitorflow/agent.toml "root:monitorflow 640"
check "sudoers root 0440" mode_is /etc/sudoers.d/monitorflow-agent "root:root 440"
check "service runs as monitorflow" test "$(systemctl show -p User --value monitorflow-agent)" = monitorflow
check "token file deleted after enrollment" absent /etc/monitorflow/agent-enroll.env
check "token not in agent.toml" bash -c "! sudo grep -qF -- '$token' /etc/monitorflow/agent.toml"
check "token not in the installer output" bash -c "! grep -qF -- '$token' '$work/install.log'"
check "token not in the journal" bash -c "! sudo journalctl -u monitorflow-agent --no-pager | grep -qF -- '$token'"
identity=$(sudo sha256sum /var/lib/monitorflow/agent/tls/cert.pem /etc/monitorflow/agent.toml)

echo "== second run: upgrade in place"
installer MF_DOWNLOAD_URL="$base" MF_TOKEN="$token"
check "identity and configuration kept" test "$(sudo sha256sum /var/lib/monitorflow/agent/tls/cert.pem /etc/monitorflow/agent.toml)" = "$identity"
check "service active after upgrade" systemctl is-active --quiet monitorflow-agent
check "agent online again" online "$PEER"

if [ -n "$newer" ]; then
    echo "== self-update"
    # The agent and relay trust the file server's throwaway CA for their
    # downloads.
    sudo install -m 0644 "$work/ca.pem" /etc/monitorflow/e2e-ca.pem
    trust_files() {
        sudo mkdir -p "/etc/systemd/system/$1.service.d"
        printf '[Service]\nEnvironment=SSL_CERT_FILE=/etc/monitorflow/e2e-ca.pem\n' \
            | sudo tee "/etc/systemd/system/$1.service.d/e2e.conf" >/dev/null
        sudo systemctl daemon-reload
        sudo systemctl restart "$1"
    }
    [ -z "$relay" ] || { trust_files monitorflow-relay; relay_online; }
    trust_files monitorflow-agent
    online "$PEER"
    old_version=$(/usr/bin/monitorflow-agent --version | cut -d ' ' -f 2)
    # Fills $work/files/next with a signed release of version $3 holding
    # the agent $1 and the relay $2, either of them empty.
    publish() {
        rm -rf "$work/files/next"
        mkdir "$work/files/next"
        [ -z "$1" ] || install -m 0755 "$1" "$work/files/next/monitorflow-agent-linux-x86_64"
        [ -z "$2" ] || install -m 0755 "$2" "$work/files/next/monitorflow-relay-linux-x86_64"
        MF_SIGNING_KEY=$work/signing.pem MF_RELEASE_VERSION=$3 ./release-files.sh "$work/files/next" >/dev/null
    }
    server_id=$(curl -fsSk -b "$work/cookies" "$api/servers" | jq -r --arg peer "$PEER" '.servers[] | select(.peer_id == $peer) | .id')
    # Polls $1 until its state is final and prints its JSON.
    final() {
        local state
        for _ in $(seq 180); do
            curl -fsSk -b "$work/cookies" "$1" >"$work/command.json" || true
            state=$(jq -r .state "$work/command.json" 2>/dev/null || true)
            case $state in succeeded | failed | expired | cancelled) break ;; esac
            sleep 1
        done
        cat "$work/command.json"
    }
    post() {
        curl -fsSk -b "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' -d "$2" "$api/$1"
    }
    # Queues a self_update of the agent to $1 and prints its final JSON.
    update_to() {
        local id
        id=$(post "servers/$server_id/commands" "$(jq -n --arg v "$1" '{kind: "self_update", version: $v, health_timeout_s: 30}')" | jq -r .id)
        final "$api/commands/$id"
    }
    # The relay's last self-update once final, from its page.
    relay_update_to() {
        post "relays/$relay_id/update" "$(jq -n --arg v "$1" '{version: $v, health_timeout_s: 30}')" >/dev/null
        for _ in $(seq 180); do
            curl -fsSk -b "$work/cookies" "$api/relays/$relay_id" | jq .last_update >"$work/relay-update.json" || true
            case $(jq -r .state "$work/relay-update.json" 2>/dev/null) in succeeded | failed | expired | cancelled) break ;; esac
            sleep 1
        done
        cat "$work/relay-update.json"
    }
    state_is() { [ "$(jq -r .state "$1")" = "$2" ]; }
    reports_version() {
        for _ in $(seq "${2:-60}"); do
            if curl -fsSk -b "$work/cookies" "$api/servers/$server_id" \
                | jq -e --arg v "$1" '.agent_version == $v and .status == "online"' >/dev/null; then
                return 0
            fi
            sleep 1
        done
        return 1
    }
    relay_reports() {
        for _ in $(seq 60); do
            if curl -fsSk -b "$work/cookies" "$api/relays/$relay_id" \
                | jq -e --arg v "$1" '.relay_version == $v and .status == "online"' >/dev/null; then
                return 0
            fi
            sleep 1
        done
        return 1
    }

    if [ -n "$relay" ]; then
        echo "== relay self-update"
        old_relay=$(/usr/bin/monitorflow-relay --version | cut -d ' ' -f 2)
        publish "" "$newer_relay" "$new_version"
        relay_update_to "$new_version" | tee "$work/relay-update.json"
        check "relay self-update to $new_version succeeded" state_is "$work/relay-update.json" succeeded
        check "relay binary is $new_version" test "$(/usr/bin/monitorflow-relay --version)" = "monitorflow-relay $new_version"
        check "previous relay binary kept" sudo test "$(sudo /var/lib/monitorflow/relay-self-update/previous --version)" = "monitorflow-relay $old_relay"
        check "relay binary root:root 0755 after update" mode_is /usr/bin/monitorflow-relay "root:root 755"
        check "relay identity and configuration kept" test "$(sudo sha256sum /var/lib/monitorflow/relay/tls/cert.pem /etc/monitorflow/relay.toml)" = "$relay_identity"
        check "API reports relay $new_version online" relay_reports "$new_version"
        check "agent back online through the relay" online

        # A signed relay release that cannot start: rolled back.
        publish "" /bin/false 99.9.9
        relay_update_to 99.9.9 | tee "$work/relay-update.json"
        check "broken relay update failed" state_is "$work/relay-update.json" failed
        check "relay error says rolled back" bash -c "jq -r .error '$work/relay-update.json' | grep -q '^rolled back to $new_version'"
        check "relay binary is $new_version again" test "$(/usr/bin/monitorflow-relay --version)" = "monitorflow-relay $new_version"
        check "API reports relay $new_version online again" relay_reports "$new_version"
        check "agent online through the relay again" online
        sudo journalctl -u monitorflow-relay-self-update --no-pager -n 20 || true
    fi

    echo "== automatic update"
    # The server caches the newer release; nothing else asks for it.
    publish "$newer" "$newer_relay" "$new_version"
    compose cp "$work/files/next" server:/tmp/release
    check "server caches $new_version" compose exec -T server monitorflow-server agent-release import --dir /tmp/release
    check "agent updated itself to $new_version" reports_version "$new_version" 240
    check "binary is $new_version" test "$(/usr/bin/monitorflow-agent --version)" = "monitorflow-agent $new_version"
    check "previous binary kept" sudo test "$(sudo /var/lib/monitorflow/self-update/previous --version)" = "monitorflow-agent $old_version"
    check "binary root:root 0755 after update" mode_is /usr/bin/monitorflow-agent "root:root 755"
    check "identity and configuration kept" test "$(sudo sha256sum /var/lib/monitorflow/agent/tls/cert.pem /etc/monitorflow/agent.toml)" = "$identity"
    # The update's result comes once the new agent answers the command.
    rollout_done() {
        for _ in $(seq 60); do
            curl -fsSk -b "$work/cookies" "$api/auto-update" >"$work/auto-update.json" || true
            # shellcheck disable=SC2016 # $v is jq's
            if jq -e --arg v "$new_version" '.agents.rollout.version == $v and .agents.rollout.state == "rolling"
                    and .agents.totals.succeeded >= 1 and .agents.totals.waiting == 0' "$work/auto-update.json" >/dev/null; then
                return 0
            fi
            sleep 1
        done
        cat "$work/auto-update.json" >&2
        return 1
    }
    check "rollout of $new_version recorded, nothing waiting" rollout_done

    # A signed release that cannot start: the watchdog puts $new_version back.
    publish /bin/false "" 99.9.9
    update_to 99.9.9 | tee "$work/update.json"
    check "broken update failed" state_is "$work/update.json" failed
    check "error says rolled back" bash -c "jq -r .error '$work/update.json' | grep -q '^rolled back to $new_version'"
    check "binary is $new_version again" test "$(/usr/bin/monitorflow-agent --version)" = "monitorflow-agent $new_version"
    check "API reports $new_version online again" reports_version "$new_version"
    sudo journalctl -u monitorflow-agent-self-update --no-pager -n 20 || true
fi

echo "== tampered files"
binary=$(sha256sum /usr/bin/monitorflow-agent)
# The installer from the good directory, the files from another one, as
# with a compromised mirror.
refuses() {
    ! curl -fsSL --cacert "$work/ca.pem" "$base/install.sh" \
        | sudo env CURL_CA_BUNDLE="$work/ca.pem" MF_DOWNLOAD_URL="$base/$1" sh
}
check "installer refuses a checksum mismatch" refuses tampered
check "installer refuses another key's signature" refuses foreign
check "installer refuses a missing signature" refuses unsigned
check "binary unchanged" test "$(sha256sum /usr/bin/monitorflow-agent)" = "$binary"

echo "== uninstall"
installer MF_UNINSTALL=1
check "binary, unit and sudo rules removed" bash -c "! ls /usr/bin/monitorflow-agent /etc/systemd/system/monitorflow-agent.service /etc/sudoers.d/monitorflow-agent"
check "configuration and identity kept" sudo test -s /var/lib/monitorflow/agent/tls/cert.pem -a -s /etc/monitorflow/agent.toml
installer MF_UNINSTALL=1 MF_PURGE=1
check "purge removes data and user" bash -c "! sudo test -e /var/lib/monitorflow/agent && ! id monitorflow"

echo "== central source"
web="https://127.0.0.1:$HTTPS_PORT"
# Nothing cached yet, unless the automatic update above imported a
# release: the files route and the API say so.
empty_cache() {
    [ "$(curl -sk -o "$work/empty.txt" -w '%{http_code}' "$web/agent/latest/install.sh")" = 404 ] \
        && grep -q 'agent-release import' "$work/empty.txt"
}
if [ -z "$newer" ]; then
    check "empty cache answers 404 with a reason" empty_cache
    check "API offers no central release yet" bash -c \
        "curl -fsSk -b '$work/cookies' '$api/settings/agent-install' | jq -e '.central == null'"
fi
# The release as downloaded from GitHub, imported without any network.
mkdir -p "$work/release"
find "$work/files" -maxdepth 1 -type f -exec cp {} "$work/release/" \;
chmod 0711 "$work"
chmod -R a+rX "$work/release"
docker run --rm --network none -v monitorflow-install_serverdata:/var/lib/monitorflow/server \
    -v "$work/release:/release:ro" monitorflow-server:local agent-release import --dir /release \
    | tee "$work/import.log"
check "offline import kept the release" grep -q '^imported ' "$work/import.log"
check "latest install.sh served over HTTPS" bash -c \
    "curl -fsSk '$web/agent/latest/install.sh' | cmp -s - '$work/release/install.sh'"
curl -fsSk -b "$work/cookies" -X PUT -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d '{"source": "central"}' "$api/settings/agent-install" >/dev/null
site_id=$(curl -fsSk -b "$work/cookies" "$api/sites" | jq -r '.sites[] | select(.name == "ci") | .id')
curl -fsSk -b "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg site "$site_id" --arg peer "$PEER-central" '{role: "agent", peer_id: $peer, site_id: $site}')" \
    "$api/tokens" >"$work/central-token.json"
check "new token defaults to central" jq -e '.install.source == "central"' "$work/central-token.json"
path=$(jq -r .install.central.path "$work/central-token.json")
sums=$(jq -r .install.central.sums_sha256 "$work/central-token.json")
installer_sha=$(jq -r .install.central.installer_sha256 "$work/central-token.json")
central_token=$(jq -r .token "$work/central-token.json")
# The token page's command (web/src/lib/install.ts), without CURL_CA_BUNDLE:
# this host does not trust Caddy's certificate.
central_install() {
    local f
    f=$(mktemp) && curl -fsSLk -o "$f" "$web$path/install.sh" \
        && echo "$installer_sha  $f" | sha256sum -c \
        && sudo MF_DOWNLOAD_URL="$web$path" MF_SUMS_SHA256="$1" MF_SERVER="localhost:$GRPC_PORT" \
            MF_ENROLL="localhost:$ENROLL_PORT" MF_TOKEN="$central_token" sh "$f"
    local status=$?
    rm -f "$f"
    return $status
}
check "web certificate is not trusted here" bash -c "! curl -fsS '$web/agent/latest/VERSION'"
refuses_other_sums() { ! central_install "$(printf '%064d' 0)"; }
check "central refuses other SHA256SUMS" refuses_other_sums
check "nothing installed after the refusal" absent /usr/bin/monitorflow-agent
central_install "$sums" | tee "$work/central.log"
check "central agent shows online" online "$PEER-central"
check "central installer verified the signature" grep -q 'release signature verified' "$work/central.log"
check "central binary root:root 0755" mode_is /usr/bin/monitorflow-agent "root:root 755"
check "central token file deleted" absent /etc/monitorflow/agent-enroll.env
installer MF_UNINSTALL=1 MF_PURGE=1

if [ -n "$relay" ]; then
    installer MF_ROLE=relay MF_UNINSTALL=1
    check "relay binary, unit and sudo rule removed" bash -c "! ls /usr/bin/monitorflow-relay /etc/systemd/system/monitorflow-relay.service /etc/sudoers.d/monitorflow-relay"
    check "relay configuration and identity kept" sudo test -s /var/lib/monitorflow/relay/tls/cert.pem -a -s /etc/monitorflow/relay.toml
    installer MF_ROLE=relay MF_UNINSTALL=1 MF_PURGE=1
    check "purge removes relay data and user" bash -c "! sudo test -e /var/lib/monitorflow/relay && ! id monitorflow-relay"
fi

if [ "$failed" -ne 0 ]; then
    sudo journalctl -u monitorflow-relay -n 50 --no-pager || true
    sudo journalctl -u monitorflow-relay-self-update -n 50 --no-pager || true
    sudo journalctl -u monitorflow-agent -n 50 --no-pager || true
    sudo journalctl -u monitorflow-agent-self-update -n 50 --no-pager || true
    compose logs --tail 50 server
    exit 1
fi
echo "install test passed"
