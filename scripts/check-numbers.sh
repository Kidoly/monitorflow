#!/usr/bin/env bash
# Fails when two ADRs or two migrations share a number. Parallel branches
# pick the same next number without a merge conflict, so nothing else
# catches it for ADRs.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0

# The files directly in $1 whose names start with the same text before $2.
check_dir() {
    local dir=$1 separator=$2 number
    for number in $(find "$dir" -maxdepth 1 -type f -name '[0-9]*' -printf '%f\n' |
        cut -d "$separator" -f 1 | sort | uniq -d); do
        echo "duplicate number $number:" "$dir/$number$separator"*
        status=1
    done
}

check_dir docs/adr -
check_dir crates/server/migrations _
exit "$status"
