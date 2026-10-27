#!/usr/bin/env bash
# End-to-end test of deploy/install.sh (ADR-0052): starts
# deploy/docker-compose.yml, serves the release files from a local HTTPS
# directory through MF_DOWNLOAD_URL, installs the agent on this host with a
# token from the server CLI, and checks that it enrolls and shows online in
# the API. Then re-runs the installer, checks that a tampered checksum, a
# signature by another key and a missing signature fail closed (ADR-0053),
# and uninstalls. The release is signed with a throwaway key.
#
# With a second agent binary, built with the same release keys and a newer
# version (MONITORFLOW_BUILD_VERSION), it also tests self-update (ADR-0053):
# the first agent updates to the second through the API, keeping its
# identity, then a broken release (/bin/false, signed) is rolled back.
# MF_TEST_SIGNING_KEY must then be the key whose public half both agents
# were built with (MONITORFLOW_RELEASE_KEYS).
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
work=$(mktemp -d)

compose() { docker compose -p monitorflow-install --env-file /dev/null -f docker-compose.yml "$@"; }
cleanup() {
    [ -z "${files_pid:-}" ] || kill "$files_pid" 2>/dev/null || true
    sudo MF_UNINSTALL=1 MF_PURGE=1 sh install.sh >/dev/null 2>&1 || true
    sudo rm -rf /etc/systemd/system/monitorflow-agent.service.d /etc/monitorflow/e2e-ca.pem
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
mkdir -p "$work/files"
install -m 0755 "$agent" "$work/files/monitorflow-agent-linux-x86_64"
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
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=localhost \
    -addext subjectAltName=DNS:localhost -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
python3 - "$work/files" "$FILES_PORT" "$work/cert.pem" "$work/key.pem" <<'EOF' &
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
    curl -fsSL --cacert "$work/cert.pem" "$base/install.sh" \
        | sudo env CURL_CA_BUNDLE="$work/cert.pem" "$@" sh
}
api="https://127.0.0.1:$HTTPS_PORT/api"
curl -fsSk -c "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
    -d "$(jq -n --arg p "$password" '{username: "admin", password: $p}')" "$api/auth/login" >/dev/null
online() {
    for _ in $(seq 60); do
        if curl -fsSk -b "$work/cookies" "$api/servers" \
            | jq -e --arg peer "$PEER" '.servers[] | select(.peer_id == $peer and .status == "online")' >/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

echo "== first install"
installer MF_DOWNLOAD_URL="$base" MF_SERVER="localhost:$GRPC_PORT" MF_ENROLL="localhost:$ENROLL_PORT" \
    MF_TOKEN="$token" | tee "$work/install.log"
check "agent shows online in the API" online
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
check "agent online again" online

if [ -n "$newer" ]; then
    echo "== self-update"
    # The agent trusts the file server's throwaway CA for its downloads.
    sudo install -m 0644 "$work/cert.pem" /etc/monitorflow/e2e-ca.pem
    sudo mkdir -p /etc/systemd/system/monitorflow-agent.service.d
    printf '[Service]\nEnvironment=SSL_CERT_FILE=/etc/monitorflow/e2e-ca.pem\n' \
        | sudo tee /etc/systemd/system/monitorflow-agent.service.d/e2e.conf >/dev/null
    sudo systemctl daemon-reload
    sudo systemctl restart monitorflow-agent
    online
    old_version=$(/usr/bin/monitorflow-agent --version | cut -d ' ' -f 2)
    new_version=$("$newer" --version | cut -d ' ' -f 2)
    # Fills $work/files/next with a signed release of $1 as version $2.
    publish() {
        rm -rf "$work/files/next"
        mkdir "$work/files/next"
        install -m 0755 "$1" "$work/files/next/monitorflow-agent-linux-x86_64"
        MF_SIGNING_KEY=$work/signing.pem MF_RELEASE_VERSION=$2 ./release-files.sh "$work/files/next" >/dev/null
    }
    server_id=$(curl -fsSk -b "$work/cookies" "$api/servers" | jq -r --arg peer "$PEER" '.servers[] | select(.peer_id == $peer) | .id')
    # Queues a self_update to $1 and prints the command's final JSON.
    update_to() {
        local id state
        id=$(curl -fsSk -b "$work/cookies" -H 'content-type: application/json' -H 'x-monitorflow-csrf: 1' \
            -d "$(jq -n --arg v "$1" '{kind: "self_update", version: $v, health_timeout_s: 30}')" \
            "$api/servers/$server_id/commands" | jq -r .id)
        for _ in $(seq 180); do
            curl -fsSk -b "$work/cookies" "$api/commands/$id" >"$work/command.json" || true
            state=$(jq -r .state "$work/command.json" 2>/dev/null || true)
            case $state in succeeded | failed | expired | cancelled) break ;; esac
            sleep 1
        done
        cat "$work/command.json"
    }
    state_is() { [ "$(jq -r .state "$work/update.json")" = "$1" ]; }
    reports_version() {
        for _ in $(seq 60); do
            if curl -fsSk -b "$work/cookies" "$api/servers/$server_id" \
                | jq -e --arg v "$1" '.agent_version == $v and .status == "online"' >/dev/null; then
                return 0
            fi
            sleep 1
        done
        return 1
    }

    publish "$newer" "$new_version"
    update_to "$new_version" | tee "$work/update.json"
    check "self-update to $new_version succeeded" state_is succeeded
    check "binary is $new_version" test "$(/usr/bin/monitorflow-agent --version)" = "monitorflow-agent $new_version"
    check "previous binary kept" sudo test "$(sudo /var/lib/monitorflow/self-update/previous --version)" = "monitorflow-agent $old_version"
    check "binary root:root 0755 after update" mode_is /usr/bin/monitorflow-agent "root:root 755"
    check "identity and configuration kept" test "$(sudo sha256sum /var/lib/monitorflow/agent/tls/cert.pem /etc/monitorflow/agent.toml)" = "$identity"
    check "API reports $new_version online" reports_version "$new_version"

    # A signed release that cannot start: the watchdog puts $new_version back.
    publish /bin/false 99.9.9
    update_to 99.9.9 | tee "$work/update.json"
    check "broken update failed" state_is failed
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
    ! curl -fsSL --cacert "$work/cert.pem" "$base/install.sh" \
        | sudo env CURL_CA_BUNDLE="$work/cert.pem" MF_DOWNLOAD_URL="$base/$1" sh
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

if [ "$failed" -ne 0 ]; then
    sudo journalctl -u monitorflow-agent -n 50 --no-pager || true
    sudo journalctl -u monitorflow-agent-self-update -n 50 --no-pager || true
    compose logs --tail 50 server
    exit 1
fi
echo "install test passed"
