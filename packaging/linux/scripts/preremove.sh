#!/bin/sh
# The pre-remove script of the hostkeeper deb and rpm (packaging/linux/nfpm.yaml). It disables
# the unit when the package is removed, and does nothing when it is upgraded.
#
# The package manager passes: deb (prerm) `remove` on removal and `upgrade` (or another word)
# otherwise; rpm (%preun) the number of copies left, 0 on removal and 1 on upgrade.
set -e
case "$1" in
  remove | 0) systemctl disable hostkeeper.service || true ;;
esac
exit 0
