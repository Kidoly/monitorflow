#!/usr/bin/env bash
# End-to-end test of deploy/install.sh (ADR-0052): starts
# deploy/docker-compose.yml, serves the release files from a local HTTPS
# directory through MF_DOWNLOAD_URL, installs the agent on this host with a
# token from the server CLI, and checks that it enrolls and shows online in
# the API. Then re-runs the installer, checks a tampered checksum fails
# closed, and uninstalls. Changes this host's /usr/bin, /etc and systemd:
# meant for a throwaway CI runner. Needs root through sudo, systemd,
# docker with compose, curl, jq, openssl and python3.
#   deploy/install-test.sh path/to/monitorflow-agent-linux-x86_64
set -euo pipefail
agent=$(realpath "${1:?usage: deploy/install-test.sh AGENT_BINARY}")
cd "$(dirname "$0")"

export DOMAIN=127.0.0.1 SERVER_NAMES=localhost
POSTGRES_PASSWORD=$(openssl rand -hex 16)
export POSTGRES_PASSWORD
export HTTPS_PORT=28443 HTTP_PORT=28080 GRPC_PORT=27443 ENROLL_PORT=27445
FILES_PORT=28444
PEER=ci-install-$RANDOM
work=$(mktemp -d)

compose() { docker compose -p monitorflow-install --env-file /dev/null -f docker-compose.yml "$@"; }
cleanup() {
    [ -z "${files_pid:-}" ] || kill "$files_pid" 2>/dev/null || true
    sudo MF_UNINSTALL=1 MF_PURGE=1 sh install.sh >/dev/null 2>&1 || true
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

# The release directory, as the release workflow builds it, plus a copy
# whose checksums do not match.
mkdir -p "$work/files" "$work/files/tampered"
install -m 0755 "$agent" "$work/files/monitorflow-agent-linux-x86_64"
./release-files.sh "$work/files" >/dev/null
find "$work/files" -maxdepth 1 -type f -exec cp {} "$work/files/tampered/" \;
awk -v zero="$(printf '%064d' 0)" '$2 == "monitorflow-agent-linux-x86_64" { $1 = zero } 1' \
    "$work/files/SHA256SUMS" >"$work/files/tampered/SHA256SUMS"

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

echo "== tampered checksums"
binary=$(sha256sum /usr/bin/monitorflow-agent)
check "installer refuses a checksum mismatch" bash -c "! curl -fsSL --cacert '$work/cert.pem' '$base/install.sh' \
    | sudo env CURL_CA_BUNDLE='$work/cert.pem' MF_DOWNLOAD_URL='$base/tampered' sh"
check "binary unchanged" test "$(sha256sum /usr/bin/monitorflow-agent)" = "$binary"

echo "== uninstall"
installer MF_UNINSTALL=1
check "binary, unit and sudo rules removed" bash -c "! ls /usr/bin/monitorflow-agent /etc/systemd/system/monitorflow-agent.service /etc/sudoers.d/monitorflow-agent"
check "configuration and identity kept" sudo test -s /var/lib/monitorflow/agent/tls/cert.pem -a -s /etc/monitorflow/agent.toml
installer MF_UNINSTALL=1 MF_PURGE=1
check "purge removes data and user" bash -c "! sudo test -e /var/lib/monitorflow/agent && ! id monitorflow"

if [ "$failed" -ne 0 ]; then
    sudo journalctl -u monitorflow-agent -n 50 --no-pager || true
    compose logs --tail 50 server
    exit 1
fi
echo "install test passed"
