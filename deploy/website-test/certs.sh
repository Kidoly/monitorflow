#!/usr/bin/env bash
# Makes the website test's certificates (ADR-0150) in $1: a throwaway
# authority, and server certificates for web.test and 127.0.0.1 that are
# valid, expired, for another name, expiring in 5 days, and self-signed.
# Dates are set with `openssl ca`, which every OpenSSL 3 knows.
set -euo pipefail
out=$1
mkdir -p "$out"
cd "$out"
stamp() { date -u -d "$1" +%Y%m%d%H%M%SZ; }

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
    -subj "/CN=MonitorFlow website test CA" -keyout ca-key.pem -out ca.pem 2>/dev/null
mkdir -p ca/new
: >ca/index.txt
echo 1000 >ca/serial
cat >ca.cnf <<CNF
[ca]
default_ca = test
[test]
dir = ca
database = ca/index.txt
new_certs_dir = ca/new
serial = ca/serial
certificate = ca.pem
private_key = ca-key.pem
default_md = sha256
policy = anything
unique_subject = no
copy_extensions = none
[anything]
commonName = supplied
[site]
subjectAltName = \$ENV::SAN
extendedKeyUsage = serverAuth
basicConstraints = CA:false
CNF

# leaf NAME SAN START END: a certificate of the authority.
leaf() {
    local name=$1 san=$2 start=$3 end=$4 cn=${2#DNS:}
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=${cn%%,*}" \
        -keyout "$name-key.pem" -out "$name.csr" 2>/dev/null
    SAN=$san openssl ca -batch -config ca.cnf -extensions site -notext -in "$name.csr" -out "$name.pem" \
        -startdate "$(stamp "$start")" -enddate "$(stamp "$end")" 2>/dev/null
}
leaf valid "DNS:web.test,IP:127.0.0.1" "-1 day" "+90 days"
leaf expired "DNS:web.test,IP:127.0.0.1" "-60 days" "-2 days"
leaf wrong "DNS:other.test" "-1 day" "+90 days"
leaf soon "DNS:web.test,IP:127.0.0.1" "-80 days" "+5 days"
# Signed by itself, not by the authority.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 90 -subj "/CN=web.test" \
    -addext "subjectAltName=DNS:web.test,IP:127.0.0.1" -keyout self-key.pem -out self.pem 2>/dev/null
# Read by nginx's own user; throwaway keys.
chmod 644 ./*-key.pem
