#!/bin/sh
# Installs, repairs, upgrades, restores or removes the MonitorFlow central
# server on a Linux host (ADR-0065): Docker if missing, the compose files of
# a signed release in /opt/monitorflow/deploy, its images by digest, and a
# printed setup link, with a browser certificate that needs no terminal
# (ADR-0068). Nothing to clone and nothing to build:
#
#   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
#
# Run it again at any time: settings and secrets stay, the stack comes back
# up, and a newer release is installed through the updater, with a backup
# and a rollback (ADR-0056).
#
# Options, after `sudo sh -s --` when piped:
#   --restore FILE   install if needed, then restore a backup (ADR-0058).
#   --uninstall      remove the containers and keep the data, or ask.
#   --purge          with --uninstall: delete the data without asking.
#   --add-server     on the first central server: let other central servers
#                    join it, and write the file they join with (ADR-0100,
#                    Enterprise edition).
#   --join FILE      on a new machine: run another central server on the
#                    database of the first, from the file --add-server wrote
#                    there (Enterprise edition).
#
# Settings, from the environment:
#   MF_PUBLIC_NAME   the name or IP address browsers and agents reach this
#                    server at, on the first install. Asked on a terminal,
#                    guessed otherwise.
#   MF_VERSION       release to install, like 0.2.0; latest by default.
#   MF_DOWNLOAD_URL  https URL of a directory holding the release files, for
#                    sites that serve them themselves; updates come from
#                    there too.
#   MF_SHARE_ADDRESS with --add-server: the address of this machine the
#                    added central servers reach; its main one by default.
set -eu

RELEASES=https://github.com/Kidoly/monitorflow/releases
ROOT=/opt/monitorflow
DEPLOY=$ROOT/deploy
ENV_FILE=$DEPLOY/.env
BUNDLE=monitorflow-server-compose.tar.gz
# What --add-server writes, for --join.
JOIN_FILE=/root/monitorflow-join.tar
LICENSE_HELP="Install its license key under Administration > License on the first central server. To get one, see https://github.com/Kidoly/monitorflow#licence"
# The compose project's fixed name, in docker-compose.yml.
PROJECT=monitorflow

MF_PUBLIC_NAME=${MF_PUBLIC_NAME:-}
MF_VERSION=${MF_VERSION:-}
MF_DOWNLOAD_URL=${MF_DOWNLOAD_URL:-}
MF_SHARE_ADDRESS=${MF_SHARE_ADDRESS:-}

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

usage() {
    printf 'usage: install-server.sh [--restore FILE | --add-server | --join FILE | --uninstall [--purge]]\n' >&2
    exit 2
}

# The compose files of this machine: an added central server has its own;
# the first one shares its stores once --add-server ran (ADR-0100).
compose() {
    if [ -f "$ENV_FILE" ] && grep -q '^JOINED=1$' "$ENV_FILE"; then
        docker compose --project-directory "$DEPLOY" -f "$DEPLOY/docker-compose.joined.yml" "$@"
    elif [ -f "$ENV_FILE" ] && grep -q '^SHARE_ADDRESS=.' "$ENV_FILE"; then
        docker compose --project-directory "$DEPLOY" -f "$DEPLOY/docker-compose.yml" \
            -f "$DEPLOY/docker-compose.shared.yml" "$@"
    else
        docker compose --project-directory "$DEPLOY" -f "$DEPLOY/docker-compose.yml" "$@"
    fi
}
joined() { [ -f "$ENV_FILE" ] && grep -q '^JOINED=1$' "$ENV_FILE"; }

# Whether a terminal can answer, which `curl | sh` does not give on stdin.
has_tty() { (exec </dev/tty) 2>/dev/null; }

# Prints the answer to the question $1, read from the terminal.
ask() {
    printf '%s' "$1" >/dev/tty
    answer=
    read -r answer </dev/tty || true
    printf '%s' "$answer"
}

try_fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only -O "$2" "$1"
    else
        die "curl or wget is needed to download MonitorFlow"
    fi
}

fetch() { try_fetch "$1" "$2" || die "cannot download $1"; }

# Checks SHA256SUMS.sig against the release keys with OpenSSL 3, which
# covers the compose files and the image digests of images.json. Without
# OpenSSL 3 only the checksums, from the same HTTPS source as this script.
verify_signature() {
    case $RELEASE_KEYS in
        *'BEGIN PUBLIC KEY'*) ;;
        *) die "this copy of the installer carries no release key: use the one published with a release" ;;
    esac
    if ! command -v openssl >/dev/null 2>&1 || ! openssl pkeyutl -help 2>&1 | grep -q -- -rawin; then
        say "warning: OpenSSL 3 is not installed, so the release signature is not checked; checking checksums only"
        return
    fi
    fetch "$base/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig"
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

# Fails unless the file $1 of $2 ($tmp by default) has the checksum
# SHA256SUMS lists for it.
verify() {
    expected=$(awk -v name="$1" '$2 == name || $2 == "*" name { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || die "$1 is not listed in SHA256SUMS"
    actual=$(sha256sum "${2:-$tmp}/$1" | cut -d ' ' -f 1)
    [ "$actual" = "$expected" ] || die "checksum mismatch for $1: the download is corrupt or was tampered with; nothing was installed"
}

# `key reference` lines of images.json, references by digest only (ADR-0056).
image_lines() {
    tr -s '{},' '\n' <"$tmp/images.json" \
        | sed -n 's/^[[:space:]]*"\([a-z]*\)":[[:space:]]*"\([a-zA-Z0-9./:_-]*@sha256:[0-9a-f]\{64\}\)"[[:space:]]*$/\1 \2/p'
}

# SERVER_IMAGE for `server`: the line of `deploy/.env` that pins an image.
image_key() { printf '%s_IMAGE' "$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]')"; }

# What this release is, verified: nothing on the host has changed yet.
download_release() {
    say "downloading the release from $base"
    for file in SHA256SUMS VERSION images.json "$BUNDLE"; do
        fetch "$base/$file" "$tmp/$file"
    done
    verify_signature
    for file in VERSION images.json "$BUNDLE"; do
        verify "$file"
    done
    version=$(cat "$tmp/VERSION")
    grep -q "\"version\": *\"$version\"" "$tmp/images.json" || die "images.json is not for version $version"
    for image in server web updater mirror; do
        image_lines | grep -q "^$image " || die "images.json names no $image image by digest: this release has no server images"
    done
}

ensure_docker() {
    if ! command -v docker >/dev/null 2>&1; then
        say "Docker is not installed: installing it with Docker's official script (https://get.docker.com), which takes a minute or two"
        fetch https://get.docker.com "$tmp/get-docker.sh"
        if ! sh "$tmp/get-docker.sh" >"$tmp/get-docker.log" 2>&1; then
            tail -n 20 "$tmp/get-docker.log" >&2
            die "Docker could not be installed on this distribution: install Docker Engine and its compose plugin with your package manager, then run this again"
        fi
    fi
    if ! docker info >/dev/null 2>&1; then
        say "starting Docker"
        systemctl enable --now docker >/dev/null 2>&1 || service docker start >/dev/null 2>&1 || true
        waited=0
        until docker info >/dev/null 2>&1; do
            [ "$waited" -lt 30 ] || die "Docker does not start here; see: systemctl status docker"
            sleep 1
            waited=$((waited + 1))
        done
    fi
    docker compose version >/dev/null 2>&1 \
        || die "Docker's compose plugin is missing: install docker-compose-plugin (docker-compose-v2 on Ubuntu's own packages), then run this again"
}

data_exists() { docker volume inspect "${PROJECT}_pgdata" >/dev/null 2>&1; }

primary_ip() {
    ip=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -n 1) || true
    [ -n "$ip" ] || ip=$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -m 1 '^[0-9.]*$') || true
    printf '%s' "$ip"
}

# Whether a public CA could ever issue for the name $1: not for an address,
# a single label, or a name reserved for private networks, whatever it
# resolves to here.
acme_name() {
    lower=$(printf '%s' "${1%.}" | tr '[:upper:]' '[:lower:]')
    case $lower in
        *[!0-9.]*) ;;
        *) return 1 ;;
    esac
    case $lower in
        *.*) ;;
        *) return 1 ;;
    esac
    case $lower in
        *.lan | *.local | *.localdomain | *.localhost | *.internal | *.intranet | *.home | *.corp \
            | *.home.arpa | *.test | *.example | *.invalid) return 1 ;;
    esac
}

# Whether an address of the name $1 is on the internet. Only a DNS view from
# this host: whether port 80 answers from outside shows when ACME runs.
resolves_publicly() {
    for addr in $(getent ahosts "$1" 2>/dev/null | awk '{ print $1 }' | sort -u); do
        case $addr in
            *:*)
                case $addr in 2* | 3*) return 0 ;; esac
                ;;
            0.* | 10.* | 127.* | 169.254.* | 192.168.* | 172.1[6-9].* | 172.2[0-9].* | 172.3[01].* \
                | 100.6[4-9].* | 100.[7-9][0-9].* | 100.1[01][0-9].* | 100.12[0-7].*) ;;
            *) return 0 ;;
        esac
    done
    return 1
}

public_name() { acme_name "$1" && resolves_publicly "$1"; }

# The host's full name if it resolves to a public address, where Caddy can
# get a certificate by ACME; else its IP address, which always works, with
# a certificate from Caddy's own CA.
guess_name() {
    fqdn=$(hostname -f 2>/dev/null) || fqdn=
    if public_name "$fqdn"; then
        printf '%s' "$fqdn"
        return
    fi
    ip=$(primary_ip)
    if [ -n "$ip" ]; then printf '%s' "$ip"; else hostname; fi
}

valid_name() {
    case $1 in
        '' | -* | *[!A-Za-z0-9.-]*) return 1 ;;
    esac
}

# Sets $name: MF_PUBLIC_NAME, else what the terminal says, else the guess.
choose_name() {
    if [ -n "$MF_PUBLIC_NAME" ]; then
        name=$MF_PUBLIC_NAME
        valid_name "$name" || die "MF_PUBLIC_NAME must be a DNS name or an IPv4 address, like monitor.example.com"
    elif has_tty; then
        guess=$(guess_name)
        printf 'Browsers and agents need a name or IP address to reach this server.\n' >/dev/tty
        [ "$mode" != restore ] || printf 'Use the one of the old server, so agents find this one.\n' >/dev/tty
        while :; do
            name=$(ask "Public name [$guess]: ")
            name=${name:-$guess}
            valid_name "$name" && break
            printf 'A DNS name or an IPv4 address, like monitor.example.com.\n' >/dev/tty
        done
    else
        name=$(guess_name)
        say "using $name as the public name; set MF_PUBLIC_NAME to choose another"
    fi
}

install_bundle() {
    install -d -m 0755 "$ROOT"
    tar -xzf "$tmp/$BUNDLE" -C "$ROOT" --no-same-owner
}

# The only secret is the database's: the server keeps its keys in a volume.
write_env() {
    password=$(od -An -tx1 -N24 /dev/urandom | tr -d ' \n')
    ip=$(primary_ip)
    names=$name
    [ -z "$ip" ] || [ "$ip" = "$name" ] || names=$name,$ip
    (
        umask 077
        {
            printf '# Written by install-server.sh. Settings: .env.example, next to this file.\n'
            printf '# Run the installer again after a change.\n'
            printf 'DOMAIN=%s\nSERVER_NAMES=%s\nPOSTGRES_PASSWORD=%s\n' "$name" "$names" "$password"
            [ -z "$MF_DOWNLOAD_URL" ] || printf 'RELEASE_URL=%s\n' "$base"
            image_lines | while read -r image reference; do
                printf '%s=%s\n' "$(image_key "$image")" "$reference"
            done
        } >"$ENV_FILE.new"
    )
    mv -f "$ENV_FILE.new" "$ENV_FILE"
}

# The value of $1 in .env, or $2.
env_get() {
    value=$(sed -n "s/^$1=//p" "$ENV_FILE" | tail -n 1)
    printf '%s' "${value:-$2}"
}

# Sets $1 to $2 in .env, which stays private.
env_set() {
    (
        umask 077
        { grep -v "^$1=" "$ENV_FILE" || true; printf '%s=%s\n' "$1" "$2"; } >"$ENV_FILE.new"
    )
    mv -f "$ENV_FILE.new" "$ENV_FILE"
}

# WEB_CERTIFICATE follows the name each time, so a name that gets public DNS
# or open ports moves to ACME by running this again (ADR-0068). A name that
# only stopped resolving keeps its setting: a DNS failure must not replace a
# certificate that works. $1 is the setting for a server without one.
choose_certificate() {
    domain=$(env_get DOMAIN '')
    if public_name "$domain"; then
        certificate=acme
    elif acme_name "$domain"; then
        certificate=$(env_get WEB_CERTIFICATE "$1")
    else
        certificate=internal
    fi
    [ "$(env_get WEB_CERTIFICATE '')" = "$certificate" ] || env_set WEB_CERTIFICATE "$certificate"
    if [ "$certificate" = acme ]; then
        say "browser certificate: a public one by ACME, as $domain resolves publicly"
    elif acme_name "$domain"; then
        say "browser certificate: this server's own CA, as $domain does not resolve to a public address"
    else
        say "browser certificate: this server's own CA, as public CAs do not issue for $domain"
    fi
}

# A message that names the port, before compose fails with less to say.
check_ports() {
    command -v ss >/dev/null 2>&1 || return 0
    [ -z "$(compose ps -q 2>/dev/null)" ] || return 0
    listening=$(ss -Hltn | awk '{ print $4 }')
    for setting in HTTPS_PORT:443 HTTP_PORT:80 GRPC_PORT:7443 ENROLL_PORT:7445 MIRROR_PORT:7480; do
        port=$(env_get "${setting%:*}" "${setting#*:}")
        if printf '%s\n' "$listening" | grep -q ":$port\$"; then
            die "port $port is already in use on this machine (see: ss -ltnp): free it, or set ${setting%:*} in $ENV_FILE, then run this again"
        fi
    done
}

# All the images, so a restore finds them too. The release's come one by
# one: for a service with `build:`, `compose pull` advises building it.
pull_images() {
    say "pulling MonitorFlow's images"
    grep -E '^[A-Z]+_IMAGE=' "$ENV_FILE" | cut -d = -f 2- | while read -r reference; do
        docker pull -q "$reference" >/dev/null 2>"$tmp/pull.log" || {
            cat "$tmp/pull.log" >&2
            die "cannot pull $reference: check that this machine reaches its registry"
        }
    done
    compose pull --quiet --ignore-buildable >"$tmp/pull.log" 2>&1 || {
        cat "$tmp/pull.log" >&2
        die "cannot pull the database images: check that this machine reaches Docker Hub"
    }
}

# Pins the images this release adds to an install from before them, which
# the new compose file needs: an older updater does not know them. A pin
# already there is the updater's.
pin_new_images() {
    image_lines | while read -r image reference; do
        key=$(image_key "$image")
        [ -z "$(env_get "$key" '')" ] || continue
        say "pulling the $image image, new in this release"
        docker pull -q "$reference" >/dev/null 2>"$tmp/pull.log" || {
            cat "$tmp/pull.log" >&2
            die "cannot pull $reference: check that this machine reaches its registry"
        }
        printf '%s=%s\n' "$key" "$reference" >>"$ENV_FILE"
    done
}

start() {
    say "starting MonitorFlow"
    if ! compose up -d --no-build --wait --wait-timeout 300 >"$tmp/up.log" 2>&1; then
        tail -n 20 "$tmp/up.log" >&2
        compose ps -a >&2 || true
        compose logs --tail 20 server >&2 || true
        if joined && compose logs server 2>/dev/null | grep -q 'additional central server does not start'; then
            die "this central server was not added: the lines above say why. $LICENSE_HELP"
        fi
        die "MonitorFlow did not start; the lines above say why. Fix it, then run this again: settings and data are kept"
    fi
}

running_version() { compose exec -T server monitorflow-server --version | cut -d ' ' -f 2; }

# Every file of the release in $tmp/release, checked.
download_release_files() {
    [ ! -d "$tmp/release" ] || return 0
    mkdir "$tmp/release"
    awk '{ sub(/^\*/, "", $2); print $2 }' "$tmp/SHA256SUMS" | while read -r file; do
        fetch "$base/$file" "$tmp/release/$file"
        verify "$file" "$tmp/release"
    done
    cp "$tmp/SHA256SUMS" "$tmp/release/"
    [ ! -f "$tmp/SHA256SUMS.sig" ] || cp "$tmp/SHA256SUMS.sig" "$tmp/release/"
}

# Updates through the updater, as the web interface does: it checks the
# release again, backs up, and rolls back. Its image is replaced last, by
# a container of its own, which `up` must not run into.
update_server() {
    say "updating from $1 to $version, with a backup first"
    download_release_files
    compose run --rm --no-deps -T -v "$tmp/release:/release:ro" updater update --dir /release \
        || die "the update failed and the updater put the previous version back, or says above what to restore"
    updater=$(env_get UPDATER_IMAGE '')
    waited=0
    until [ "$(docker inspect --format '{{.Config.Image}}' "$(compose ps -q updater)" 2>/dev/null)" = "$updater" ]; do
        [ "$waited" -lt 60 ] || break
        sleep 1
        waited=$((waited + 1))
    done
}

# The mirrors volume (ADR-0067) hides the directory that held the agent
# release cache, so the first start with it needs this release imported
# again. Hosts can still install from GitHub if that fails.
refill_agent_cache() {
    compose exec -T server monitorflow-server agent-release list | grep -qxF "$version" && return 0
    say "caching release $version on the server for its agents"
    download_release_files
    if ! { compose cp "$tmp/release" server:/tmp/release \
        && compose exec -T server monitorflow-server agent-release import --dir /tmp/release; } >"$tmp/import.log" 2>&1; then
        tail -n 5 "$tmp/import.log" >&2
        say "warning: the agent release is not cached; run this installer again"
    fi
}

# Brings an installed server up, then to this release if it is newer. The
# compose files follow the release, as the updater changes images only.
repair_or_upgrade() {
    [ -f "$DEPLOY/docker-compose.yml" ] || install_bundle
    had_mirrors=$(grep -cF 'monitorflow-mirror:local' "$DEPLOY/docker-compose.yml") || true
    pin_new_images
    check_ports
    start
    running=$(running_version)
    if [ "$running" = "$version" ]; then
        install_bundle
    elif [ "$(printf '%s\n%s\n' "$running" "$version" | sort -V | tail -n 1)" = "$version" ]; then
        update_server "$running"
        install_bundle
    else
        say "$running runs, newer than release $version: nothing to update"
        return
    fi
    start
    [ "$had_mirrors" != 0 ] || refill_agent_cache
}

restore_backup() {
    if [ "$installed" = 1 ] && data_exists && has_tty; then
        case $(ask "This replaces the data of this server with the backup. Continue? [y/N] ") in
            y | Y | yes) ;;
            *) die "nothing was changed" ;;
        esac
    fi
    say "restoring $backup"
    "$DEPLOY/restore.sh" "$backup" || die "the restore failed; the updater says why above"
}

# Asks the web interface on this host; $1 is what curl needs to succeed.
web_request() {
    port=$(env_get HTTPS_PORT 443)
    curl "$1" --max-time 5 --connect-to "::127.0.0.1:$port" -o /dev/null \
        "https://$(env_get DOMAIN ''):$port/api/health/ready"
}

# Caddy may still be fetching its certificate when compose is done.
wait_for_web() {
    waited=0
    until web_request -fsk; do
        [ "$waited" -lt 120 ] || return 1
        sleep 3
        waited=$((waited + 3))
    done
}

# ACME fails if port 80 or 443 is closed to the internet, which nothing here
# can see beforehand. A server with no certificate at all is what must not
# happen: its own CA works now, and a later run tries ACME again. Once there
# is a certificate, Caddy renews it and this does not apply.
settle_certificate() {
    command -v curl >/dev/null 2>&1 || return 0
    wait_for_web && return
    if [ "$(env_get WEB_CERTIFICATE '')" = acme ] && ! web_request -sk; then
        say "warning: no certificate from a public CA after two minutes. Ports 80 and 443 must be open to the internet for that. Using this server's own CA for now; once they are open, run this installer again"
        env_set WEB_CERTIFICATE internal
        start
        wait_for_web && return
    fi
    say "warning: the web interface does not answer on this machine yet; see: cd $ROOT && docker compose -f deploy/docker-compose.yml logs web"
}

# The fingerprint of the internal CA, as the web service wrote it, which
# happens just after Caddy creates the CA.
ca_fingerprint() {
    waited=0
    until compose exec -T web cat /srv/monitorflow/ca-fingerprint 2>/dev/null; do
        [ "$waited" -lt 15 ] || return 0
        sleep 1
        waited=$((waited + 1))
    done
}

# With the internal CA: the page that makes browsers accept it, and what an
# administrator compares that page with (ADR-0068).
report_trust() {
    say "open this page once on each computer, so that browsers trust this server:"
    http_port=$(env_get HTTP_PORT 80)
    trust=http://$domain/trust/
    [ "$http_port" = 80 ] || trust=http://$domain:$http_port/trust/
    printf '\n    %s\n\n' "$trust"
    fingerprint=$(ca_fingerprint)
    if [ -n "$fingerprint" ]; then
        say "that page arrives unencrypted: it is genuine only if it shows this fingerprint (SHA-256):"
        printf '\n    %s\n\n' "$fingerprint"
    fi
}

report() {
    domain=$(env_get DOMAIN '')
    port=$(env_get HTTPS_PORT 443)
    url=https://$domain/
    [ "$port" = 443 ] || url=https://$domain:$port/
    link=$(compose exec -T server monitorflow-server setup-link 2>/dev/null \
        | grep -o 'https://[^ "]*/setup/#[^ "]*' | tail -n 1) || link=
    say "MonitorFlow $(running_version) is running at $url"
    [ "$(env_get WEB_CERTIFICATE '')" != internal ] || report_trust
    if [ -n "$link" ]; then
        say "create the administrator with this link, once, within 24 hours:"
        printf '\n    %s\n\n' "$link"
    fi
    say "settings are in $ENV_FILE; run this installer again after a change, or to upgrade"
}

# Asks the running server whether the license has room for one more
# central server; says why not and stops, before anything changes.
check_license() {
    if ! answer=$("$@" 2>&1); then
        printf '%s\n' "$answer" | tail -n 3 >&2
        die "no central server was added. $LICENSE_HELP"
    fi
    say "$(printf '%s\n' "$answer" | tail -n 1)"
}

# On the first central server: publishes its stores on its address, and
# writes what another central server needs to join it. The file holds the
# certificate authority's key.
add_server() {
    joined && die "this is an added central server: run --add-server on the first one"
    check_license compose exec -T server monitorflow-server license check-add
    share=${MF_SHARE_ADDRESS:-$(env_get SHARE_ADDRESS "$(primary_ip)")}
    [ -n "$share" ] || die "cannot tell this machine's address: set MF_SHARE_ADDRESS"
    [ "$(env_get SHARE_ADDRESS '')" = "$share" ] || env_set SHARE_ADDRESS "$share"
    say "sharing the database and the metric and log stores on $share, ports 5432, 8428 and 9428, with TLS and a password; allow only the added central servers to reach them"
    start
    mkdir "$tmp/join"
    compose exec -T server tar -C /var/lib/monitorflow -cf - server/ca storage-tls >"$tmp/join/server.tar" \
        || die "cannot read the certificate authority from the server"
    compose exec -T server cat /certs/ca.pem >"$tmp/join/db-ca.pem" || die "cannot read the database's CA"
    if [ "$(env_get WEB_CERTIFICATE '')" = internal ]; then
        compose exec -T web tar -C /data/caddy/pki/authorities -cf - local >"$tmp/join/caddy.tar" \
            || die "cannot read the browser certificate authority"
    fi
    {
        printf 'FIRST_ADDRESS=%s\n' "$share"
        for key in DOMAIN POSTGRES_PASSWORD WEB_CERTIFICATE RELEASE_URL AGENT_DOWNLOAD_URL HTTPS_PORT HTTP_PORT \
            GRPC_PORT ENROLL_PORT MIRROR_PORT; do
            value=$(env_get "$key" '')
            [ -z "$value" ] || printf '%s=%s\n' "$key" "$value"
        done
    } >"$tmp/join/env"
    (umask 077 && tar -cf "$JOIN_FILE.new" -C "$tmp/join" .)
    mv -f "$JOIN_FILE.new" "$JOIN_FILE"
}

join_instructions() {
    say "wrote $JOIN_FILE. It holds the keys of this server's certificate authority: copy it to the new machine, then run there:"
    printf '\n    curl -fsSL %s/install-server.sh | sudo sh -s -- --join %s\n\n' "$base" "$(basename "$JOIN_FILE")"
    say "then delete both copies of the file"
}

# The settings of the join file in $tmp/join, with this machine's own.
write_joined_env() {
    ip=$(primary_ip)
    domain=$(sed -n 's/^DOMAIN=//p' "$tmp/join/env")
    names=$domain
    [ -z "$ip" ] || [ "$ip" = "$domain" ] || names=$domain,$ip
    (
        umask 077
        {
            printf '# Written by install-server.sh --join: an added central server (ADR-0100).\n'
            printf 'JOINED=1\nINSTANCE_NAME=%s\nSERVER_NAMES=%s\n' "$(hostname)" "$names"
            grep -E '^[A-Z_]+=' "$tmp/join/env"
            image_lines | while read -r image reference; do
                printf '%s=%s\n' "$(image_key "$image")" "$reference"
            done
        } >"$ENV_FILE.new"
    )
    mv -f "$ENV_FILE.new" "$ENV_FILE"
}

# Fills the volumes of an added central server from the join file.
copy_join_data() {
    compose run --rm --no-deps -T --user 0 --entrypoint tar server -C /var/lib/monitorflow -xf - \
        <"$tmp/join/server.tar" >/dev/null || die "cannot copy the certificate authority"
    compose run --rm --no-deps -T --user 0 -v "${PROJECT}_dbcerts:/dbcerts" --entrypoint sh server \
        -c 'cat >/dbcerts/ca.pem' <"$tmp/join/db-ca.pem" >/dev/null || die "cannot copy the database's CA"
    if [ -f "$tmp/join/caddy.tar" ]; then
        compose run --rm --no-deps -T --user 0 --entrypoint sh web \
            -c 'mkdir -p /data/caddy/pki/authorities && tar -C /data/caddy/pki/authorities -xf -' \
            <"$tmp/join/caddy.tar" >/dev/null || die "cannot copy the browser certificate authority"
    fi
}

# On a new machine: another central server on the first one's database.
join_server() {
    [ ! -f "$ENV_FILE" ] || joined || die "this machine already runs a central server of its own"
    if joined; then
        say "already added: updating this central server to $version"
        rewrite_image_pins
        pull_images
    else
        mkdir "$tmp/join"
        tar -xf "$join_file" -C "$tmp/join" || die "$join_file is not a file written by --add-server"
        [ -f "$tmp/join/env" ] && [ -f "$tmp/join/server.tar" ] || die "$join_file is not a file written by --add-server"
        say "adding this machine as a central server of $(sed -n 's/^DOMAIN=//p' "$tmp/join/env")"
        install_bundle
        write_joined_env
        pull_images
        copy_join_data
        check_license compose run --rm --no-deps -T server license check-add
    fi
    start
}

# An added central server follows the release it is given: the images of
# this one, pulled, then started.
rewrite_image_pins() {
    install_bundle
    image_lines | while read -r image reference; do
        env_set "$(image_key "$image")" "$reference"
    done
}

uninstall() {
    [ -d "$ROOT" ] || { command -v docker >/dev/null 2>&1 && data_exists; } \
        || die "MonitorFlow is not installed on this machine"
    if [ "$purge" != 1 ] && has_tty; then
        [ "$(ask "Delete the data too: database, metrics, logs, backups and the agents' CA? Type delete, or press Enter to keep it: ")" != delete ] || purge=1
    fi
    if command -v docker >/dev/null 2>&1; then
        if [ "$purge" = 1 ]; then
            docker compose -p "$PROJECT" down --volumes --remove-orphans
        else
            docker compose -p "$PROJECT" down --remove-orphans
        fi
    fi
    if joined; then
        say "this was an added central server: free its place under Administration > License"
    fi
    if [ "$purge" = 1 ]; then
        rm -rf "$ROOT"
        say "MonitorFlow and its data are removed; Docker and the downloaded images stay"
    else
        rm -rf "$DEPLOY/docker" "$DEPLOY/docker-compose.yml" "$DEPLOY/restore.sh" "$DEPLOY/.env.example"
        say "MonitorFlow is removed; its data stays in Docker volumes and its settings in $ENV_FILE, and running the installer again brings all of it back. To delete them: --uninstall --purge"
    fi
}

mode=install
purge=
backup=
join_file=
while [ "$#" -gt 0 ]; do
    case $1 in
        --uninstall) mode=uninstall ;;
        --purge) purge=1 ;;
        --restore)
            [ "$#" -ge 2 ] || usage
            mode=restore
            backup=$2
            shift
            ;;
        --add-server) mode=add ;;
        --join)
            [ "$#" -ge 2 ] || usage
            mode='join'
            join_file=$2
            shift
            ;;
        *) usage ;;
    esac
    shift
done
[ -z "$purge" ] || [ "$mode" = uninstall ] || usage

[ "$(id -u)" -eq 0 ] || die "run as root, like: curl -fsSL …/install-server.sh | sudo sh"
[ "$(uname -s)" = Linux ] || die "this installer is for Linux"

if [ "$mode" = uninstall ]; then
    uninstall
    exit 0
fi

# What the installer cannot do without, said before anything changes.
case $(uname -m) in
    x86_64 | amd64 | aarch64 | arm64) ;;
    *) die "no server images for $(uname -m); amd64 and arm64 are available" ;;
esac
for tool in awk gzip sha256sum tar; do
    command -v "$tool" >/dev/null 2>&1 || die "this installer needs $tool: install it with your package manager and run the installer again"
done
if [ "$mode" = restore ]; then
    [ -f "$backup" ] || die "$backup: no such file"
fi
if [ "$mode" = join ]; then
    [ -f "$join_file" ] || die "$join_file: no such file. On the first central server, run this installer with --add-server; it writes the file to copy here"
    join_file=$(cd "$(dirname "$join_file")" && pwd)/$(basename "$join_file")
fi
if [ "$mode" = add ] && [ ! -f "$ENV_FILE" ]; then
    die "MonitorFlow is not installed on this machine: install it first, then run --add-server here"
fi

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

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

download_release
ensure_docker

if [ "$mode" = join ] || joined; then
    [ "$mode" != restore ] && [ "$mode" != add ] || die "this is an added central server: run that on the first one"
    join_file=${join_file:-}
    join_server
    settle_certificate
    report
    say "this machine is a central server of $(env_get DOMAIN ''): add its address $(primary_ip) to that name in your DNS, so agents and browsers reach either. Back up and update the first central server; then run this installer again here"
    exit 0
fi

installed=0
if [ -f "$ENV_FILE" ]; then
    installed=1
    say "already installed: settings and secrets in $ENV_FILE are kept"
    [ -z "$MF_PUBLIC_NAME" ] || say "MF_PUBLIC_NAME is not used: edit DOMAIN and SERVER_NAMES in $ENV_FILE to change the name"
else
    data_exists && die "this machine has MonitorFlow data in Docker volumes but no $ENV_FILE, which holds its database password. To start over and delete that data, run this with --uninstall --purge"
    choose_name
    say "installing MonitorFlow $version in $DEPLOY"
    install_bundle
    write_env
fi
# Before this setting existed Caddy tried ACME for such a name by itself.
if [ "$installed" = 1 ]; then choose_certificate acme; else choose_certificate internal; fi

if [ "$mode" = restore ]; then
    [ -f "$DEPLOY/docker-compose.yml" ] || install_bundle
    check_ports
    pull_images
    restore_backup
elif [ "$installed" = 1 ]; then
    repair_or_upgrade
    [ "$mode" != add ] || add_server
else
    check_ports
    pull_images
    start
fi
settle_certificate
report
[ "$mode" != add ] || join_instructions
