#!/bin/sh
# The post-install script of the hostkeeper apk (packaging/alpine/nfpm.yaml). It adds the init
# script to the default runlevel; it never starts the service, so a host whose config is not
# placed yet runs no daemon until the next boot.
#
# apk runs it with the new version as $1 on a first install only. On an upgrade apk runs
# .post-upgrade instead, which this package does not have, so an upgrade leaves the service in
# the runlevels as the operator left it. The daemon's directories are made by the init script
# at each start.
set -e
rc-update add hostkeeper default
exit 0
