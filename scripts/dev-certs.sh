#!/usr/bin/env bash
# Throwaway development PKI for local runs. Never use these certificates in
# production; the built-in CA issues the real ones.
#
# Creates in deploy/dev-certs/:
#   ca.pem, ca-key.pem          dev CA
#   server.pem, server-key.pem  server cert, SAN localhost, 127.0.0.1 and
#                               monitorflow://server/dev-server
#   agent.pem, agent-key.pem    client cert, SAN monitorflow://agent/<agent-id>
#
# Identity is the monitorflow:// URI SAN (ADR-0004); the CN is informational.
#
# Usage: scripts/dev-certs.sh [agent-id]
set -euo pipefail
cd "$(dirname "$0")/.."

agent_id="${1:-dev-agent}"
# Same rule as common::tls::validate_id.
if ! [[ "$agent_id" =~ ^[A-Za-z0-9._-]{1,64}$ ]]; then
    echo "error: agent ID must be 1 to 64 characters from A-Z a-z 0-9 . _ -, got '$agent_id'" >&2
    exit 1
fi
out=deploy/dev-certs
days=30
mkdir -p "$out"
umask 077
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

key() { openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1" 2>/dev/null; }

key "$out/ca-key.pem"
openssl req -x509 -new -key "$out/ca-key.pem" -sha256 -days "$days" \
    -subj "/CN=MonitorFlow dev CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign,digitalSignature" \
    -out "$out/ca.pem"

# issue <name> <subject> <extensions>
issue() {
    key "$out/$1-key.pem"
    openssl req -new -key "$out/$1-key.pem" -subj "$2" -out "$tmp/$1.csr"
    printf '%s\n' "$3" > "$tmp/$1.ext"
    openssl x509 -req -in "$tmp/$1.csr" -CA "$out/ca.pem" -CAkey "$out/ca-key.pem" \
        -CAcreateserial -CAserial "$tmp/ca.srl" -sha256 -days "$days" \
        -extfile "$tmp/$1.ext" -out "$out/$1.pem" 2>/dev/null
}

issue server "/CN=localhost" "basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1,URI:monitorflow://server/dev-server"

issue agent "/CN=$agent_id" "basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=clientAuth
subjectAltName=URI:monitorflow://agent/$agent_id"

echo "Dev certificates written to $out/ (valid $days days, agent ID: $agent_id)"
