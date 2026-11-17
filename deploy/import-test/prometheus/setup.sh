#!/usr/bin/env bash
# Starts the throwaway Prometheus setup of compose.yml, checks and reloads
# its configuration, waits for its targets and creates silences through
# Alertmanager's API, for MonitorFlow's import tests (ADR-0146). Local
# Docker only.
#
#   deploy/import-test/prometheus/setup.sh
#
# The configuration files are in config/: the import tests upload the
# same files. Running it again reloads them and creates only the silences
# that are missing. At the end it prints the addresses for a live import.
#
# Everything here is example data: example.com addresses, no real
# credentials.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROM="http://127.0.0.1:19090"
AM="http://127.0.0.1:19093"
COMPOSE=(docker compose -p mf-import-prom -f "$HERE/compose.yml")
PROM_IMAGE="prom/prometheus:v3.5.0"
AM_IMAGE="prom/alertmanager:v0.28.1"

for tool in curl jq docker; do
  command -v "$tool" >/dev/null || { echo "setup.sh needs $tool" >&2; exit 1; }
done

say() { printf '== %s\n' "$*" >&2; }

say "Checking the configuration files"
docker run --rm --entrypoint promtool \
  -v "$HERE/config/prometheus.yml:/etc/prometheus/prometheus.yml:ro" \
  -v "$HERE/config/rules:/etc/prometheus/rules:ro" \
  "$PROM_IMAGE" check config --syntax-only /etc/prometheus/prometheus.yml >&2
docker run --rm --entrypoint amtool \
  -v "$HERE/config/alertmanager.yml:/etc/alertmanager/alertmanager.yml:ro" \
  "$AM_IMAGE" check-config /etc/alertmanager/alertmanager.yml >&2

say "Starting Prometheus, Alertmanager and the exporters"
"${COMPOSE[@]}" up -d >&2

# wait_for URL: until it answers 200, at most two minutes.
wait_for() {
  local url="$1" i
  for i in $(seq 1 120); do
    if curl -fsS -o /dev/null "$url"; then return 0; fi
    sleep 1
  done
  echo "$url did not answer" >&2
  exit 1
}
wait_for "$PROM/-/ready"
wait_for "$AM/-/ready"

# Files changed since the containers started are read again.
say "Reloading the configuration"
curl -fsS -X POST "$PROM/-/reload" >&2
curl -fsS -X POST "$AM/-/reload" >&2

say "Waiting for the targets"
expected="node blackbox-exporter blackbox-http blackbox-tcp"
for i in $(seq 1 90); do
  down="$(curl -fsS "$PROM/api/v1/targets?state=active" | jq -r --arg jobs "$expected" '
    ($jobs | split(" ")) as $want
    | [.data.activeTargets[] | select(.labels.job as $j | $want | index($j)) | select(.health != "up")]
    | length')"
  total="$(curl -fsS "$PROM/api/v1/targets?state=active" | jq '.data.activeTargets | length')"
  if [[ "$down" == 0 && "$total" -ge 9 ]]; then break; fi
  if [[ $i == 90 ]]; then
    echo "Targets are not all up after 90 seconds" >&2
    exit 1
  fi
  sleep 1
done
curl -fsS "$PROM/api/v1/targets?state=active" \
  | jq -r '.data.activeTargets[] | "   \(.labels.job)\t\(.labels.instance)\t\(.health)"' >&2

# silence COMMENT HOURS MATCHERS_JSON: creates it unless an active or
# pending silence has that comment already.
silence() {
  local comment="$1" hours="$2" matchers="$3" existing body
  existing="$(curl -fsS "$AM/api/v2/silences" | jq --arg c "$comment" \
    '[.[] | select(.comment == $c and (.status.state == "active" or .status.state == "pending"))] | length')"
  if [[ "$existing" != 0 ]]; then
    say "Silence \"$comment\" is there already"
    return
  fi
  body="$(jq -nc --arg c "$comment" --argjson h "$hours" --argjson m "$matchers" '{
    matchers: $m,
    startsAt: (now | todate),
    endsAt: (now + $h * 3600 | todate),
    createdBy: "setup.sh",
    comment: $c}')"
  curl -fsS -X POST -H 'Content-Type: application/json' -d "$body" "$AM/api/v2/silences" >/dev/null
  say "Created silence \"$comment\""
}

silence "Database upgrade" 48 \
  '[{"name": "instance", "value": "db-01.*", "isRegex": true, "isEqual": true}]'
silence "Nightly batch on the web server" 48 \
  '[{"name": "instance", "value": "web-01:9100", "isRegex": false, "isEqual": true},
    {"name": "alertname", "value": "HostHighCpuLoad", "isRegex": false, "isEqual": true}]'
silence "Old staging machines" 48 \
  '[{"name": "instance", "value": "staging-.*", "isRegex": true, "isEqual": true}]'

say "Ready"
cat <<EOF
Prometheus:   $PROM
Alertmanager: $AM
Files:        $HERE/config
Live test:    MF_PROMETHEUS_URL=$PROM MF_ALERTMANAGER_URL=$AM \\
              cargo test -p monitorflow-import --test prometheus -- --ignored
EOF
