#!/bin/sh
# Installs, upgrades or removes the MonitorFlow agent on any Linux
# distribution with systemd, as the manual steps of docs/deploy-agent.md do
# (ADR-0052, ADR-0063), or a relay with MF_ROLE=relay, as those of
# docs/deploy-relay.md (ADR-0055):
#
#   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
#     | sudo MF_SERVER=relay1.example.com:7443 MF_TOKEN=mfe1.… sh
#
# Settings, from the environment:
#   MF_ROLE          agent (the default) or relay.
#   MF_SERVER        host:port (or https URL) of the upstream, server or
#                    relay. Required on the first install; port 7443 by
#                    default. IPv6 addresses in brackets: [2001:db8::1]:7443
#   MF_TOKEN         the site's install key (agents, ADR-0058) or a one-time
#                    enrollment token. Required unless enrolled.
#   MF_ENROLL        host:port (or https URL) of the server's enrollment
#                    listener. Defaults to MF_SERVER's host on port 7445;
#                    set it when MF_SERVER is a relay.
#   MF_MIRROR_URL    relay only: the http(s) URL this relay's agents reach
#                    its mirror listener at, like http://relay1.example.com:7480;
#                    turns on the relay's copy of the mirrors (ADR-0034).
#   MF_MIRROR=1      relay only: the same, at the first of the relay's
#                    confirmed names on port 7480 (ADR-0066).
#   MF_VERSION       release to install from GitHub, like 0.2.0; latest by
#                    default.
#   MF_DOWNLOAD_URL  https URL of a directory holding the release files,
#                    for sites that serve them themselves.
#   MF_SUMS_SHA256   SHA-256 of the release's SHA256SUMS, as in the commands
#                    of the central server (ADR-0054). Every file is then
#                    checked against it, so downloads do not check the
#                    certificate (still HTTPS only).
#   MF_TIMEOUT       seconds to wait for the enrollment; 120 by default.
#   MF_REQUIRE_SIGNATURE=1  fail when OpenSSL 3 is missing and the
#                    release signature cannot be checked.
#   MF_ALLOW_UNSIGNED=1  install a release without SHA256SUMS.sig, like
#                    0.1.0, checking its checksums only.
#   MF_ALLOW_COMMANDS=1  agent only: let it run the shell commands of AI
#                    remediation (commands.shell), also on a host already
#                    installed; 0 takes that back. Unset keeps what is there.
#   MF_UNINSTALL=1   remove the binary, service and sudo rules; keep the
#                    configuration and identity. With MF_PURGE=1, remove
#                    those and the service's user too.
set -eu

MF_ROLE=${MF_ROLE:-agent}
case $MF_ROLE in
    agent)
        RUN_USER=monitorflow
        # The previous binary and status of the last self-update (ADR-0053).
        STATE_DIR=/var/lib/monitorflow/self-update
        ;;
    relay)
        RUN_USER=monitorflow-relay
        STATE_DIR=/var/lib/monitorflow/relay-self-update
        ;;
    *)
        printf 'monitorflow: error: MF_ROLE must be agent or relay\n' >&2
        exit 1
        ;;
esac
RELEASES=https://github.com/Kidoly/monitorflow/releases
BIN=/usr/bin/monitorflow-$MF_ROLE
CONF_DIR=/etc/monitorflow
CONF=$CONF_DIR/$MF_ROLE.toml
ENV_FILE=$CONF_DIR/$MF_ROLE-enroll.env
DATA_DIR=/var/lib/monitorflow/$MF_ROLE
SUDOERS=/etc/sudoers.d/monitorflow-$MF_ROLE
UNIT=/etc/systemd/system/monitorflow-$MF_ROLE.service
SERVICE=monitorflow-$MF_ROLE
ALLOW_COMMANDS=$UNIT.d/allow-commands.conf

MF_SERVER=${MF_SERVER:-}
MF_TOKEN=${MF_TOKEN:-}
MF_ENROLL=${MF_ENROLL:-}
MF_VERSION=${MF_VERSION:-}
MF_DOWNLOAD_URL=${MF_DOWNLOAD_URL:-}
MF_SUMS_SHA256=${MF_SUMS_SHA256:-}
MF_TIMEOUT=${MF_TIMEOUT:-120}
MF_UNINSTALL=${MF_UNINSTALL:-}
MF_PURGE=${MF_PURGE:-}
MF_REQUIRE_SIGNATURE=${MF_REQUIRE_SIGNATURE:-}
MF_ALLOW_UNSIGNED=${MF_ALLOW_UNSIGNED:-}
MF_MIRROR_URL=${MF_MIRROR_URL:-}
MF_MIRROR=${MF_MIRROR:-}
MF_ALLOW_COMMANDS=${MF_ALLOW_COMMANDS:-}

# Public keys that sign releases (ADR-0053), filled in from
# deploy/release-keys.pem by deploy/release-files.sh.
RELEASE_KEYS='
@RELEASE_KEYS@
'

say() { printf 'monitorflow: %s\n' "$*"; }
die() {
    printf 'monitorflow: error: %s\n' "$*" >&2
    exit 1
}

enrolled() { [ -s "$DATA_DIR/tls/cert.pem" ] && [ -s "$DATA_DIR/tls/key.pem" ]; }

# `https://host:port` from host:port, [v6]:port, a bare host or an https
# URL; $3 is the default port. The agent or relay checks the rest when it
# loads its configuration.
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

# With MF_SUMS_SHA256 the content is pinned, not the certificate: hosts
# need not trust the central server's web certificate (ADR-0054).
try_fetch() {
    if command -v curl >/dev/null 2>&1; then
        if [ -n "$MF_SUMS_SHA256" ]; then
            curl -fsSL --proto '=https' --tlsv1.2 --insecure -o "$2" "$1"
        else
            curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
        fi
    elif command -v wget >/dev/null 2>&1; then
        if [ -n "$MF_SUMS_SHA256" ]; then
            wget -q --https-only --no-check-certificate -O "$2" "$1"
        else
            wget -q --https-only -O "$2" "$1"
        fi
    else
        die "curl or wget is needed to download the $MF_ROLE"
    fi
}

fetch() { try_fetch "$1" "$2" || die "cannot download $1"; }

# Checks SHA256SUMS.sig against the release keys with OpenSSL 3, whatever
# served the files. Without a key (an installer run from the repository)
# or OpenSSL 3, only the checksums are checked, as before.
verify_signature() {
    case $RELEASE_KEYS in
        *'BEGIN PUBLIC KEY'*) ;;
        *)
            say "warning: this installer carries no release key; checking checksums only"
            return
            ;;
    esac
    if ! command -v openssl >/dev/null 2>&1 || ! openssl pkeyutl -help 2>&1 | grep -q -- -rawin; then
        [ "$MF_REQUIRE_SIGNATURE" != 1 ] || die "OpenSSL 3 is needed to check the release signature (MF_REQUIRE_SIGNATURE=1)"
        if [ -n "$MF_SUMS_SHA256" ]; then
            say "OpenSSL 3 is not installed, so the release signature is not checked; SHA256SUMS matches MF_SUMS_SHA256"
        else
            say "warning: OpenSSL 3 is not installed, so the release signature is not checked; checking checksums only"
        fi
        return
    fi
    if ! try_fetch "$base/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig"; then
        [ "$MF_ALLOW_UNSIGNED" = 1 ] || die "cannot download $base/SHA256SUMS.sig: releases before 0.2.0 are not signed; MF_ALLOW_UNSIGNED=1 installs one with its checksums checked only"
        say "warning: unsigned release; checking checksums only (MF_ALLOW_UNSIGNED=1)"
        return
    fi
    # One file per key; any of them may have signed (key rotation).
    printf '%s\n' "$RELEASE_KEYS" | awk -v dir="$tmp" '
        /BEGIN PUBLIC KEY/ { n++; inside = 1 }
        inside { print > (dir "/release-key-" n ".pem") }
        /END PUBLIC KEY/ { inside = 0; close(dir "/release-key-" n ".pem") }'
    for key in "$tmp"/release-key-*.pem; do
        if openssl pkeyutl -verify -pubin -inkey "$key" -rawin -in "$tmp/SHA256SUMS" \
            -sigfile "$tmp/SHA256SUMS.sig" >/dev/null 2>&1; then
            say "release signature verified"
            return
        fi
    done
    die "SHA256SUMS.sig does not verify against the release keys: the files were tampered with or are not a MonitorFlow release; nothing was installed"
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
        rm -f "$UNIT" "$ALLOW_COMMANDS"
        rmdir "$UNIT.d" 2>/dev/null || true
        systemctl daemon-reload
    fi
    rm -f "$BIN" "$SUDOERS" "$ENV_FILE"
    rm -rf "$STATE_DIR"
    if [ "$MF_PURGE" = 1 ]; then
        rm -rf "$CONF" "$DATA_DIR"
        rmdir "$CONF_DIR" /var/lib/monitorflow 2>/dev/null || true
        # Only the user this installer made, whose home was the service's data.
        if [ "$(getent passwd "$RUN_USER" | cut -d : -f 6)" = "$DATA_DIR" ]; then
            userdel "$RUN_USER"
        fi
        say "$MF_ROLE removed, with its configuration, identity and user"
    else
        say "$MF_ROLE removed; $CONF and the identity in $DATA_DIR are kept (MF_PURGE=1 removes them)"
    fi
}

create_user() {
    id -u "$RUN_USER" >/dev/null 2>&1 && return
    nologin=/usr/sbin/nologin
    for shell in /usr/sbin/nologin /sbin/nologin /usr/bin/nologin; do
        if [ -x "$shell" ]; then
            nologin=$shell
            break
        fi
    done
    if getent group "$RUN_USER" >/dev/null; then
        group="-g $RUN_USER"
    else
        group=-U
    fi
    # shellcheck disable=SC2086 # $group is two words or one, never empty
    useradd --system $group --home-dir "$DATA_DIR" --no-create-home --shell "$nologin" "$RUN_USER"
}

install_sudoers() {
    if ! command -v visudo >/dev/null 2>&1; then
        say "warning: sudo is not installed; privileged commands and self-updates will fail until it is and $SUDOERS exists"
        return
    fi
    visudo -cqf "$tmp/$SERVICE-sudoers" || die "visudo refuses the downloaded sudo rules"
    # The whole configuration too, unless it failed before we touched it.
    if visudo -cq >/dev/null 2>&1; then
        install -o root -g root -m 0440 "$tmp/$SERVICE-sudoers" "$SUDOERS"
        if ! visudo -c >"$tmp/visudo.log" 2>&1; then
            rm -f "$SUDOERS"
            cat "$tmp/visudo.log" >&2
            die "sudo's configuration fails visudo -c with $SUDOERS, which was removed again"
        fi
    else
        install -o root -g root -m 0440 "$tmp/$SERVICE-sudoers" "$SUDOERS"
        visudo -c >&2 || true
        say "warning: sudo's configuration already failed visudo -c before $SUDOERS was added; fix it, or the $MF_ROLE's sudo rules may not apply"
    fi
}

# Writes relay.toml as docs/deploy-relay.md shows it: the upstream, the
# enrollment endpoint when given, and the mirror copy with MF_MIRROR_URL
# or MF_MIRROR.
write_relay_config() {
    {
        printf '# MonitorFlow relay (docs/deploy-relay.md), written by install.sh.\n'
        printf '# Every key and its default: deploy/config/relay.example.toml.\n'
        printf 'upstream_url = "%s"\n' "$upstream_url"
        [ -z "$enroll_url" ] || printf 'enroll_url = "%s"\n' "$enroll_url"
        if [ -n "$MF_MIRROR_URL" ]; then
            printf '\n[mirror]\nenabled = true\npublic_url = "%s"\n' "$MF_MIRROR_URL"
        elif [ "$MF_MIRROR" = 1 ]; then
            printf '\n[mirror]\nenabled = true\n'
        fi
    } >"$tmp/relay.toml"
    install -o root -g "$RUN_USER" -m 0640 "$tmp/relay.toml" "$CONF"
}

# Waits for the identity the first start writes. Fails as soon as the service
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
                say "the $MF_ROLE did not enroll within $MF_TIMEOUT seconds"
            else
                say "the $MF_ROLE stopped before it enrolled"
            fi
            journalctl -u "$SERVICE" -n 15 --no-pager >&2 || true
            die "enrollment failed; see journalctl -u $SERVICE. Check MF_SERVER, MF_ENROLL and that the token or install command is still valid, then run the installer again"
        fi
        sleep 1
        waited=$((waited + 1))
    done
    rm -f "$ENV_FILE"
}

[ "$(id -u)" -eq 0 ] || die "run as root, like: curl -fsSL …/install.sh | sudo MF_SERVER=… MF_TOKEN=… sh"
[ "$(uname -s)" = Linux ] || die "this installer is for Linux; see docs/deploy-agent.md for Windows"

if [ "$MF_UNINSTALL" = 1 ]; then
    uninstall
    exit 0
fi

# What the installer cannot do without, said before anything changes. The
# distribution does not matter (ADR-0063).
[ -d /run/systemd/system ] || die "this installer needs systemd, which keeps the $MF_ROLE running, and systemd is not running here; on another init system, run the binary as in docs/deploy-$MF_ROLE.md"
for tool in awk install sha256sum systemctl useradd; do
    command -v "$tool" >/dev/null 2>&1 || die "this installer needs $tool: install it with your package manager and run the installer again"
done
for dir in /usr/bin /etc/systemd/system; do
    [ -w "$dir" ] || die "$dir is read-only on this host, as on image-based systems, and the $MF_ROLE installs there"
done

case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "no $MF_ROLE build for $(uname -m); x86_64 and aarch64 are available" ;;
esac
case $MF_TIMEOUT in
    '' | *[!0-9]*) die "MF_TIMEOUT must be a number of seconds" ;;
esac
case $MF_SUMS_SHA256 in
    '') ;;
    *[!0-9A-Fa-f]*) die "MF_SUMS_SHA256 must be a SHA-256 in hex" ;;
    *)
        [ ${#MF_SUMS_SHA256} -eq 64 ] || die "MF_SUMS_SHA256 must be a SHA-256 in hex"
        MF_SUMS_SHA256=$(printf '%s' "$MF_SUMS_SHA256" | tr 'A-F' 'a-f')
        ;;
esac
[ -z "$MF_MIRROR" ] || [ "$MF_ROLE" = relay ] || die "MF_MIRROR is for relays (MF_ROLE=relay)"
if [ -n "$MF_MIRROR_URL" ]; then
    [ "$MF_ROLE" = relay ] || die "MF_MIRROR_URL is for relays (MF_ROLE=relay)"
    case $MF_MIRROR_URL in
        http://*[!A-Za-z0-9.:_/\[\]-]* | https://*[!A-Za-z0-9.:_/\[\]-]*) die "MF_MIRROR_URL must be an http(s) URL like http://relay1.example.com:7480" ;;
        http://?* | https://?*) ;;
        *) die "MF_MIRROR_URL must be an http(s) URL like http://relay1.example.com:7480" ;;
    esac
fi
case $MF_ALLOW_COMMANDS in
    '' | 0 | 1) ;;
    *) die "MF_ALLOW_COMMANDS must be 1 or 0" ;;
esac
[ -z "$MF_ALLOW_COMMANDS" ] || [ "$MF_ROLE" = agent ] || die "MF_ALLOW_COMMANDS is for agents"

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
    [ -z "$MF_TOKEN" ] || say "already enrolled: MF_TOKEN is not used. If this host was removed in the web interface, run the uninstall command it shows, then this one again"
else
    [ -n "$MF_TOKEN" ] || die "MF_TOKEN is required: copy the command from the web interface (Administration › Add servers)"
    case $MF_TOKEN in
        *[!A-Za-z0-9._-]* | *.*.*.*) die "MF_TOKEN is not an enrollment token (mfe1.…)" ;;
        mfe1.?*.?*) ;;
        *) die "MF_TOKEN is not an enrollment token (mfe1.…)" ;;
    esac
fi
upstream_url=
enroll_url=
if [ -f "$CONF" ]; then
    [ -z "$MF_SERVER$MF_ENROLL$MF_MIRROR_URL$MF_MIRROR" ] || say "keeping the existing $CONF: MF_SERVER, MF_ENROLL, MF_MIRROR_URL and MF_MIRROR are not used"
else
    [ -n "$MF_SERVER" ] || die "MF_SERVER is required: the host:port of the server or relay the $MF_ROLE connects to"
    upstream_url=$(to_url "$MF_SERVER" MF_SERVER 7443)
    [ -z "$MF_ENROLL" ] || enroll_url=$(to_url "$MF_ENROLL" MF_ENROLL 7445)
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

asset=$SERVICE-linux-$arch
say "downloading $asset from $base"
for file in SHA256SUMS "$asset" "$SERVICE.service" "$SERVICE-sudoers"; do
    fetch "$base/$file" "$tmp/$file"
done
if [ -n "$MF_SUMS_SHA256" ]; then
    [ "$(sha256sum "$tmp/SHA256SUMS" | cut -d ' ' -f 1)" = "$MF_SUMS_SHA256" ] \
        || die "SHA256SUMS from $base does not match MF_SUMS_SHA256: another release, or tampered with; nothing was installed"
fi
verify_signature
for file in "$asset" "$SERVICE.service" "$SERVICE-sudoers"; do
    verify "$file"
done
chmod 0755 "$tmp/$asset"
version=$("$tmp/$asset" --version) || die "the downloaded $MF_ROLE does not run on this host"

create_user
install -d -o "$RUN_USER" -g "$RUN_USER" -m 0700 "$DATA_DIR"
install -d -o root -g root -m 0755 "$CONF_DIR"
# A rename, so a running service keeps its file until it restarts.
install -o root -g root -m 0755 "$tmp/$asset" "$BIN.new"
mv -f "$BIN.new" "$BIN"
install_sudoers
install -o root -g root -m 0644 "$tmp/$SERVICE.service" "$UNIT"
# A drop-in, so it outlives agent.toml edits and self-updates of the unit.
if [ "$MF_ALLOW_COMMANDS" = 1 ]; then
    install -d -m 0755 "$UNIT.d"
    printf '[Service]\nEnvironment=MONITORFLOW_COMMANDS_SHELL=true\n' >"$ALLOW_COMMANDS"
    say "this agent will run the commands AI remediation sends; MF_ALLOW_COMMANDS=0 takes that back"
elif [ "$MF_ALLOW_COMMANDS" = 0 ]; then
    rm -f "$ALLOW_COMMANDS"
    rmdir "$UNIT.d" 2>/dev/null || true
fi
systemctl daemon-reload

if [ -n "$upstream_url" ] && [ "$MF_ROLE" = relay ]; then
    write_relay_config
    say "wrote $CONF for $upstream_url"
elif [ -n "$upstream_url" ]; then
    "$BIN" --config "$CONF" init-config --upstream-url "$upstream_url" --enroll-url "$enroll_url" >/dev/null
    chown root:"$RUN_USER" "$CONF"
    chmod 0640 "$CONF"
    say "wrote $CONF for $upstream_url"
fi

systemctl enable --quiet "$SERVICE"
if enrolled; then
    systemctl restart "$SERVICE"
    say "$version installed; configuration and identity kept"
else
    # Read by systemd as root; the service's user cannot read it (ADR-0007).
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
