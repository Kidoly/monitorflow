#!/usr/bin/env bash
# The tested channel's file (ADR-0147): tested.json names the release
# subscribers' central servers are offered, and tested.json.sig is the raw
# Ed25519 signature of exactly its bytes by the release signing key, as
# SHA256SUMS.sig. release.yml runs it on a promotion, and to check the file
# it carries forward into a new release.
#
#   MF_SIGNING_KEY=key.pem deploy/channel-file.sh sign VERSION DIR
#   deploy/channel-file.sh verify DIR
#
# Environment:
#   MF_SIGNING_KEY   the release signing key (sign only); its public half
#                    must be in MF_RELEASE_KEYS.
#   MF_RELEASE_KEYS  the release public keys; deploy/release-keys.pem by
#                    default.
#   MF_PROMOTED      the promotion's date; today (UTC) by default.
set -euo pipefail

deploy=$(cd "$(dirname "$0")" && pwd)
keys=${MF_RELEASE_KEYS:-$deploy/release-keys.pem}
usage() {
    sed -n '8,9p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

# Whether DIR/tested.json verifies with one of the keys.
verify() {
    local dir=$1 work one
    work=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -rf '$work'" RETURN
    awk -v work="$work" '
        /-----BEGIN PUBLIC KEY-----/ { n++; out = work "/key" n ".pem" }
        out { print >out }
        /-----END PUBLIC KEY-----/ { close(out); out = "" }
    ' "$keys"
    for one in "$work"/key*.pem; do
        [ -e "$one" ] || break
        if openssl pkeyutl -verify -pubin -inkey "$one" -rawin -in "$dir/tested.json" \
            -sigfile "$dir/tested.json.sig" >/dev/null 2>&1; then
            return 0
        fi
    done
    echo "$dir/tested.json does not verify with the keys of $keys" >&2
    return 1
}

case ${1:-} in
    sign)
        [ "$#" -eq 3 ] || usage
        version=$2 dir=$3
        key=${MF_SIGNING_KEY:?MF_SIGNING_KEY must name the release signing key}
        [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
            { echo "$version is not a release version like 0.7.0; pre-releases are never promoted" >&2; exit 1; }
        promoted=${MF_PROMOTED:-$(date -u +%Y-%m-%d)}
        mkdir -p "$dir"
        jq -cnj --arg version "$version" --arg promoted "$promoted" \
            '{channel: "tested", version: $version, promoted: $promoted}' >"$dir/tested.json"
        openssl pkeyutl -sign -inkey "$key" -rawin -in "$dir/tested.json" -out "$dir/tested.json.sig"
        # A file signed with a key the servers do not trust would offer nothing.
        verify "$dir"
        cat "$dir/tested.json"
        echo
        ;;
    verify)
        [ "$#" -eq 2 ] || usage
        verify "$2"
        ;;
    *) usage ;;
esac
