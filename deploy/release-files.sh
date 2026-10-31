#!/usr/bin/env bash
# Completes a directory of release files (ADR-0052, ADR-0053, ADR-0055,
# ADR-0065): adds the agent and server installers with the release keys
# filled in, what the server installer deploys, the systemd units, the sudo
# rules and VERSION next to the agent and relay binaries already in it,
# writes SHA256SUMS over everything and signs it into SHA256SUMS.sig. The release
# workflow publishes the result; deploy/install-test.sh serves it to the
# installer.
#   MF_SIGNING_KEY=key.pem deploy/release-files.sh DIR
# Environment:
#   MF_SIGNING_KEY      Ed25519 private key (PEM) that signs SHA256SUMS;
#                       its public half must be in MF_RELEASE_KEYS.
#   MF_RELEASE_KEYS     public keys for the installer; deploy/release-keys.pem
#                       by default, the file the binaries were built with.
#   MF_RELEASE_VERSION  the version in VERSION; Cargo.toml's by default.
set -euo pipefail
dir=${1:?usage: MF_SIGNING_KEY=key.pem deploy/release-files.sh DIR}
deploy=$(cd "$(dirname "$0")" && pwd)
key=${MF_SIGNING_KEY:?MF_SIGNING_KEY must name the release signing key}
keys=${MF_RELEASE_KEYS:-$deploy/release-keys.pem}
version=${MF_RELEASE_VERSION:-$(awk -F '"' '/^\[workspace.package\]/ { ws = 1 } ws && /^version = / { print $2; exit }' "$deploy/../Cargo.toml")}
[ -n "$version" ] || { echo "no version" >&2; exit 1; }

# A release signed with a key the agents do not trust could never be
# installed by a self-update.
public=$(openssl pkey -in "$key" -pubout)
if ! grep -qxF "$(printf '%s\n' "$public" | sed -n 2p)" "$keys"; then
    echo "the public half of MF_SIGNING_KEY is not in $keys" >&2
    exit 1
fi

# The keys go where the installers have the line @RELEASE_KEYS@.
for installer in install.sh install-server.sh; do
    awk -v keys="$keys" '
        $0 == "@RELEASE_KEYS@" { while ((getline line < keys) > 0) if (line !~ /^#/ && line != "") print line; close(keys); next }
        { print }
    ' "$deploy/$installer" >"$dir/$installer"
    chmod 0755 "$dir/$installer"
done
# Installed as <root>/deploy, so commands that name deploy/ work there too.
tar -czf "$dir/monitorflow-server-compose.tar.gz" -C "$deploy/.." --owner=0 --group=0 --numeric-owner \
    deploy/docker-compose.yml deploy/docker/pg_hba.conf deploy/restore.sh deploy/.env.example
install -m 0644 "$deploy/systemd/monitorflow-agent.service" "$dir/monitorflow-agent.service"
install -m 0644 "$deploy/sudoers/monitorflow-agent" "$dir/monitorflow-agent-sudoers"
install -m 0644 "$deploy/systemd/monitorflow-relay.service" "$dir/monitorflow-relay.service"
install -m 0644 "$deploy/sudoers/monitorflow-relay" "$dir/monitorflow-relay-sudoers"
printf '%s\n' "$version" >"$dir/VERSION"
rm -f "$dir/SHA256SUMS" "$dir/SHA256SUMS.sig"
sums=$(cd "$dir" && find . -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort | xargs sha256sum)
printf '%s\n' "$sums" >"$dir/SHA256SUMS"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$dir/SHA256SUMS" -out "$dir/SHA256SUMS.sig"
openssl pkeyutl -verify -pubin -inkey <(printf '%s\n' "$public") -rawin \
    -in "$dir/SHA256SUMS" -sigfile "$dir/SHA256SUMS.sig" >/dev/null
cat "$dir/SHA256SUMS"
