#!/bin/sh
# Creates, once, the TLS material of the server's link to PostgreSQL
# (ADR-0051) in /certs: a CA of its own and a certificate for the host name
# "postgres". The CA key is deleted at once, so nothing else can be issued.
# Runs as root to hand the key to PostgreSQL's user.
set -eu
cd /certs
if [ -f server.pem ]; then
    echo "db-certs: already present"
    exit 0
fi
umask 077
openssl req -x509 -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 -days 3650 \
    -keyout ca-key.pem -out ca.pem -subj "/CN=MonitorFlow database CA"
openssl req -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
    -keyout server-key.pem -out server.csr -subj "/CN=postgres"
printf 'subjectAltName=DNS:postgres\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n' > ext.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -days 3650 \
    -extfile ext.cnf -out server.pem
rm -f ca-key.pem ca.srl server.csr ext.cnf
# 999 is the postgres user of the official image; it requires a private key.
chown 999:999 server-key.pem
chmod 0644 ca.pem server.pem
echo "db-certs: created"
