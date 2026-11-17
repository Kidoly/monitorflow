#!/usr/bin/env bash
# Signing in through real identity providers (ADR-0145), in a real browser:
# Keycloak and authentik over OpenID Connect, OpenLDAP over LDAPS and
# StartTLS, all in local containers with a throwaway certificate authority
# made here. The MonitorFlow server runs from this checkout on the host, its
# web interface behind Caddy over HTTPS with the production headers.
# deploy/sso-test/browser.mjs drives Chromium through the sign-ins: first
# sign-in creates the account with its role, a group change changes the
# role, losing the groups refuses the sign-in, plain LDAP is refused, and a
# provider outage leaves the local administrator able to sign in.
#
# Needs docker with compose, curl, jq, openssl, cargo, Node with npm, and
# Chromium or Chrome (MF_TEST_CHROMIUM picks one). Removes everything it
# made on exit. Uses loopback ports 25432, 27443 to 27480 and 28389 to 29000.
#
#   deploy/sso-test.sh            build, start, test, clean up
#   MF_TEST_KEEP=1 deploy/sso-test.sh   leave it running for a look
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)

# `sso-test.sh ctl <action> ...`: what the browser script asks of the
# providers while it runs, with the state of the running test.
if [ "${1:-}" = ctl ]; then
    # shellcheck source=/dev/null
    . "$MF_SSO_ENV"
fi

compose() { docker compose -f "$here/sso-test/compose.yml" --env-file /dev/null "$@"; }

KC=https://localhost:28543
AK=https://localhost:28643
WEB=https://localhost:28443
LDAP_BASE=dc=example,dc=org

kc_token() {
    curl -fsS --cacert "$MF_SSO_CERTS/ca.pem" "$KC/realms/master/protocol/openid-connect/token" \
        -d grant_type=password -d client_id=admin-cli -d username=admin --data-urlencode "password=$KC_ADMIN_PASSWORD" |
        jq -r .access_token
}

# kc METHOD PATH [JSON]: the Keycloak admin API of the test realm.
kc() {
    local args=(-fsS --cacert "$MF_SSO_CERTS/ca.pem" -X "$1" -H "Authorization: Bearer $(kc_token)")
    [ $# -ge 3 ] && args+=(-H 'Content-Type: application/json' -d "$3")
    curl "${args[@]}" "$KC/admin/realms$2"
}

# ak METHOD PATH [JSON]: authentik's API.
ak() {
    local args=(-fsS -X "$1" -H "Authorization: Bearer $AK_TOKEN")
    [ $# -ge 3 ] && args+=(-H 'Content-Type: application/json' -d "$3")
    curl "${args[@]}" "http://127.0.0.1:29000/api/v3$2"
}

ldap_modify() {
    compose exec -T openldap ldapmodify -x -H ldap://localhost -D "cn=admin,$LDAP_BASE" -w "$LDAP_ADMIN_PASSWORD" >/dev/null
}

# Sets the Keycloak groups of `user` to exactly the ones named.
kc_groups() {
    local user=$1 id gid
    shift
    id=$(kc GET "/mf/users?username=$user&exact=true" | jq -r '.[0].id')
    for gid in $(kc GET "/mf/users/$id/groups" | jq -r '.[].id'); do
        kc DELETE "/mf/users/$id/groups/$gid" >/dev/null
    done
    for name in "$@"; do
        gid=$(kc GET "/mf/groups?search=$name&exact=true" | jq -r '.[0].id')
        kc PUT "/mf/users/$id/groups/$gid" >/dev/null
    done
}

# Sets the authentik groups of `user` to exactly the ones named.
ak_groups() {
    local user=$1 pk groups=() name
    shift
    pk=$(ak GET "/core/users/?username=$user" | jq -r '.results[0].pk')
    for name in "$@"; do
        groups+=("$(ak GET "/core/groups/?name=$name" | jq -r '.results[0].pk')")
    done
    ak PATCH "/core/users/$pk/" "$(jq -n '{groups: $ARGS.positional}' --args "${groups[@]}")" >/dev/null
}

if [ "${1:-}" = ctl ]; then
    shift
    case "$1" in
        kc-groups) shift; kc_groups "$@" ;;
        ak-groups) shift; ak_groups "$@" ;;
        ldap-member)
            # ldap-member add|delete GROUP DN
            printf 'dn: cn=%s,ou=groups,%s\nchangetype: modify\n%s: member\nmember: %s\n' \
                "$3" "$LDAP_BASE" "$2" "$4" | ldap_modify ;;
        stop) compose stop "$2" >/dev/null 2>&1 ;;
        *) echo "unknown action $1" >&2; exit 2 ;;
    esac
    exit 0
fi

for tool in docker curl jq openssl cargo npm node; do
    command -v "$tool" >/dev/null || { echo "sso-test needs $tool" >&2; exit 1; }
done
chromium=${MF_TEST_CHROMIUM:-$(command -v chromium || command -v google-chrome || command -v chromium-browser || true)}
[ -n "$chromium" ] || { echo "sso-test needs Chromium or Chrome (MF_TEST_CHROMIUM)" >&2; exit 1; }

work=$(mktemp -d)
server_pid=
cleanup() {
    status=$?
    if [ "${MF_TEST_KEEP:-}" = 1 ]; then
        echo "kept running: $WEB, state in $work"
        echo "drive the browser again: (. $work/env && MF_SSO_ENV=$work/env node $here/sso-test/browser.mjs)"
        return
    fi
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null && wait "$server_pid" 2>/dev/null
    if [ "$status" -ne 0 ]; then
        echo "--- server log"; tail -n 60 "$work/server.log" 2>/dev/null || true
        compose logs --tail 30 keycloak authentik-server openldap caddy 2>/dev/null || true
    fi
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT

secret() { openssl rand -hex 16; }
export MF_SSO_CERTS=$work/certs MF_WEB_OUT=$root/web/out
export MF_DB_PASSWORD KC_ADMIN_PASSWORD AK_DB_PASSWORD AK_SECRET_KEY AK_ADMIN_PASSWORD AK_TOKEN
export LDAP_ADMIN_PASSWORD LDAP_SERVICE_PASSWORD
MF_DB_PASSWORD=$(secret) KC_ADMIN_PASSWORD=$(secret) AK_DB_PASSWORD=$(secret)
AK_SECRET_KEY=$(openssl rand -hex 32) AK_ADMIN_PASSWORD=$(secret) AK_TOKEN=$(secret)
LDAP_ADMIN_PASSWORD=$(secret) LDAP_SERVICE_PASSWORD=$(secret)
KC_SECRET=$(secret) AK_CLIENT_SECRET=$(secret) ROOT_PASSWORD=$(secret)

# A throwaway CA and one certificate for localhost, for every TLS listener.
mkdir -p "$MF_SSO_CERTS"
(
    cd "$MF_SSO_CERTS"
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
        -subj "/CN=MonitorFlow SSO test CA" -keyout ca-key.pem -out ca.pem 2>/dev/null
    openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout leaf-key.pem -out leaf.csr 2>/dev/null
    printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' >leaf.ext
    openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -days 2 \
        -extfile leaf.ext -out leaf.pem 2>/dev/null
    # Read by the containers' own users; a throwaway key.
    chmod 644 leaf-key.pem
)

echo "building the server and the web interface"
(cd "$root" && cargo build -q -p monitorflow-server --bin monitorflow-server)
server_bin=${CARGO_TARGET_DIR:-$root/target}/debug/monitorflow-server
if [ "${MF_TEST_SKIP_WEB_BUILD:-}" != 1 ] || [ ! -d "$MF_WEB_OUT" ]; then
    (cd "$root/web" && npm ci --no-audit --no-fund --silent && npm run --silent build >/dev/null)
fi
npm install --silent --no-audit --no-fund --prefix "$work/pw" playwright-core@1.64.1 >/dev/null

echo "starting the providers"
compose up -d --quiet-pull >/dev/null 2>&1
wait_for() {
    local what=$1 i
    shift
    for i in $(seq 1 120); do
        "$@" >/dev/null 2>&1 && return 0
        sleep 2
    done
    echo "$what did not come up" >&2
    return 1
}
wait_for postgres compose exec -T postgres pg_isready -U monitorflow
wait_for keycloak curl -fsS --cacert "$MF_SSO_CERTS/ca.pem" "$KC/realms/master/.well-known/openid-configuration"
# Ready once both TLS listeners answer with the test certificate: the
# image sets TLS up after a first start without it.
ldap_ready() {
    openssl s_client -connect localhost:28636 -CAfile "$MF_SSO_CERTS/ca.pem" -verify_return_error </dev/null >/dev/null 2>&1 &&
        compose exec -T -e LDAPTLS_CACERT=/container/service/slapd/assets/certs/ca.pem openldap \
            ldapwhoami -ZZ -x -H ldap://localhost -D "cn=admin,$LDAP_BASE" -w "$LDAP_ADMIN_PASSWORD"
}
wait_for openldap ldap_ready
wait_for authentik ak GET /core/users/me/
wait_for caddy curl -fsS --cacert "$MF_SSO_CERTS/ca.pem" "$AK/-/health/ready/"

echo "filling the directory"
compose exec -T openldap ldapadd -x -H ldap://localhost -D "cn=admin,$LDAP_BASE" -w "$LDAP_ADMIN_PASSWORD" \
    <"$here/sso-test/directory.ldif" >/dev/null

echo "setting up Keycloak"
kc POST "" '{"realm": "mf", "enabled": true}'
for group in mf-admins helpdesk staff; do kc POST /mf/groups "{\"name\": \"$group\"}"; done
kc POST /mf/clients "$(jq -n --arg secret "$KC_SECRET" --arg redirect "$WEB/api/auth/oidc/callback" '{
    clientId: "monitorflow", secret: $secret, publicClient: false, standardFlowEnabled: true,
    directAccessGrantsEnabled: false, redirectUris: [$redirect],
    attributes: {"pkce.code.challenge.method": "S256"},
    protocolMappers: [{name: "groups", protocol: "openid-connect", protocolMapper: "oidc-group-membership-mapper",
        config: {"full.path": "false", "id.token.claim": "true", "access.token.claim": "true",
                 "userinfo.token.claim": "true", "claim.name": "groups"}}]}')"
kc POST /mf/users '{"username": "alice", "enabled": true, "email": "alice@example.org", "emailVerified": true,
    "firstName": "Alice", "lastName": "Example", "groups": ["mf-admins"],
    "credentials": [{"type": "password", "value": "alice-keycloak-pw", "temporary": false}]}'

echo "setting up authentik"
flow() { ak GET "/flows/instances/?slug=$1" | jq -r '.results[0].pk'; }
# Its worker applies the default flows, keys and mappings after the API is up.
ak_ready() {
    [ "$(flow default-provider-authorization-implicit-consent)" != null ] &&
        [ "$(flow default-provider-invalidation-flow)" != null ] &&
        [ "$(flow default-authentication-flow)" != null ] &&
        [ "$(ak GET '/crypto/certificatekeypairs/?has_key=true' | jq '.results | length')" -gt 0 ] &&
        [ "$(ak GET '/propertymappings/provider/scope/?page_size=100' | jq '[.results[].managed] | map(select(. != null and startswith("goauthentik.io/providers/oauth2/scope-"))) | length')" -ge 3 ]
}
wait_for "authentik's defaults" ak_ready
mappings=$(ak GET '/propertymappings/provider/scope/?page_size=100' |
    jq '[.results[] | select(.managed == "goauthentik.io/providers/oauth2/scope-openid"
        or .managed == "goauthentik.io/providers/oauth2/scope-email"
        or .managed == "goauthentik.io/providers/oauth2/scope-profile") | .pk]')
signing_key=$(ak GET '/crypto/certificatekeypairs/?has_key=true' | jq -r '.results[0].pk')
provider=$(ak POST /providers/oauth2/ "$(jq -n --arg secret "$AK_CLIENT_SECRET" --arg redirect "$WEB/api/auth/oidc/callback" \
    --arg authz "$(flow default-provider-authorization-implicit-consent)" \
    --arg inval "$(flow default-provider-invalidation-flow)" --arg key "$signing_key" --argjson mappings "$mappings" '{
    name: "MonitorFlow", authorization_flow: $authz, invalidation_flow: $inval, client_type: "confidential",
    client_id: "monitorflow", client_secret: $secret, signing_key: $key, property_mappings: $mappings,
    redirect_uris: [{matching_mode: "strict", url: $redirect}]}')" | jq -r .pk)
ak POST /core/applications/ "{\"name\": \"MonitorFlow\", \"slug\": \"monitorflow\", \"provider\": $provider}" >/dev/null
for group in mf-operators other; do ak POST /core/groups/ "{\"name\": \"$group\"}" >/dev/null; done
carol=$(ak POST /core/users/ '{"username": "carol", "name": "Carol", "email": "carol@example.org", "is_active": true}' | jq -r .pk)
ak POST "/core/users/$carol/set_password/" '{"password": "carol-authentik-pw"}' >/dev/null
ak_groups carol mf-operators

echo "starting MonitorFlow"
db="postgres://monitorflow:$MF_DB_PASSWORD@127.0.0.1:25432/monitorflow"
cat >"$work/server.toml" <<TOML
grpc_listen_addr = "127.0.0.1:27443"
enroll_listen_addr = "127.0.0.1:27445"
api_listen_addr = "127.0.0.1:27444"
mirror_listen_addr = "127.0.0.1:27480"
database_url = "$db"
data_dir = "$work/server"
server_names = ["localhost"]
web_url = "$WEB"

[tls]
ca_cert = "$work/server/tls/ca.pem"
cert = "$work/server/tls/cert.pem"
key = "$work/server/tls/key.pem"
TOML
"$server_bin" --config "$work/server.toml" >"$work/server.log" 2>&1 &
server_pid=$!
wait_for "the server" curl -fsS http://127.0.0.1:27444/api/health/ready
echo "$ROOT_PASSWORD" | "$server_bin" --config "$work/server.toml" user add --username root --role administrator >/dev/null 2>&1
wait_for "the web interface" curl -fsS --cacert "$MF_SSO_CERTS/ca.pem" "$WEB/login/"

# What `ctl` needs, for the browser script's calls back into this script.
export MF_SSO_ENV=$work/env
{
    for name in MF_SSO_CERTS KC_ADMIN_PASSWORD AK_TOKEN LDAP_ADMIN_PASSWORD; do
        printf 'export %s=%q\n' "$name" "${!name}"
    done
} >"$MF_SSO_ENV"

echo "testing the directory link from Rust"
(cd "$root" && MF_TEST_LDAP_CA="$MF_SSO_CERTS/ca.pem" MF_TEST_LDAPS_URL=ldaps://localhost:28636 \
    MF_TEST_LDAP_URL=ldap://localhost:28389 MF_TEST_LDAP_SERVICE_PASSWORD="$LDAP_SERVICE_PASSWORD" \
    cargo test -q -p monitorflow-server --test sso_ldap -- --ignored)

# What the browser script reads, kept with the run for MF_TEST_KEEP.
{
    printf 'export MF_SSO_PW=%q MF_SSO_CTL=%q MF_SSO_CHROMIUM=%q MF_SSO_WEB=%q\n' \
        "$work/pw" "$here/sso-test.sh" "$chromium" "$WEB"
    printf 'export MF_SSO_ROOT_PASSWORD=%q MF_SSO_KC_SECRET=%q MF_SSO_AK_SECRET=%q MF_SSO_LDAP_PASSWORD=%q\n' \
        "$ROOT_PASSWORD" "$KC_SECRET" "$AK_CLIENT_SECRET" "$LDAP_SERVICE_PASSWORD"
} >>"$MF_SSO_ENV"

echo "signing in through the browser"
(
    # shellcheck source=/dev/null
    . "$MF_SSO_ENV"
    node "$here/sso-test/browser.mjs"
)
echo "all sign-in checks passed"
