#!/usr/bin/env bash
# Fails when the em dash (U+2014) appears in a tracked file. The project
# never uses it (CLAUDE.md, Typography). The script names it by its UTF-8
# bytes so that it does not trip over itself.
#
# web/ and docs/guides/ are cleaned in their own PRs; drop them from the
# exclusions once those are merged. No merged SQL migration contains the
# character, and this check runs before a migration can merge, so none
# needs an exclusion.
set -euo pipefail
cd "$(dirname "$0")/.."

dash=$(printf '\xe2\x80\x94')
if git grep -n -F -e "$dash" -- . ':!web/' ':!docs/guides/'; then
    echo "error: the lines above contain an em dash (U+2014); rewrite them" \
        "with a comma, colon, parentheses or a period" >&2
    exit 1
fi
