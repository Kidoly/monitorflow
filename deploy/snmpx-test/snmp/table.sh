#!/bin/sh
# A table for snmpd's `pass` (ADR-0157 test): one row per line of
# /work/rows, as `index|name|state|count`, under
# .1.3.6.1.4.1.8072.9999.9999.1.1: column 2 the name, 3 a state (an
# integer), 4 a Counter64. `-g OID` answers that object, `-n OID` the one
# after it, as snmpd asks.
BASE=.1.3.6.1.4.1.8072.9999.9999.1.1
ROWS=/work/rows
[ -r "$ROWS" ] || exit 0
list() {
    for col in 2 3 4; do
        sort -t'|' -k1,1n "$ROWS" | while IFS='|' read -r index name state count; do
            [ -n "$index" ] || continue
            case $col in
                2) echo "$BASE.$col.$index string $name" ;;
                3) echo "$BASE.$col.$index integer $state" ;;
                4) echo "$BASE.$col.$index counter64 $count" ;;
            esac
        done
    done
}
# Whether OID $1 comes after OID $2, arc by arc.
after() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        n = split(substr(a, 2), x, "."); m = split(substr(b, 2), y, ".")
        for (i = 1; i <= n && i <= m; i++) {
            if (x[i] + 0 > y[i] + 0) exit 0
            if (x[i] + 0 < y[i] + 0) exit 1
        }
        exit (n > m) ? 0 : 1
    }'
}
answer() { echo "$1"; echo "$2"; shift 2; echo "$*"; }
case $1 in
    -g)
        list | while read -r oid type value; do
            if [ "$oid" = "$2" ]; then answer "$oid" "$type" "$value"; break; fi
        done
        ;;
    -n)
        list | while read -r oid type value; do
            if after "$oid" "$2"; then answer "$oid" "$type" "$value"; break; fi
        done
        ;;
esac
