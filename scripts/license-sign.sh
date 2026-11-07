#!/usr/bin/env bash
# Signs a MonitorFlow Enterprise license key (ADR-0100), on the owner's own
# machine, with the key from scripts/license-keypair.sh.
#
#   scripts/license-sign.sh --key FILE --customer NAME --servers N --expires YYYY-MM-DD [--out FILE]
#
#   --servers   central servers allowed on the database, the first one
#               included: 2 allows one additional server.
#   --expires   last day it is valid (UTC).
#   --out       where to write it; standard output by default.
#
# The license key is the JSON below in base64, then the raw Ed25519
# signature of exactly those bytes, as `ee/crates/server-ee`
# reads them. Needs OpenSSL 3 and jq.
set -euo pipefail

usage() {
    sed -n '5,10p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

key='' customer='' servers='' expires='' out=''
while [ "$#" -gt 0 ]; do
    [ "$#" -ge 2 ] || usage
    case $1 in
        --key) key=$2 ;;
        --customer) customer=$2 ;;
        --servers) servers=$2 ;;
        --expires) expires=$2 ;;
        --out) out=$2 ;;
        *) usage ;;
    esac
    shift 2
done
if [ -z "$key" ] || [ -z "$customer" ] || [ -z "$servers" ] || [ -z "$expires" ]; then usage; fi
[ -f "$key" ] || { echo "$key: no such file" >&2; exit 1; }
[[ $servers =~ ^[1-9][0-9]{0,3}$ ]] || { echo "--servers must be a number from 1" >&2; exit 1; }
[[ $expires =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || { echo "--expires must be a date, like 2027-11-30" >&2; exit 1; }
for tool in openssl jq; do
    command -v "$tool" >/dev/null || { echo "this needs $tool" >&2; exit 1; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if [ -r /proc/sys/kernel/random/uuid ]; then
    id=$(cat /proc/sys/kernel/random/uuid)
else
    id=$(uuidgen | tr '[:upper:]' '[:lower:]')
fi
jq -cnj --arg id "$id" --arg customer "$customer" --argjson servers "$servers" \
    --arg issued "$(date -u +%Y-%m-%d)" --arg expires "$expires" \
    '{format: 1, id: $id, customer: $customer, edition: "enterprise",
      server_instances: $servers, issued: $issued, expires: $expires}' >"$work/license.json"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$work/license.json" -out "$work/license.sig"
# Checks the signature with the public half, as the server will.
openssl pkey -in "$key" -pubout -out "$work/public.pem"
openssl pkeyutl -verify -pubin -inkey "$work/public.pem" -rawin -in "$work/license.json" \
    -sigfile "$work/license.sig" >/dev/null

{
    echo "MonitorFlow Enterprise license for $customer: $servers central servers, until $expires."
    echo "Install it under Administration > License."
    echo
    echo "-----BEGIN MONITORFLOW LICENSE-----"
    openssl base64 -in "$work/license.json"
    echo "-----END MONITORFLOW LICENSE-----"
    echo "-----BEGIN MONITORFLOW LICENSE SIGNATURE-----"
    openssl base64 -in "$work/license.sig"
    echo "-----END MONITORFLOW LICENSE SIGNATURE-----"
} >"${out:-/dev/stdout}"
echo "license $id signed for $customer" >&2
