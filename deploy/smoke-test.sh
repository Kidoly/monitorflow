#!/usr/bin/env bash
# Starts deploy/docker-compose.yml under its own project name and ports,
# checks that each part answers and that the server's database link uses TLS
# (ADR-0051), then removes everything it created. Needs docker with compose,
# curl and openssl. CI runs it on every pull request.
set -euo pipefail
cd "$(dirname "$0")"

export DOMAIN=localhost SERVER_NAMES=localhost
POSTGRES_PASSWORD=$(openssl rand -hex 16)
export POSTGRES_PASSWORD
export HTTPS_PORT=18443 HTTP_PORT=18080 GRPC_PORT=17443 ENROLL_PORT=17445

# --env-file /dev/null: a real deploy/.env must not leak into the test.
compose() { docker compose -p monitorflow-smoke --env-file /dev/null -f docker-compose.yml "$@"; }
trap 'compose down -v --remove-orphans >/dev/null 2>&1 || true' EXIT

compose up -d --build --wait --wait-timeout 600

failed=0
check() {
    printf '%-45s' "$1"
    shift
    if "$@" >/dev/null 2>&1; then echo ok; else echo FAILED; failed=1; fi
}

# The certificate a TLS listener presents names localhost. No client
# certificate is sent: the gRPC listener still shows its own first.
cert_names_localhost() {
    openssl s_client -connect "localhost:$1" -alpn h2 </dev/null 2>/dev/null \
        | openssl x509 -noout -ext subjectAltName | grep -q 'DNS:localhost'
}
# Every connection of the server's pool, seen by PostgreSQL, is encrypted.
db_link_uses_tls() {
    [ "$(compose exec -T postgres psql -U monitorflow -d monitorflow -tAc \
        "SELECT count(*) > 0 AND bool_and(s.ssl) FROM pg_stat_ssl s JOIN pg_stat_activity a USING (pid)
         WHERE a.backend_type = 'client backend' AND a.pid <> pg_backend_pid()")" = t ]
}
# No port of the service is bound on the host.
not_published() {
    local ports
    ports=$(docker inspect -f '{{json .NetworkSettings.Ports}}' "$(compose ps -q "$1")")
    ! grep -q HostPort <<<"$ports"
}

check "API ready behind Caddy" curl -fsk "https://localhost:$HTTPS_PORT/api/health/ready"
check "web interface served" curl -fsk -o /dev/null "https://localhost:$HTTPS_PORT/"
check "enrollment listener: certificate" cert_names_localhost "$ENROLL_PORT"
check "gRPC listener: certificate" cert_names_localhost "$GRPC_PORT"
check "database connections use TLS" db_link_uses_tls
check "PostgreSQL not published" not_published postgres
check "VictoriaMetrics not published" not_published victoriametrics
check "VictoriaLogs not published" not_published victorialogs

if [ "$failed" -ne 0 ]; then
    compose logs --tail 50 server web postgres
    exit 1
fi
echo "smoke test passed"
