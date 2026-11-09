#!/usr/bin/env bash
# What CI runs. Run before declaring a task done.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/check-numbers.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
# The server from AGPL code only, without the Enterprise edition (ADR-0100).
cargo clippy -p monitorflow-server --no-default-features --all-targets -- -D warnings
# sqlx names test databases after the test alone, so two checkouts
# testing against one PostgreSQL drop each other's databases: take turns.
if command -v flock >/dev/null; then
    flock "${TMPDIR:-/tmp}/monitorflow-db-tests.lock" cargo test --workspace
else
    cargo test --workspace
fi
