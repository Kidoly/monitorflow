#!/bin/sh
# Entry point of the web image: Caddy, then, with the internal CA, the trust
# page of ADR-0068. Caddy creates the CA as it starts, and only this script
# can compute the fingerprint the page shows: templates have no SHA-256, and
# a browser refuses WebCrypto to a page served over plain HTTP.
set -eu

ca=/data/caddy/pki/authorities/local/root.crt
out=/srv/monitorflow

# Copies what is public out of /data, where the CA's key stays.
publish_trust_page() {
    until grep -q 'END CERTIFICATE' "$ca" 2>/dev/null; do sleep 1; done
    # The SHA-256 of the DER form, as browsers and systems display it.
    fingerprint=$(sed '/-----/d' "$ca" | base64 -d | sha256sum | cut -d ' ' -f 1 \
        | tr a-f A-F | sed 's/../&:/g; s/:$//')
    domain=${MONITORFLOW_DOMAIN:-localhost}
    port=${MONITORFLOW_HTTPS_PORT:-443}
    url=https://$domain
    [ "$port" = 443 ] || url=$url:$port
    mkdir -p "$out/public/trust"
    cp "$ca" "$out/public/trust/monitorflow-ca.crt"
    sed -e "s|@DOMAIN@|$domain|g" -e "s|@URL@|$url|g" -e "s|@FINGERPRINT@|$fingerprint|g" \
        /etc/caddy/trust.html >"$out/public/trust/index.html"
    # What the installer prints; out of the public directory.
    printf '%s\n' "$fingerprint" >"$out/ca-fingerprint"
}

if [ "${MONITORFLOW_CERTIFICATE:-acme}" = internal ]; then
    publish_trust_page &
fi
exec "$@"
