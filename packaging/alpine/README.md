# Alpine packaging

What an Alpine host needs before hostkeeper can deliver anything to it: the daemon and an OpenRC
init script that runs it. The files here are the sources of an apk.

| file | what |
|---|---|
| `hostkeeper.initd` | the init script, `/etc/init.d/hostkeeper`: supervise-daemon runs the daemon as root with the flags of `packaging/linux/hostkeeper.service`, `--ready-file /run/hostkeeper/ready` in place of `--notify-systemd`; `start_post` waits for that file (or for the daemon's first failed start), supervise-daemon respawns the daemon a second after it exits, and `start_pre` makes the daemon's directories and removes the ready file before each start |
| `nfpm.yaml` | the apk, for nfpm |
| `scripts/postinstall.sh`, `scripts/preremove.sh` | the apk's scripts: add the service to the default runlevel on a first install, take it out on removal |

## The package

`hostkeeper_<version>_<arch>.apk` (`x86_64`, `aarch64`) installs the static musl build of the
daemon, the init script, the template of the config and the pinned `gh` and Sigstore trusted root
its attestation check runs on:

| path | mode |
|---|---|
| `/usr/bin/hostkeeper` | 0755 |
| `/etc/init.d/hostkeeper` | 0755 |
| `/usr/share/hostkeeper/hostkeeper.toml.template` | 0644 |
| `/usr/lib/hostkeeper/gh` | 0755 |
| `/usr/lib/hostkeeper/trusted_root.json` | 0644 |

all owned by root. It needs OpenRC 0.62 or later (Alpine 3.22 or later), the oldest release the
device tests run the package on. It carries
neither the systemd unit nor `hostkeeper-pxe`, the boot server of network install, which runs on
a systemd host.

The release pipeline does not publish the apk yet. It is built from the same static musl daemon
the release stages for the deb and the rpm (A15 of `docs/design/packages.md`,
`target/nfpm/hostkeeper`); publishing it (its asset name, attestation and verify document) is a
change of its own. It is hostkeeper's own package, as the deb and the rpm are;
there is no `apk` installer of service packages (`docs/design/packages-repositories.md`), so an
Alpine host's services take the tarball. Until then the apk is built by hand, as the header of
`nfpm.yaml` says.

## Installing

An apk built here is not signed with a key the host trusts, so install it with

```
apk add --allow-untrusted hostkeeper_<version>_<arch>.apk
```

On a first install the post-install script adds the service to the default runlevel; it does not
start it. An upgrade leaves the runlevels as they were, and removing the package takes the
service out of the default runlevel without stopping a running daemon.

The package does not place `/etc/hostkeeper/hostkeeper.toml`, so an upgrade never replaces a
host's config. Whoever installs the package places it, from
`/usr/share/hostkeeper/hostkeeper.toml.template`, as `packaging/linux/README.md` describes, and
then starts the service with `rc-service hostkeeper start` (or reboots).

`cargo test -p xtask --test packaging` reads every file here and checks it: the package's keys,
files, modes and dependency, that the init script runs the flags of the systemd unit and makes
the directories of the tmpfiles rules, and that the scripts never start the service.
