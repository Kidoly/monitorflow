#!/bin/sh
# Debian packages reposync as a dnf plugin without its command. dnf needs a
# release to start on a system that is not a dnf one; repositories come
# from the arguments only. The cache is in the mirrors volume, which backups
# leave out, not in the container.
exec dnf --releasever=1 --setopt=reposdir=/nonexistent \
    --setopt=cachedir=/var/lib/monitorflow/server/mirrors/dnf-work/.cache \
    --disableplugin=system_upgrade reposync "$@"
