#!/usr/bin/env bash
# Completes a directory of release files (ADR-0052): adds the installer, the
# systemd unit and the sudo rules next to the agent binaries already in it,
# then writes SHA256SUMS over everything. The release workflow publishes
# the result; deploy/install-test.sh serves it to the installer.
#   deploy/release-files.sh DIR
set -euo pipefail
dir=${1:?usage: deploy/release-files.sh DIR}
deploy=$(cd "$(dirname "$0")" && pwd)

install -m 0755 "$deploy/install.sh" "$dir/install.sh"
install -m 0644 "$deploy/systemd/monitorflow-agent.service" "$dir/monitorflow-agent.service"
install -m 0644 "$deploy/sudoers/monitorflow-agent" "$dir/monitorflow-agent-sudoers"
rm -f "$dir/SHA256SUMS"
sums=$(cd "$dir" && find . -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort | xargs sha256sum)
printf '%s\n' "$sums" >"$dir/SHA256SUMS"
cat "$dir/SHA256SUMS"
