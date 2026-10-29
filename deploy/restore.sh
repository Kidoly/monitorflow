#!/bin/sh
# Restores a MonitorFlow backup (ADR-0058), the file "Download" gives on
# the Backups page, into the compose deployment next to this script:
#   deploy/restore.sh monitorflow-backup-20261029T030000Z-daily.tar.gz
# Replaces the database and the server's data (its CA and keys), then
# starts everything. Works on a fresh install too: agents reconnect as
# before. The updater does the work, in a one-off container, and refuses
# a backup newer than the server image. COMPOSE_FILE and
# COMPOSE_PROJECT_NAME, when set, choose another deployment.
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 BACKUP_FILE" >&2
    exit 2
fi
if [ ! -f "$1" ]; then
    echo "$1: no such file" >&2
    exit 1
fi
file=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
here=$(cd "$(dirname "$0")" && pwd)

if [ -n "${COMPOSE_FILE:-}" ]; then
    set --
else
    set -- -f "$here/docker-compose.yml"
fi
exec docker compose "$@" run --rm --no-deps -T \
    -v "$file:/restore/backup.tar.gz:ro" updater restore /restore/backup.tar.gz
