#!/usr/bin/env bash
# End-to-end test of a major upgrade (ADR-0182): Debian 12 to Debian 13 for
# real, in a plain `debian:12` container, with the agent of this checkout.
#
# The container is unprivileged and runs no systemd, so it never touches
# the host's consoles (see PR #320): the test starts at the unit's step,
# `monitorflow-agent major-upgrade run`, from a state file written as the
# root helper writes it, and `systemctl` is a stub that records the reboot.
# The signed command, the consent and the transient unit are covered by the
# unit tests of crates/agent/src/major_upgrade.
#
# 1. Brings the container up to date and adds a deb822 and a one-line
#    third-party source.
# 2. Runs the steps: sources moved to trixie, the third-party ones switched
#    off, `apt-get update`, `upgrade --without-new-pkgs`, `full-upgrade`,
#    then the reboot.
# 3. Checks the state file, /etc/os-release, the sources and the reboot.
# 4. Runs it again from a broken mirror and checks that the sources are put
#    back as they were and no package changed.
#
# Usage: run-e2e.sh
# Needs: docker, cargo, and Internet access to deb.debian.org.
set -euo pipefail
cd "$(dirname "$0")/../.."

NAME=mf-major-upgrade-e2e
TARGET=${CARGO_TARGET_DIR:-target}
AGENT_BIN=$(realpath "$TARGET/debug/monitorflow-agent")
failures=0

cleanup() { docker rm -f "$NAME" "$NAME-broken" >/dev/null 2>&1 || true; }
trap cleanup EXIT

check() {
    local what=$1; shift
    if "$@"; then echo "ok   $what"; else echo "FAIL $what"; failures=$((failures + 1)); fi
}

cargo build -q -p monitorflow-agent

# The agent runs through the host's own loader and libraries: the host's
# glibc is newer than the image's.
start() {
    docker run -d --name "$1" -v "$AGENT_BIN:/agent:ro" -v /usr/lib:/hostlib:ro debian:12 sleep infinity >/dev/null
    docker exec "$1" sh -ec '
        apt-get -q update >/dev/null
        DEBIAN_FRONTEND=noninteractive apt-get -q -y upgrade >/dev/null
        printf "#!/bin/sh\necho \"\$@\" >>/tmp/systemctl.log\n" >/usr/bin/systemctl
        chmod 0755 /usr/bin/systemctl
        printf "Types: deb\nURIs: https://packages.example.invalid/apt\nSuites: bookworm\nComponents: main\n" \
            >/etc/apt/sources.list.d/vendor.sources
        echo "deb http://apt.example.invalid/x bookworm main" >/etc/apt/sources.list.d/other.list
        mkdir -m 0755 -p /var/lib/monitorflow/major-upgrade
        boot=$(cat /proc/sys/kernel/random/boot_id)
        now=$(($(date +%s) * 1000))
        printf "{\"command_id\": \"e2e\", \"from\": \"debian 12\", \"to\": \"debian 13\", \"phase\": \"starting\", \"boot_id\": \"%s\", \"started_at_unix_ms\": %s, \"updated_at_unix_ms\": %s}\n" \
            "$boot" "$now" "$now" >/var/lib/monitorflow/major-upgrade/state.json'
}

run_steps() {
    docker exec "$1" /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent major-upgrade run
}

phase() { docker exec "$1" sh -c 'grep -o "\"phase\": \"[a-z_]*\"" /var/lib/monitorflow/major-upgrade/state.json | cut -d\" -f4'; }

echo "== Debian 12 to 13"
start "$NAME"
check "the steps finish" run_steps "$NAME"
check "the state says rebooting" test "$(phase "$NAME")" = rebooting
check "the server runs Debian 13" docker exec "$NAME" grep -qx 'VERSION_ID="13"' /etc/os-release
check "the reboot was asked" docker exec "$NAME" grep -qx reboot /tmp/systemctl.log
check "Debian's deb822 sources point at trixie" docker exec "$NAME" grep -q "^Suites: trixie trixie-updates" /etc/apt/sources.list.d/debian.sources
check "the deb822 third-party source is off" docker exec "$NAME" grep -qx "Enabled: no" /etc/apt/sources.list.d/vendor.sources
check "the one-line third-party source is off" docker exec "$NAME" grep -q "^# deb http://apt.example.invalid/x bookworm main" /etc/apt/sources.list.d/other.list
check "the originals were kept" docker exec "$NAME" grep -q "Suites: bookworm" /var/lib/monitorflow/major-upgrade/backup/etc/apt/sources.list.d/debian.sources
check "the state lists the sources switched off" docker exec "$NAME" grep -q "packages.example.invalid" /var/lib/monitorflow/major-upgrade/state.json
check "it runs once" bash -c "! docker exec $NAME /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent major-upgrade run 2>/dev/null"

echo "== A broken mirror puts the sources back"
start "$NAME-broken"
docker exec "$NAME-broken" sh -c 'sed -i "s#http://deb.debian.org/debian\$#http://deb.debian.org/no-such-debian#" /etc/apt/sources.list.d/debian.sources
    cp /etc/apt/sources.list.d/debian.sources /tmp/before.sources
    dpkg-query -W >/tmp/before.packages'
check "the steps stop" bash -c "! docker exec $NAME-broken /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent major-upgrade run 2>/dev/null"
check "the state says failed" test "$(phase "$NAME-broken")" = failed
check "the sources are back as they were" docker exec "$NAME-broken" cmp /tmp/before.sources /etc/apt/sources.list.d/debian.sources
check "no package changed" docker exec "$NAME-broken" sh -c 'dpkg-query -W | cmp - /tmp/before.packages'
check "the state says the sources were put back" docker exec "$NAME-broken" grep -q '"sources_restored": true' /var/lib/monitorflow/major-upgrade/state.json
check "no reboot was asked" bash -c "! docker exec $NAME-broken test -e /tmp/systemctl.log"

if [ "$failures" -gt 0 ]; then
    echo "$failures checks failed"
    exit 1
fi
echo "all checks passed"
