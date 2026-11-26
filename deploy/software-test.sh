#!/bin/sh
# Runs the fixed command lines of the software helper (ADR-0188) against
# the real package managers, in throwaway Debian 13 and Rocky Linux 9
# containers: the lines `monitorflow-agent packages software` builds, and
# the listings it reads to skip what is installed already. Needs Docker
# and network access to the distributions' mirrors.
#
#   deploy/software-test.sh            # both
#   deploy/software-test.sh debian     # one
#
# The unit tests in crates/agent/src/software/tests.rs check that the
# helper builds exactly these lines.
set -eu

PACKAGE=${PACKAGE:-tree}

debian() {
    docker run --rm -e PACKAGE="$PACKAGE" debian:13 sh -euc '
        export DEBIAN_FRONTEND=noninteractive LC_ALL=C
        opts="-q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold"
        installed() {
            dpkg-query -W -f "\${Package} \${binary:Package} \${db:Status-Abbrev}\n" |
                awk -v p="$1" "\$1 == p && \$3 ~ /^ii/ { found = 1 } END { exit !found }"
        }
        apt-get -q update >/dev/null
        ! installed "$PACKAGE" || { echo "FAIL: $PACKAGE installed before"; exit 1; }
        # shellcheck disable=SC2086
        apt-get $opts --no-upgrade install "$PACKAGE" >/dev/null
        installed "$PACKAGE" || { echo "FAIL: install"; exit 1; }
        echo "ok: apt-get --no-upgrade install $PACKAGE"
        # Installed already: nothing to do, success.
        # shellcheck disable=SC2086
        apt-get $opts --no-upgrade install "$PACKAGE" | grep -Eq "already (installed|the newest)" ||
            { echo "FAIL: second install"; exit 1; }
        echo "ok: a second install is a no-op"
        # shellcheck disable=SC2086
        apt-get $opts remove "$PACKAGE" >/dev/null
        ! installed "$PACKAGE" || { echo "FAIL: remove"; exit 1; }
        echo "ok: apt-get remove $PACKAGE"
        # An unknown name fails, so the helper reports it.
        # shellcheck disable=SC2086
        if apt-get $opts --no-upgrade install monitorflow-no-such-package >/dev/null 2>&1; then
            echo "FAIL: unknown package installed"; exit 1
        fi
        echo "ok: an unknown package fails"
        echo "--- dpkg-query sample"
        dpkg-query -W -f "\${Package} \${binary:Package} \${db:Status-Abbrev}\n" | head -n 5
    '
}

rocky() {
    docker run --rm -e PACKAGE="$PACKAGE" rockylinux:9 sh -euc '
        export LC_ALL=C
        installed() { rpm -qa --qf "%{NAME}\n" | grep -qx "$1"; }
        ! installed "$PACKAGE" || { echo "FAIL: $PACKAGE installed before"; exit 1; }
        dnf -q -y install "$PACKAGE" >/dev/null
        installed "$PACKAGE" || { echo "FAIL: install"; exit 1; }
        echo "ok: dnf -q -y install $PACKAGE"
        dnf -q -y install "$PACKAGE" >/dev/null && echo "ok: a second install is a no-op"
        dnf -q -y remove "$PACKAGE" >/dev/null
        ! installed "$PACKAGE" || { echo "FAIL: remove"; exit 1; }
        echo "ok: dnf -q -y remove $PACKAGE"
        if dnf -q -y install monitorflow-no-such-package >/dev/null 2>&1; then
            echo "FAIL: unknown package installed"; exit 1
        fi
        echo "ok: an unknown package fails"
        echo "--- rpm sample"
        rpm -qa --qf "%{NAME}\n" | head -n 5
    '
}

case ${1:-all} in
    debian) debian ;;
    rocky) rocky ;;
    all) debian && rocky ;;
    *) echo "usage: $0 [debian|rocky]" >&2; exit 2 ;;
esac
