#!/bin/sh
# The pre-remove script of the hostkeeper-coordinator deb and rpm
# (packaging/coordinator/nfpm.yaml). On removal it disables the unit and stops it, so no
# coordinator runs a program the package manager is about to delete; on an upgrade it does
# nothing, and the coordinator keeps running the old program until the operator restarts it.
# The user, /etc/hostkeeper-coordinator and /var/lib/hostkeeper-coordinator, which hold the keys
# and the state, stay.
#
# The package manager passes: deb (prerm) `remove` on removal and `upgrade` (or another word)
# otherwise; rpm (%preun) the number of copies left, 0 on removal and 1 on upgrade.
set -e
case "$1" in
  remove | 0) systemctl disable --now hostkeeper-coordinator.service || true ;;
esac
exit 0
