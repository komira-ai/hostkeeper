# The Alpine host of the OpenRC device tests (crates/hostkeeper-privileged/tests/openrc_device.rs):
# Alpine 3.22, pinned by digest, with OpenRC as its init. privileged.yml's job openrc-device builds
# it, before the tests run, as
#
#   docker build --tag hostkeeper-openrc-device crates/hostkeeper-privileged/openrc
#
# and the tests boot one container of it per test, with nothing fetched while they run. OpenRC
# is told it runs in a container (rc_sys="docker": no kernel or device setup), and the gettys of
# busybox's inittab are dropped (a container has no terminals). /sbin/init (busybox's) is the
# container's first process: it runs OpenRC's sysinit, boot and default runlevels, and reaps
# what is left to it.
FROM alpine:3.22@sha256:bfe7ceb9f09ec0566c48fb93356e45a47a00be5a1dd14fa5b909e7709c66b034
RUN apk add --no-cache openrc && sed -i -e 's/^#rc_sys=""/rc_sys="docker"/' /etc/rc.conf && sed -i -e '/getty/d' /etc/inittab
CMD ["/sbin/init"]
