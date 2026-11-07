#!/usr/bin/env bash
# Makes the key pair that signs MonitorFlow Enterprise license keys
# (ADR-0100). Run it once, on the owner's own machine, never on a server or
# in CI. It is not the release signing key and must not be the same file.
#
#   scripts/license-keypair.sh DIR
#
# Writes DIR/license-signing.pem, the private key, readable by you only,
# and prints the public key to append to ee/license-keys.pem. The private
# key is never printed: keep it offline, with a backup; whoever has it can
# sign licenses. Needs OpenSSL 3.
set -euo pipefail

dir=${1:?usage: scripts/license-keypair.sh DIR}
key=$dir/license-signing.pem
[ ! -e "$key" ] || { echo "$key already exists: not replacing a signing key" >&2; exit 1; }
mkdir -p "$dir"
(umask 077 && openssl genpkey -algorithm ed25519 -out "$key")
echo "private key: $key (keep it offline and backed up; never commit it)" >&2
echo "append this public key to ee/license-keys.pem, and commit that file:" >&2
openssl pkey -in "$key" -pubout
