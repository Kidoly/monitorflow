#!/usr/bin/env bash
# What CI runs. Run before declaring a task done.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/check-numbers.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
