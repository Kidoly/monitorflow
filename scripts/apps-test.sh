#!/usr/bin/env bash
# Real tests of the ready-made application checks (ADR-0183): starts
# PostgreSQL, MySQL, MariaDB, SQL Server and Redis in containers with the
# read-only accounts the guides describe, and Tomcat and a Kafka broker
# with the Jolokia JVM agent behind a password, runs
# crates/apps/tests/real.rs against them and against the local Docker
# engine, then removes the containers. A second SQL Server, 2025, has a
# certificate from a CA made here, for TDS 8.0's strict encryption (2022
# on Linux refuses strict sessions); the test trusts that CA through
# SSL_CERT_FILE. The named instance test plays SQL Server Browser on UDP
# 1434 of 127.0.0.1. Needs Docker, openssl, 4 GB of memory for the two
# SQL Servers and, once, Maven Central for the Jolokia agent (its SHA-256
# checked). Ports 30432, 30306, 30307, 31433, 31434, 30379, 30778 and
# 30779 on 127.0.0.1.
set -euo pipefail
cd "$(dirname "$0")/.."

password=mf-apps-test-pw
names=(mf-apps-pg mf-apps-mysql mf-apps-mariadb mf-apps-mssql mf-apps-mssql-strict mf-apps-redis mf-apps-tomcat mf-apps-kafka)
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
# Always On turned on, for an availability group of one replica.
docker run -d --name mf-apps-mssql -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD='Admin-pw-1433!' \
    -e MSSQL_ENABLE_HADR=1 -p 127.0.0.1:31433:1433 \
    mcr.microsoft.com/mssql/server:2022-latest >/dev/null
# Strict encryption needs a certificate SQL Server does not make itself:
# one for localhost, from a CA of this run.
certs="$work/certs"
mkdir "$certs"
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=MonitorFlow test CA" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign" \
    -keyout "$certs/ca.key" -out "$certs/ca.pem" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$certs/server.key" -out "$certs/server.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature,keyEncipherment\n' >"$certs/ext"
openssl x509 -req -in "$certs/server.csr" -CA "$certs/ca.pem" -CAkey "$certs/ca.key" -CAcreateserial \
    -days 2 -extfile "$certs/ext" -out "$certs/server.pem" 2>/dev/null
printf '[network]\ntlscert = /certs/server.pem\ntlskey = /certs/server.key\nforceencryption = 1\n' >"$certs/mssql.conf"
chmod 0755 "$certs"
chmod 0644 "$certs"/*
docker run -d --name mf-apps-mssql-strict -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD='Admin-pw-1433!' \
    -v "$certs/mssql.conf:/var/opt/mssql/mssql.conf:ro" -v "$certs:/certs:ro" \
    -p 127.0.0.1:31434:1433 mcr.microsoft.com/mssql/server:2025-latest >/dev/null
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

sqlcmd() {
    docker exec mf-apps-mssql /opt/mssql-tools18/bin/sqlcmd -C -b -S localhost \
        -U sa -P 'Admin-pw-1433!' -Q "$1"
}
wait_for sqlcmd "SELECT 1"
strict_sqlcmd() {
    docker exec mf-apps-mssql-strict /opt/mssql-tools18/bin/sqlcmd -C -b -S localhost \
        -U sa -P 'Admin-pw-1433!' -Q "$1"
}
wait_for strict_sqlcmd "SELECT 1"
strict_sqlcmd "CREATE LOGIN monitorflow WITH PASSWORD = '$password', CHECK_POLICY = OFF;
GRANT VIEW SERVER STATE TO monitorflow;" >/dev/null
# The login the guide describes, one without its right, and an
# availability group of one replica, without a cluster, over a database.
sqlcmd "CREATE LOGIN monitorflow WITH PASSWORD = '$password', CHECK_POLICY = OFF;
GRANT VIEW SERVER STATE TO monitorflow;
CREATE LOGIN mf_bare WITH PASSWORD = '$password', CHECK_POLICY = OFF;" >/dev/null
sqlcmd "CREATE MASTER KEY ENCRYPTION BY PASSWORD = 'Master-key-pw-1433!';
CREATE CERTIFICATE mf_ag_cert WITH SUBJECT = 'mf_ag';
CREATE ENDPOINT mf_hadr STATE = STARTED AS TCP (LISTENER_PORT = 5022)
    FOR DATABASE_MIRRORING (ROLE = ALL, AUTHENTICATION = CERTIFICATE mf_ag_cert);
CREATE DATABASE shop;
ALTER DATABASE shop SET RECOVERY FULL;
BACKUP DATABASE shop TO DISK = '/var/opt/mssql/data/shop.bak';
DECLARE @sql NVARCHAR(MAX) = N'CREATE AVAILABILITY GROUP mf_ag WITH (CLUSTER_TYPE = NONE)
    FOR DATABASE shop REPLICA ON N''' + @@SERVERNAME + N''' WITH (
    ENDPOINT_URL = N''tcp://localhost:5022'', AVAILABILITY_MODE = ASYNCHRONOUS_COMMIT,
    FAILOVER_MODE = MANUAL, SEEDING_MODE = MANUAL)';
EXEC (@sql);" >/dev/null

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
    MF_APPS_TEST_MSSQL=127.0.0.1:31433 \
    MF_APPS_TEST_MSSQL_STRICT=localhost:31434 \
    SSL_CERT_FILE="$certs/ca.pem" \
    MF_APPS_TEST_REDIS=127.0.0.1:30379 \
    MF_APPS_TEST_TOMCAT=127.0.0.1:30778 \
    MF_APPS_TEST_KAFKA=127.0.0.1:30779 \
    MF_APPS_TEST_DOCKER=1 \
    cargo test -p monitorflow-apps --test real -- --nocapture
