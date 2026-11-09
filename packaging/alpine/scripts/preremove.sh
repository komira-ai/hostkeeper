#!/bin/sh
# The pre-remove script of the hostkeeper apk (packaging/alpine/nfpm.yaml). It takes the init
# script out of the default runlevel when the package is removed; apk runs it as
# .pre-deinstall on removal only, and not on an upgrade. It does not stop a running daemon, and
# a runlevel the service was never in is not an error.
set -e
rc-update del hostkeeper default || true
exit 0
