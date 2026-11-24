#!/usr/bin/env bash
# Real tests of the ready-made application checks (ADR-0183): starts
# PostgreSQL, MySQL, MariaDB and Redis in containers with the read-only
# accounts the guides describe, runs crates/apps/tests/real.rs against
# them and against the local Docker engine, then removes the containers.
# Needs Docker. Ports 30432, 30306, 30307 and 30379 on 127.0.0.1.
set -euo pipefail
cd "$(dirname "$0")/.."

password=mf-apps-test-pw
names=(mf-apps-pg mf-apps-mysql mf-apps-mariadb mf-apps-redis)

cleanup() {
    docker rm -f "${names[@]}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker run -d --name mf-apps-pg -e POSTGRES_PASSWORD=admin-pw \
    -p 127.0.0.1:30432:5432 postgres:17 >/dev/null
docker run -d --name mf-apps-mysql -e MYSQL_ROOT_PASSWORD=admin-pw \
    -p 127.0.0.1:30306:3306 mysql:8.4 >/dev/null
docker run -d --name mf-apps-mariadb -e MARIADB_ROOT_PASSWORD=admin-pw \
    -p 127.0.0.1:30307:3306 mariadb:11.4 >/dev/null
docker run -d --name mf-apps-redis -p 127.0.0.1:30379:6379 redis:7.4 >/dev/null

# Waits for `$@` to succeed, at most 3 minutes.
wait_for() {
    for _ in $(seq 90); do
        if "$@" >/dev/null 2>&1; then return 0; fi
        sleep 2
    done
    echo "timed out waiting for: $*" >&2
    return 1
}

wait_for docker exec mf-apps-pg pg_isready -U postgres
docker exec mf-apps-pg psql -q -U postgres \
    -c "CREATE ROLE monitorflow LOGIN PASSWORD '$password'; GRANT pg_monitor TO monitorflow;"

wait_for docker exec mf-apps-redis redis-cli ping
docker exec mf-apps-redis redis-cli ACL SETUSER monitorflow on ">$password" -@all +info +ping >/dev/null

grant="CREATE USER 'monitorflow'@'%' IDENTIFIED BY '$password'; GRANT PROCESS, REPLICATION CLIENT ON *.* TO 'monitorflow'@'%';"
wait_for docker exec mf-apps-mysql mysql -uroot -padmin-pw -e "SELECT 1"
docker exec mf-apps-mysql mysql -uroot -padmin-pw -e "$grant" 2>/dev/null
wait_for docker exec mf-apps-mariadb mariadb -uroot -padmin-pw -e "SELECT 1"
docker exec mf-apps-mariadb mariadb -uroot -padmin-pw -e "$grant"

MF_APPS_TEST_PASSWORD=$password \
    MF_APPS_TEST_POSTGRESQL=127.0.0.1:30432 \
    MF_APPS_TEST_MYSQL=127.0.0.1:30306 \
    MF_APPS_TEST_MARIADB=127.0.0.1:30307 \
    MF_APPS_TEST_REDIS=127.0.0.1:30379 \
    MF_APPS_TEST_DOCKER=1 \
    cargo test -p monitorflow-apps --test real -- --nocapture
