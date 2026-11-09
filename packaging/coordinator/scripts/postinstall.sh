#!/bin/sh
# The post-install script of the hostkeeper-coordinator deb and rpm
# (packaging/coordinator/nfpm.yaml), run on every install and upgrade. It creates the
# coordinator's user with systemd-sysusers and its directories with systemd-tmpfiles (both from
# systemd, which the package depends on), and has systemd read the unit again. It neither
# enables nor starts the unit, on a first install or an upgrade: an unconfigured coordinator
# cannot start, so the operator enables it once /etc/hostkeeper-coordinator/serve.toml is
# written.
set -e
systemd-sysusers hostkeeper-coordinator.conf
systemd-tmpfiles --create hostkeeper-coordinator.conf
# Fails where systemd is not running (a container or a chroot); nothing above needs it.
systemctl daemon-reload || true
exit 0
