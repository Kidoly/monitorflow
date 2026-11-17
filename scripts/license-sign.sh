#!/usr/bin/env bash
# Signs a MonitorFlow license key (ADR-0100, ADR-0147), on the owner's own
# machine, with the key from scripts/license-keypair.sh.
#
#   scripts/license-sign.sh --key FILE --customer NAME --tier TIER --managed-servers N
#                           --expires YYYY-MM-DD [--central-servers N] [--support CONTACT]
#                           [--format 1] [--out FILE]
#
#   --tier             basic, standard, premium or enterprise.
#   --managed-servers  managed servers the subscription covers.
#   --central-servers  central servers allowed on the database, the first
#                      one included: 2 allows one additional server. More
#                      than 1 needs --tier enterprise. 1 by default.
#   --support          where to ask for support: an email address or an
#                      https:// address, shown on the License page.
#   --expires          last day it is valid (UTC).
#   --format 1         an Enterprise key in the format of ADR-0100, for a
#                      server older than ADR-0147; it says no tier, managed
#                      servers or contact.
#   --out              where to write it; standard output by default.
#
# The license key is the JSON below in base64, then the raw Ed25519
# signature of exactly those bytes, as `ee/crates/server-ee` reads them.
# Needs OpenSSL 3 and jq. The private key is only read, never copied.
set -euo pipefail

usage() {
    sed -n '5,20p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

key='' customer='' tier='' managed='' central=1 support='' expires='' format=2 out=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || usage
    case $1 in
        --key) key=$2 ;;
        --customer) customer=$2 ;;
        --tier) tier=$2 ;;
        --managed-servers) managed=$2 ;;
        # --servers is the name ADR-0100 gave it.
        --central-servers | --servers) central=$2 ;;
        --support) support=$2 ;;
        --expires) expires=$2 ;;
        --format) format=$2 ;;
        --out) out=$2 ;;
        *) usage ;;
    esac
    shift 2
done
fail() { echo "$*" >&2; exit 1; }
if [ -z "$key" ] || [ -z "$customer" ] || [ -z "$expires" ]; then usage; fi
[ -f "$key" ] || fail "$key: no such file"
[[ $central =~ ^[1-9][0-9]{0,3}$ ]] || fail "--central-servers must be a number from 1"
[[ $expires =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || fail "--expires must be a date, like 2027-11-30"
case $format in
    1)
        [ -z "$tier" ] || [ "$tier" = enterprise ] || fail "--format 1 is for Enterprise keys only"
        if [ -n "$managed" ] || [ -n "$support" ]; then fail "--format 1 says no managed servers or support contact"; fi
        tier=enterprise
        ;;
    2)
        case $tier in
            basic | standard | premium | enterprise) ;;
            '') usage ;;
            *) fail "--tier must be basic, standard, premium or enterprise" ;;
        esac
        [[ $managed =~ ^[1-9][0-9]{0,6}$ ]] || fail "--managed-servers must be a number from 1"
        [ "$central" -eq 1 ] || [ "$tier" = enterprise ] ||
            fail "only --tier enterprise allows more than one central server"
        if [ -n "$support" ]; then
            [ "${#support}" -le 200 ] || fail "--support is longer than 200 characters"
            [[ $support =~ ^https://[^[:space:]\<\>\"\'\`\\]+$ || $support =~ ^[^[:space:]:@\<\>\"\'\`\\]+@[^[:space:]:@\<\>\"\'\`\\]+\.[^[:space:]:@\<\>\"\'\`\\]+$ ]] ||
                fail "--support must be an email address or an https:// address"
        fi
        ;;
    *) fail "--format must be 1 or 2" ;;
esac
for tool in openssl jq; do
    command -v "$tool" >/dev/null || fail "this needs $tool"
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [ -r /proc/sys/kernel/random/uuid ]; then
    id=$(cat /proc/sys/kernel/random/uuid)
else
    id=$(uuidgen | tr '[:upper:]' '[:lower:]')
fi
args=(--arg id "$id" --arg customer "$customer" --argjson central "$central"
    --arg issued "$(date -u +%Y-%m-%d)" --arg expires "$expires")
if [ "$format" = 1 ]; then
    jq -cnj "${args[@]}" \
        '{format: 1, id: $id, customer: $customer, edition: "enterprise",
          server_instances: $central, issued: $issued, expires: $expires}' >"$work/license.json"
else
    jq -cnj "${args[@]}" --arg tier "$tier" --argjson managed "$managed" --arg support "$support" \
        '{format: 2, id: $id, customer: $customer, tier: $tier, managed_servers: $managed,
          server_instances: $central, issued: $issued, expires: $expires}
         + (if $support == "" then {} else {support: $support} end)' >"$work/license.json"
fi
openssl pkeyutl -sign -inkey "$key" -rawin -in "$work/license.json" -out "$work/license.sig"
# Checks the signature with the public half, as the server will.
openssl pkey -in "$key" -pubout -out "$work/public.pem"
openssl pkeyutl -verify -pubin -inkey "$work/public.pem" -rawin -in "$work/license.json" \
    -sigfile "$work/license.sig" >/dev/null

name="$(tr '[:lower:]' '[:upper:]' <<<"${tier:0:1}")${tier:1}"
if [ "$format" = 1 ]; then
    covers="$central central servers"
elif [ "$central" -gt 1 ]; then
    covers="$managed managed servers and $central central servers"
else
    covers="$managed managed servers"
fi
{
    echo "MonitorFlow $name subscription for $customer: $covers, until $expires."
    echo "Install it under Administration > License."
    echo
    echo "-----BEGIN MONITORFLOW LICENSE-----"
    openssl base64 -in "$work/license.json"
    echo "-----END MONITORFLOW LICENSE-----"
    echo "-----BEGIN MONITORFLOW LICENSE SIGNATURE-----"
    openssl base64 -in "$work/license.sig"
    echo "-----END MONITORFLOW LICENSE SIGNATURE-----"
} >"${out:-/dev/stdout}"
echo "license $id signed for $customer ($name)" >&2
