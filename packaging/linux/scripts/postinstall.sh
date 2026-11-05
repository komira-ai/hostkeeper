#!/bin/sh
# The post-install script of the hostkeeper deb and rpm (packaging/linux/nfpm.yaml). It makes
# the daemon's directories and enables the unit on a first install; it never starts the unit,
# so a host whose config is not placed yet runs no daemon until the next boot.
#
# The package manager passes: deb (postinst) `configure` and the version it replaces, which is
# empty on a first install; rpm (%post) the number of installed copies, 1 on a first install.
# An upgrade leaves the unit enabled or disabled as the operator left it.
set -e
systemd-tmpfiles --create hostkeeper.conf
# Fails where systemd is not running (a boot image built in a chroot); the enable does not
# need it.
systemctl daemon-reload || true
case "$1:$2" in
  configure: | 1:) systemctl enable hostkeeper.service ;;
esac
exit 0
