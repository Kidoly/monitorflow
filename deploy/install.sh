#!/bin/sh
# Installs, upgrades or removes the MonitorFlow agent on a Linux host with
# systemd, as the manual steps of docs/deploy-agent.md do (ADR-0052):
#
#   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
#     | sudo MF_SERVER=relay1.example.com:7443 MF_TOKEN=mfe1.… sh
#
# Settings, from the environment:
#   MF_SERVER        host:port (or https URL) of the upstream, server or
#                    relay. Required on the first install; port 7443 by
#                    default. IPv6 addresses in brackets: [2001:db8::1]:7443
#   MF_TOKEN         one-time enrollment token. Required unless enrolled.
#   MF_ENROLL        host:port (or https URL) of the server's enrollment
#                    listener. Defaults to MF_SERVER's host on port 7445;
#                    set it when MF_SERVER is a relay.
#   MF_VERSION       release to install from GitHub, like 0.2.0; latest by
#                    default.
#   MF_DOWNLOAD_URL  https URL of a directory holding the release files,
#                    for sites that serve them themselves.
#   MF_TIMEOUT       seconds to wait for the enrollment; 120 by default.
#   MF_UNINSTALL=1   remove the binary, service and sudo rules; keep the
#                    configuration and identity. With MF_PURGE=1, remove
#                    those and the monitorflow user too.
set -eu

RELEASES=https://github.com/Kidoly/monitorflow/releases
BIN=/usr/bin/monitorflow-agent
CONF_DIR=/etc/monitorflow
CONF=$CONF_DIR/agent.toml
ENV_FILE=$CONF_DIR/agent-enroll.env
DATA_DIR=/var/lib/monitorflow/agent
SUDOERS=/etc/sudoers.d/monitorflow-agent
UNIT=/etc/systemd/system/monitorflow-agent.service
SERVICE=monitorflow-agent
AGENT_USER=monitorflow

MF_SERVER=${MF_SERVER:-}
MF_TOKEN=${MF_TOKEN:-}
MF_ENROLL=${MF_ENROLL:-}
MF_VERSION=${MF_VERSION:-}
MF_DOWNLOAD_URL=${MF_DOWNLOAD_URL:-}
MF_TIMEOUT=${MF_TIMEOUT:-120}
MF_UNINSTALL=${MF_UNINSTALL:-}
MF_PURGE=${MF_PURGE:-}

say() { printf 'monitorflow: %s\n' "$*"; }
die() {
    printf 'monitorflow: error: %s\n' "$*" >&2
    exit 1
}

enrolled() { [ -s "$DATA_DIR/tls/cert.pem" ] && [ -s "$DATA_DIR/tls/key.pem" ]; }

# `https://host:port` from host:port, [v6]:port, a bare host or an https
# URL; $3 is the default port. The agent validates the rest when it
# writes its configuration.
to_url() {
    case $1 in
        https://*)
            printf '%s' "$1"
            return
            ;;
        *://*) die "$2 must be host:port or an https:// URL" ;;
        *[!A-Za-z0-9.:_\[\]-]*) die "$2 must be host:port, like monitorflow.example.com:7443" ;;
        \[*\]:*) printf 'https://%s' "$1" ;;
        \[*\]) printf 'https://%s:%s' "$1" "$3" ;;
        *:*:*) die "$2: write an IPv6 address in brackets, like [2001:db8::1]:$3" ;;
        *:*) printf 'https://%s' "$1" ;;
        *) printf 'https://%s:%s' "$1" "$3" ;;
    esac
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only -O "$2" "$1"
    else
        die "curl or wget is needed to download the agent"
    fi || die "cannot download $1"
}

# Fails unless $tmp/$1 has the checksum SHA256SUMS lists for it.
verify() {
    expected=$(awk -v name="$1" '$2 == name || $2 == "*" name { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || die "$1 is not listed in SHA256SUMS"
    actual=$(sha256sum "$tmp/$1" | cut -d ' ' -f 1)
    [ "$actual" = "$expected" ] || die "checksum mismatch for $1: the download is corrupt or was tampered with; nothing was installed"
}

uninstall() {
    if [ -f "$UNIT" ]; then
        systemctl disable --now "$SERVICE" >/dev/null 2>&1 || true
        rm -f "$UNIT"
        systemctl daemon-reload
    fi
    rm -f "$BIN" "$SUDOERS" "$ENV_FILE"
    if [ "$MF_PURGE" = 1 ]; then
        rm -rf "$CONF" "$DATA_DIR"
        rmdir "$CONF_DIR" /var/lib/monitorflow 2>/dev/null || true
        # Only the user this installer made, whose home was the agent's data.
        if [ "$(getent passwd "$AGENT_USER" | cut -d : -f 6)" = "$DATA_DIR" ]; then
            userdel "$AGENT_USER"
        fi
        say "agent removed, with its configuration, identity and user"
    else
        say "agent removed; $CONF and the identity in $DATA_DIR are kept (MF_PURGE=1 removes them)"
    fi
}

create_user() {
    id -u "$AGENT_USER" >/dev/null 2>&1 && return
    nologin=/usr/sbin/nologin
    for shell in /usr/sbin/nologin /sbin/nologin /usr/bin/nologin; do
        if [ -x "$shell" ]; then
            nologin=$shell
            break
        fi
    done
    if getent group "$AGENT_USER" >/dev/null; then
        group="-g $AGENT_USER"
    else
        group=-U
    fi
    # shellcheck disable=SC2086 # $group is two words or one, never empty
    useradd --system $group --home-dir "$DATA_DIR" --no-create-home --shell "$nologin" "$AGENT_USER"
}

install_sudoers() {
    if ! command -v visudo >/dev/null 2>&1; then
        say "warning: sudo is not installed; package updates and source commands will fail until it is and $SUDOERS exists"
        return
    fi
    visudo -cqf "$tmp/monitorflow-agent-sudoers" || die "visudo refuses the downloaded sudo rules"
    install -o root -g root -m 0440 "$tmp/monitorflow-agent-sudoers" "$SUDOERS"
    visudo -cq || die "sudo's configuration is invalid after adding $SUDOERS; check it with visudo -c"
}

# Waits for the identity the first start writes. Fails as soon as the agent
# exits, as on a refused token, or after MF_TIMEOUT seconds.
wait_for_enrollment() {
    pid=$(systemctl show -p MainPID --value "$SERVICE")
    waited=0
    until enrolled; do
        now=$(systemctl show -p MainPID --value "$SERVICE")
        if [ "$now" != "$pid" ] || [ "$now" = 0 ] || [ "$waited" -ge "$MF_TIMEOUT" ]; then
            systemctl stop "$SERVICE" || true
            rm -f "$ENV_FILE"
            if [ "$waited" -ge "$MF_TIMEOUT" ]; then
                say "the agent did not enroll within $MF_TIMEOUT seconds"
            else
                say "the agent stopped before it enrolled"
            fi
            journalctl -u "$SERVICE" -n 15 --no-pager >&2 || true
            die "enrollment failed; see journalctl -u $SERVICE. Check MF_SERVER, MF_ENROLL and that the token is unused and not expired, then run the installer again"
        fi
        sleep 1
        waited=$((waited + 1))
    done
    rm -f "$ENV_FILE"
}

[ "$(id -u)" -eq 0 ] || die "run as root, like: curl -fsSL …/install.sh | sudo MF_SERVER=… MF_TOKEN=… sh"
[ "$(uname -s)" = Linux ] || die "this installer is for Linux; see docs/deploy-agent.md for Windows"
[ -d /run/systemd/system ] || die "systemd is not running; install the agent by hand as in docs/deploy-agent.md"

if [ "$MF_UNINSTALL" = 1 ]; then
    uninstall
    exit 0
fi

case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no agent build for $(uname -m); x86_64 and aarch64 are available" ;;
esac
case $MF_TIMEOUT in
    '' | *[!0-9]*) die "MF_TIMEOUT must be a number of seconds" ;;
esac

if [ -n "$MF_DOWNLOAD_URL" ]; then
    [ -z "$MF_VERSION" ] || die "MF_VERSION selects a GitHub release; with MF_DOWNLOAD_URL, point it at the files of that version instead"
    base=${MF_DOWNLOAD_URL%/}
elif [ -z "$MF_VERSION" ] || [ "$MF_VERSION" = latest ]; then
    base=$RELEASES/latest/download
else
    base=$RELEASES/download/v${MF_VERSION#v}
fi
case $base in
    https://*) ;;
    *) die "MF_DOWNLOAD_URL must be an https:// URL" ;;
esac

# Check every input before changing anything.
if enrolled; then
    [ -z "$MF_TOKEN" ] || say "already enrolled: MF_TOKEN is not used; revoke it in the web interface"
else
    [ -n "$MF_TOKEN" ] || die "MF_TOKEN is required: create an agent token in the web interface (Admin › Enrollment tokens)"
    case $MF_TOKEN in
        *[!A-Za-z0-9._-]* | *.*.*.*) die "MF_TOKEN is not an enrollment token (mfe1.…)" ;;
        mfe1.?*.?*) ;;
        *) die "MF_TOKEN is not an enrollment token (mfe1.…)" ;;
    esac
fi
upstream_url=
enroll_url=
if [ -f "$CONF" ]; then
    [ -z "$MF_SERVER$MF_ENROLL" ] || say "keeping the existing $CONF: MF_SERVER and MF_ENROLL are not used"
else
    [ -n "$MF_SERVER" ] || die "MF_SERVER is required: the host:port of the server or relay the agent connects to"
    upstream_url=$(to_url "$MF_SERVER" MF_SERVER 7443)
    [ -z "$MF_ENROLL" ] || enroll_url=$(to_url "$MF_ENROLL" MF_ENROLL 7445)
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

asset=monitorflow-agent-linux-$arch
say "downloading $asset from $base"
for file in SHA256SUMS "$asset" monitorflow-agent.service monitorflow-agent-sudoers; do
    fetch "$base/$file" "$tmp/$file"
done
for file in "$asset" monitorflow-agent.service monitorflow-agent-sudoers; do
    verify "$file"
done
chmod 0755 "$tmp/$asset"
version=$("$tmp/$asset" --version) || die "the downloaded agent does not run on this host"

create_user
install -d -o "$AGENT_USER" -g "$AGENT_USER" -m 0700 "$DATA_DIR"
install -d -o root -g root -m 0755 "$CONF_DIR"
# A rename, so a running agent keeps its file until it restarts.
install -o root -g root -m 0755 "$tmp/$asset" "$BIN.new"
mv -f "$BIN.new" "$BIN"
install_sudoers
install -o root -g root -m 0644 "$tmp/monitorflow-agent.service" "$UNIT"
systemctl daemon-reload

if [ -n "$upstream_url" ]; then
    "$BIN" --config "$CONF" init-config --upstream-url "$upstream_url" --enroll-url "$enroll_url" >/dev/null
    chown root:"$AGENT_USER" "$CONF"
    chmod 0640 "$CONF"
    say "wrote $CONF for $upstream_url"
fi

systemctl enable --quiet "$SERVICE"
if enrolled; then
    systemctl restart "$SERVICE"
    say "$version installed; configuration and identity kept"
else
    # Read by systemd as root; the agent's user cannot read it (ADR-0007).
    rm -f "$ENV_FILE"
    (
        umask 077
        printf 'MONITORFLOW_ENROLLMENT_TOKEN=%s\n' "$MF_TOKEN" >"$ENV_FILE"
    )
    systemctl restart "$SERVICE"
    say "enrolling…"
    wait_for_enrollment
    say "$version installed and enrolled"
fi
say "status: systemctl status $SERVICE, logs: journalctl -u $SERVICE"
