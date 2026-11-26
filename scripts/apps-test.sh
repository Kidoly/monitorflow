#!/usr/bin/env bash
# Real tests of the ready-made application checks (ADR-0183): starts
# PostgreSQL, MySQL, MariaDB and Redis in containers with the read-only
# accounts the guides describe, and Tomcat and a Kafka broker with the
# Jolokia JVM agent behind a password, runs crates/apps/tests/real.rs
# against them and against the local Docker engine, then removes the
# containers. Needs Docker and, once, Maven Central for the Jolokia agent
# (its SHA-256 checked). Ports 30432, 30306, 30307, 30379, 30778 and 30779
# on 127.0.0.1.
set -euo pipefail
cd "$(dirname "$0")/.."

password=mf-apps-test-pw
names=(mf-apps-pg mf-apps-mysql mf-apps-mariadb mf-apps-redis mf-apps-tomcat mf-apps-kafka)
jolokia_version=2.6.3
jolokia_sha256=b51f95db08cc44c112a3724e9c782ea7f9cf60de5fb70cced119832bda17dadf
work=$(mktemp -d)

cleanup() {
    docker rm -f "${names[@]}" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
docker rm -f "${names[@]}" >/dev/null 2>&1 || true

jar="$work/jolokia.jar"
curl -fsSL -o "$jar" \
    "https://repo1.maven.org/maven2/org/jolokia/jolokia-agent-jvm/$jolokia_version/jolokia-agent-jvm-$jolokia_version-javaagent.jar"
echo "$jolokia_sha256  $jar" | sha256sum -c --quiet
chmod 0644 "$jar"
chmod 0755 "$work"
agent="-javaagent:/opt/jolokia.jar=host=0.0.0.0,port=8778,user=monitorflow,password=$password"

docker run -d --name mf-apps-pg -e POSTGRES_PASSWORD=admin-pw \
    -p 127.0.0.1:30432:5432 postgres:17 >/dev/null
docker run -d --name mf-apps-mysql -e MYSQL_ROOT_PASSWORD=admin-pw \
    -p 127.0.0.1:30306:3306 mysql:8.4 >/dev/null
docker run -d --name mf-apps-mariadb -e MARIADB_ROOT_PASSWORD=admin-pw \
    -p 127.0.0.1:30307:3306 mariadb:11.4 >/dev/null
docker run -d --name mf-apps-redis -p 127.0.0.1:30379:6379 redis:7.4 >/dev/null
docker run -d --name mf-apps-tomcat -v "$jar:/opt/jolokia.jar:ro" \
    -e CATALINA_OPTS="$agent" -p 127.0.0.1:30778:8778 tomcat:10.1-jre21 >/dev/null
docker run -d --name mf-apps-kafka -v "$jar:/opt/jolokia.jar:ro" \
    -e KAFKA_OPTS="$agent" -p 127.0.0.1:30779:8778 apache/kafka:3.9.1 >/dev/null

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

# Jolokia answers once each JVM started, the broker's MBeans once it
# runs. The topic tool runs without the agent, whose port is taken.
jolokia_ready() {
    curl -fs -u "monitorflow:$password" "http://127.0.0.1:$1/jolokia/read/$2"
}
wait_for jolokia_ready 30778 "Catalina:type=Server/stateName"
wait_for jolokia_ready 30779 "kafka.server:type=ReplicaManager,name=PartitionCount"
docker exec -e KAFKA_OPTS= mf-apps-kafka /opt/kafka/bin/kafka-topics.sh --bootstrap-server localhost:9092 \
    --create --topic mf-test --partitions 2 >/dev/null

MF_APPS_TEST_PASSWORD=$password \
    MF_APPS_TEST_POSTGRESQL=127.0.0.1:30432 \
    MF_APPS_TEST_MYSQL=127.0.0.1:30306 \
    MF_APPS_TEST_MARIADB=127.0.0.1:30307 \
    MF_APPS_TEST_REDIS=127.0.0.1:30379 \
    MF_APPS_TEST_TOMCAT=127.0.0.1:30778 \
    MF_APPS_TEST_KAFKA=127.0.0.1:30779 \
    MF_APPS_TEST_DOCKER=1 \
    cargo test -p monitorflow-apps --test real -- --nocapture
