#!/usr/bin/env bash
# End-to-end test of major upgrades on Ubuntu and the RHEL family
# (ADR-0182), in plain `ubuntu:22.04` and `rockylinux:8` containers, with
# the agent of this checkout.
#
# The containers are unprivileged and run no systemd, so they never touch
# the host's consoles (see PR #320): the test reads the checks as an
# unprivileged user, then starts at the unit's step, `monitorflow-agent
# major-upgrade run`, from a state file written as the root helper writes
# it, and `systemctl` is a stub that records the reboot. The signed
# command, the consent and the transient unit are covered by the unit
# tests of crates/agent/src/major_upgrade.
#
# Ubuntu 22.04 to 24.04, for real:
# 1. the checks as the agent's user: the archive's entries found, the next
#    LTS offered by `do-release-upgrade -c`, a third-party source listed;
# 2. on the MonitorFlow mirrors, a warning in the checks, and the steps
#    refuse before `do-release-upgrade` runs (the campaign puts the
#    server's own sources back first);
# 3. the steps: `do-release-upgrade` in non-interactive mode rewrites the
#    sources to noble and switches the third-party one off, then the
#    reboot; the container runs Ubuntu 24.04 afterwards.
#
# Rocky Linux 8 through ELevate:
# 4. the checks: leapp and leapp-data-rocky missing, then installed;
# 5. on the mirrors, a warning, and the steps refuse before leapp runs;
# 6. `leapp preupgrade` alone (`leapp_preupgrade`): its report kept, never
#    `leapp upgrade`, no package changed and no reboot asked;
# 7. the steps: `leapp preupgrade` finds inhibitors in a container (it has
#    no boot loader), so the upgrade fails with leapp's report kept as it
#    is, no package changed and no reboot asked.
#
# Usage: run-e2e-ubuntu-rhel.sh [ubuntu|rocky]   (both by default)
# Needs: docker, cargo, and Internet access to the Ubuntu, Rocky Linux and
# AlmaLinux (ELevate) archives. The Ubuntu upgrade takes 10 to 20 minutes.
set -euo pipefail
cd "$(dirname "$0")/../.."

NAME=mf-major-upgrade-e2e
TARGET=${CARGO_TARGET_DIR:-target}
WHICH=${1:-all}
failures=0

cleanup() { docker rm -f "$NAME-ubuntu" "$NAME-rocky" >/dev/null 2>&1 || true; }
trap cleanup EXIT

check() {
    local what=$1; shift
    if "$@"; then echo "ok   $what"; else echo "FAIL $what"; failures=$((failures + 1)); fi
}

cargo build -q -p monitorflow-agent
AGENT_BIN=$(realpath "$TARGET/debug/monitorflow-agent")

# The agent runs through the host's own loader and libraries: the host's
# glibc is newer than the images'.
start() {
    docker run -d --name "$1" -v "$AGENT_BIN:/agent:ro" -v /usr/lib:/hostlib:ro "$2" sleep infinity >/dev/null
    docker exec "$1" sh -ec '
        # Diverted on Ubuntu, so that the upgrade of systemd installs its
        # own systemctl beside the stub rather than over it.
        if command -v dpkg-divert >/dev/null; then
            dpkg-divert --quiet --local --rename --divert /usr/bin/systemctl.distrib --add /usr/bin/systemctl
        fi
        printf "#!/bin/sh\necho \"\$@\" >>/tmp/systemctl.log\n" >/usr/bin/systemctl
        chmod 0755 /usr/bin/systemctl
        useradd -m monitorflow'
}

agent() { docker exec "$1" /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent "${@:2}"; }
checks() { docker exec -u monitorflow "$1" /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent major-upgrade checks; }

# state CONTAINER FROM TO [true]: the state the helper writes, of an
# upgrade or, with `true`, of `leapp preupgrade` alone.
state() {
    docker exec "$1" sh -ec '
        mkdir -m 0755 -p /var/lib/monitorflow/major-upgrade
        boot=$(cat /proc/sys/kernel/random/boot_id)
        now=$(($(date +%s) * 1000))
        printf "{\"command_id\": \"e2e\", \"from\": \"%s\", \"to\": \"%s\", \"phase\": \"starting\", \"boot_id\": \"%s\", \"preupgrade_only\": %s, \"started_at_unix_ms\": %s, \"updated_at_unix_ms\": %s}\n" \
            "$1" "$2" "$boot" "$3" "$now" "$now" >/var/lib/monitorflow/major-upgrade/state.json' sh "$2" "$3" "${4:-false}"
}

run_refused() { ! docker exec "$1" /hostlib/ld-linux-x86-64.so.2 --library-path /hostlib /agent major-upgrade run 2>/dev/null; }
error_says() { docker exec "$1" grep -q "$2" /var/lib/monitorflow/major-upgrade/state.json; }

# Prints what a step printed, indented under the test's lines.
show() { while IFS= read -r line; do echo "     $line"; done <<<"$1"; }

phase() { docker exec "$1" sh -c 'grep -o "\"phase\": \"[a-z_]*\"" /var/lib/monitorflow/major-upgrade/state.json | cut -d\" -f4'; }

if [ "$WHICH" = all ] || [ "$WHICH" = ubuntu ]; then
    echo "== Ubuntu 22.04 to 24.04"
    U=$NAME-ubuntu
    start "$U" ubuntu:22.04
    docker exec "$U" sh -ec '
        export DEBIAN_FRONTEND=noninteractive
        apt-get -q update >/dev/null
        apt-get -q -y upgrade >/dev/null
        apt-get -q -y install ubuntu-release-upgrader-core >/dev/null
        rm -f /var/run/reboot-required
        echo "deb https://ppa.launchpadcontent.net/example/none/ubuntu/ jammy main" >/etc/apt/sources.list.d/ppa.list'
    out=$(checks "$U" 2>&1) || true
    show "$out"
    check "the checks name the pair" grep -q "^Ubuntu 22.04 (jammy) to Ubuntu 24.04 (noble)$" <<<"$out"
    check "the next LTS is offered" grep -q "^ok release_offered: do-release-upgrade offers 24.04" <<<"$out"
    check "the archive's entries are found" grep -q "^ok sources: .* entries of the Ubuntu archive" <<<"$out"
    check "the PPA is listed" grep -q "ppa.launchpadcontent.net" <<<"$out"
    check "nothing fails" bash -c "! grep -q '^failed ' <<<\"\$1\"" _ "$out"

    # On the mirrors: a warning, and no do-release-upgrade.
    docker exec "$U" sh -c 'printf "Types: deb\nURIs: http://mirror.example.invalid/apt/ubuntu-22.04/current/\nSuites: jammy jammy-updates\nComponents: main\n" >/etc/apt/sources.list.d/monitorflow.sources'
    out=$(checks "$U" 2>&1) || true
    show "$out"
    check "the mirrors are a warning" grep -q "^warning sources: the server takes its packages from the MonitorFlow mirrors, which do-release-upgrade does not use: the campaign puts its own sources back" <<<"$out"
    state "$U" "ubuntu 22.04" "ubuntu 24.04"
    check "the steps refuse on the mirrors" run_refused "$U"
    check "the error says why" error_says "$U" "still takes its packages from the MonitorFlow mirrors"
    check "do-release-upgrade did not run" bash -c "! docker exec $U grep -q do-release-upgrade /var/lib/monitorflow/major-upgrade/log"
    docker exec "$U" rm -f /etc/apt/sources.list.d/monitorflow.sources

    state "$U" "ubuntu 22.04" "ubuntu 24.04"
    check "the steps finish" agent "$U" major-upgrade run
    check "the state says rebooting" test "$(phase "$U")" = rebooting
    check "the server runs Ubuntu 24.04" docker exec "$U" grep -qx 'VERSION_ID="24.04"' /etc/os-release
    check "the sources point at noble" docker exec "$U" sh -c 'grep -rqs "noble" /etc/apt/sources.list /etc/apt/sources.list.d/'
    check "the PPA is off" bash -c "! docker exec $U grep -qs '^deb https://ppa.launchpadcontent.net' /etc/apt/sources.list.d/ppa.list"
    check "the state lists the PPA switched off" docker exec "$U" grep -q "ppa.launchpadcontent.net" /var/lib/monitorflow/major-upgrade/state.json
    check "the reboot was asked" docker exec "$U" grep -qx reboot /tmp/systemctl.log
fi

if [ "$WHICH" = all ] || [ "$WHICH" = rocky ]; then
    echo "== Rocky Linux 8 through ELevate"
    R=$NAME-rocky
    start "$R" rockylinux:8
    docker exec "$R" sh -ec 'dnf -q -y upgrade >/dev/null; dnf -q makecache >/dev/null'
    out=$(checks "$R" 2>&1) || true
    show "$out"
    check "the checks name the pair" grep -q "^Rocky Linux 8 to Rocky Linux 9$" <<<"$out"
    check "leapp is missing" grep -q "^failed leapp: install leapp and the ELevate data first" <<<"$out"
    check "the data package is named" grep -q "^    leapp-data-rocky$" <<<"$out"

    docker exec "$R" sh -ec '
        dnf -q -y install https://repo.almalinux.org/elevate/elevate-release-latest-el8.noarch.rpm >/dev/null
        dnf -q -y install leapp-upgrade leapp-data-rocky >/dev/null
        dnf -q makecache >/dev/null'
    out=$(checks "$R" 2>&1) || true
    show "$out"
    check "leapp is installed" grep -q "^ok leapp: leapp and its data for Rocky Linux 9 are installed" <<<"$out"
    check "the repositories are Rocky's" grep -q "^ok sources: leapp takes the repositories of Rocky Linux 9" <<<"$out"
    check "the packages are up to date" grep -q "^ok up_to_date" <<<"$out"

    docker exec "$R" sh -c 'rpm -qa | sort >/tmp/before.packages'

    # On the mirrors: a warning, and no leapp.
    docker exec "$R" sh -c 'printf "[monitorflow-baseos]\nname=MonitorFlow\nbaseurl=http://mirror.example.invalid/dnf/rocky-8/current/\nenabled=1\nskip_if_unavailable=1\n" >/etc/yum.repos.d/monitorflow-baseos.repo'
    out=$(checks "$R" 2>&1) || true
    show "$out"
    check "the mirrors are a warning" grep -q "^warning sources: the server takes its packages from the MonitorFlow mirrors, which leapp does not use" <<<"$out"
    state "$R" "rocky 8" "rocky 9"
    check "the steps refuse on the mirrors" run_refused "$R"
    check "the error says why" error_says "$R" "still takes its packages from the MonitorFlow mirrors"
    check "leapp did not run" bash -c "! docker exec $R grep -q 'bin/leapp' /var/lib/monitorflow/major-upgrade/log"
    docker exec "$R" rm -f /etc/yum.repos.d/monitorflow-baseos.repo

    # leapp preupgrade alone: its report, and nothing else.
    state "$R" "rocky 8" "rocky 9" true
    agent "$R" major-upgrade run >/dev/null 2>&1 || true
    ended=$(phase "$R")
    # shellcheck disable=SC2016 # $1 is the inner shell's.
    check "the preupgrade ended" bash -c '[ "$1" = done ] || [ "$1" = failed ]' _ "$ended"
    check "the state is a preupgrade" error_says "$R" '"preupgrade_only": true'
    check "leapp preupgrade ran" docker exec "$R" grep -qF '$ /usr/bin/leapp preupgrade' /var/lib/monitorflow/major-upgrade/log
    check "leapp upgrade did not run" bash -c "! docker exec $R grep -qF '$ /usr/bin/leapp upgrade' /var/lib/monitorflow/major-upgrade/log"
    check "its report is kept" docker exec "$R" sh -c '/usr/libexec/platform-python -c "import json,sys; s=json.load(open(sys.argv[1])); sys.exit(not s[\"report\"] or s[\"report\"] != open(sys.argv[2]).read())" /var/lib/monitorflow/major-upgrade/state.json /var/log/leapp/leapp-report.txt'
    check "no package changed by the preupgrade" docker exec "$R" sh -c 'rpm -qa | sort | cmp - /tmp/before.packages'
    check "no reboot was asked by the preupgrade" bash -c "! docker exec $R test -e /tmp/systemctl.log"

    state "$R" "rocky 8" "rocky 9"
    check "the steps stop" run_refused "$R"
    check "the state says failed" test "$(phase "$R")" = failed
    check "the error names the inhibitors" docker exec "$R" grep -q "leapp preupgrade. found [0-9]* inhibitors" /var/lib/monitorflow/major-upgrade/state.json
    check "leapp's report is kept as it is" docker exec "$R" sh -c '/usr/libexec/platform-python -c "import json,sys; s=json.load(open(sys.argv[1])); sys.exit(s[\"report\"] != open(sys.argv[2]).read())" /var/lib/monitorflow/major-upgrade/state.json /var/log/leapp/leapp-report.txt'
    check "no package changed" docker exec "$R" sh -c 'rpm -qa | sort | cmp - /tmp/before.packages'
    check "no reboot was asked" bash -c "! docker exec $R test -e /tmp/systemctl.log"
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures checks failed"
    exit 1
fi
echo "all checks passed"
