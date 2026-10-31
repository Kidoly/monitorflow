#!/bin/sh
# Entry point of the mirror tools image (ADR-0067): adds the archive keys
# of this image to the keyring aptly checks upstreams with, keeping any
# other key an administrator put there, then serves the server's requests.
set -eu
dir=/var/lib/monitorflow/server/mirrors
gpg --batch --quiet --no-default-keyring --keyring "$dir/upstream-keyring.gpg" \
    --import /usr/share/monitorflow/upstream-keys.gpg
exec monitorflow-mirror-tools "$dir"
