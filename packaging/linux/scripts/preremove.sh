#!/bin/sh
# The pre-remove script of the hostkeeper deb, rpm and Arch package (packaging/linux/nfpm.yaml).
# It disables the unit when the deb or the rpm is removed, and does nothing when it is upgraded.
#
# The package manager passes: deb (prerm) `remove` on removal and `upgrade` (or another word)
# otherwise; rpm (%preun) the number of copies left, 0 on removal and 1 on upgrade; pacman
# (nfpm inlines this file as `pre_remove`) the old version, `<pkgver>-<pkgrel>`, alone, which no
# arm below matches, so under pacman nothing runs on removal.
set -e
case "$1" in
  remove | 0) systemctl disable hostkeeper.service || true ;;
esac
exit 0
