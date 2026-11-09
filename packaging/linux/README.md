# Linux packaging

What a Linux host needs before hostkeeper can deliver anything to it: the daemon, its unit
and its config (`docs/design/hostkeeper-design.md`, sections 4.1 and 4.6). The files here are
the sources of a deb, an rpm, an Arch package and a boot-image overlay. The three packages also carry
`/usr/bin/hostkeeper-pxe`, the boot server of network install
(`docs/design/network-install.md`, section 3), with no unit: it runs only when an operator
starts it, in the foreground. Building the binaries, building a boot image, signing and
publishing the packages are not here.

| file | what |
|---|---|
| `hostkeeper.service` | the bootstrap unit, the one the deb, the rpm, the Arch package and a boot image install at `/usr/lib/systemd/system/hostkeeper.service`: the daemon as root, `Type=notify`, started as `/usr/bin/hostkeeper daemon` with its paths as flags and no `ExecStartPre=`, with `Restart=always` and no start limit of its own, and restarted by a 60-second watchdog its heartbeat loop feeds (`WatchdogSec=`, four heartbeat periods); it conflicts with kbf's update agent and its root half and is ordered after them, so a handoff stops one before it starts the other |
| `units/hostkeeper.service` | the package unit, the one hostkeeper's own package carries as `units/hostkeeper.service` and its install links in `/etc/systemd/system/`: the same keys, but it starts `current` of the binary directory of its own update, after an `ExecStartPre=-` that seeds that directory |
| `hostkeeper.tmpfiles.conf` | the daemon's directories and their modes, as `/usr/lib/tmpfiles.d/hostkeeper.conf` |
| `hostkeeper.toml.template` | the daemon config with a `@@NAME@@` placeholder in each value that belongs to a site |
| `nfpm.yaml` | the deb, the rpm and the Arch package, for nfpm |
| `scripts/postinstall.sh`, `scripts/preremove.sh` | the packages' scripts: make the directories, enable the unit (deb and rpm only), disable it on removal (deb and rpm only) |
| `overlay/` | the same files laid out as an image root, for a boot-image build to copy |

`cargo test -p xtask --test packaging` reads every file here and checks it: the keys and
command lines of each of the two units; that the bootstrap unit starts the installed binary,
`/usr/bin/hostkeeper`, one of the paths the daemon seeds and execs from, with no `ExecStartPre=`;
that the package unit starts `current` of the daemon's binary directory after the `-` seed line;
that neither passes `--admin-socket` or sets `RuntimeDirectory=`; that each restarts the
daemon's exit after its own update and sets no start limit; that the
package unit is the file hostkeeper's package is built with; that the directories are the
design's with the design's modes, that the template is refused until each placeholder is filled
and loads once they are, that the packages carry exactly the seven files (the daemon and `hostkeeper-pxe` among them) and the console's issue
link and no key, certificate, token or config, that the two programs, `gh` and the trusted root are installed at their paths from
where `cargo xtask release nfpm-stage` writes them, that the scripts never start the unit, and
that the overlay equals these sources.

## The binary the units start

There are two unit files, and they differ in the program they start.

The bootstrap unit (`hostkeeper.service`) starts `/usr/bin/hostkeeper daemon`, the binary the
deb or the rpm installed, with the daemon's paths as flags. That binary, started from a packaged
path (`hostkeeper_daemon::packaged`), gives the binary directory
of hostkeeper's own update (design section 6.6) its first binary when `current` starts nothing
(under the state directory's lock), and then execs `current` with its own arguments, keeping its process id. In the binary directory each
version is one executable in `versions/<version>`, and `current` and `previous` are links to
them. So the first start of a host runs the binary the package installed, and every later start
runs the binary the last update chose. A binary started from `versions/` never seeds or execs.

The package unit (`units/hostkeeper.service`) is the one hostkeeper's own package carries. A
self-update links it in `/etc/systemd/system/`, where it takes the place of the bootstrap unit.
It starts `/var/lib/updater/state/hostkeeper/bin/current` itself, never `/usr/bin/hostkeeper`,
which only a deb or rpm upgrade replaces and so is, on most hosts, older than `current`. Before
each start, `ExecStartPre=-` runs `/usr/bin/hostkeeper self-update-seed`, which, when `current`
starts no executable file, copies `/usr/bin/hostkeeper` in as its own version and points
`current` at it; otherwise it changes nothing. The `-` lets a start go on when the installed
binary is older and lacks the verb, since `current` is then already there.

Neither unit passes `--admin-socket` or sets `RuntimeDirectory=`: the daemon binds no socket
and makes `/run/hostkeeper` mode 0700 itself, where an older daemon binds the admin socket at
its default, `/run/hostkeeper/admin.sock`; `hostkeeper.tmpfiles.conf` makes it at boot.

- A self-update stages the new binary in `versions/`, points the link `next` at it and exits 0
  (the hand-over, design section 4.3); `current` is not moved. `Restart=always` starts the binary
  `current` names, the old one, which execs `next` while its start count lasts, raising the count
  at each exec. The new binary's start finishes the switch (`previous` to the old binary,
  `current` to itself) and confirms the update. A new binary that does not start, or exits before
  it confirms, leaves the count raised; at the third such start the old binary settles the update
  failed, marks the version bad and runs as the daemon, with `current` where it was. There is no
  fallback unit and no start limit: the unit retries a daemon that cannot start every second
  (`RestartSec=1`), however many times. The releases before the hand-over wrote
  `/etc/systemd/system/hostkeeper-fallback.service` and the drop-in
  `/etc/systemd/system/hostkeeper.service.d/self-update.conf`; the daemon removes both at its
  start, drop-in first, and runs `systemctl daemon-reload`, and answers a self-update `busy` until
  they are gone.
- Once `current` exists, upgrading the deb or the rpm replaces `/usr/bin/hostkeeper` and does not
  change the binary the daemon runs: hostkeeper's own update does that. A self-update also links
  the unit its package carries in `/etc/systemd/system/`, which then takes the place of the one
  the package installed; the two differ in the program they start (above).
- The binary directory is under `/var/lib`, so the unit executes a file there: a host whose
  `/var/lib` is mounted `noexec`, or whose SELinux policy does not let systemd execute files of
  that directory, cannot start the daemon this way.
- A host whose unit predates these (`ExecStart=/usr/bin/hostkeeper`, `Restart=on-failure`)
  gets one from the deb, the rpm or a boot image built from these files, not from a self-update:
  under that unit the daemon's exit after a self-update is final.

## The packages

`nfpm.yaml` says how to build them: one deb, one rpm and one Arch package for each Linux platform
of hostkeeper's packages, `linux-x86_64` (`amd64` for the deb, `x86_64` for the rpm and the Arch
package) and `linux-arm64` (`arm64`, `aarch64`). The two programs in them, `hostkeeper` and
`hostkeeper-pxe`, are the static (musl) build, so a package runs on every release of its family
and needs no library of the host. `hostkeeper_<version>_<arch>.deb`,
`hostkeeper-<version>-1.<arch>.rpm` and `hostkeeper-<version>-1-<arch>.pkg.tar.zst` install:

| path | mode |
|---|---|
| `/usr/bin/hostkeeper` | 0755 |
| `/usr/bin/hostkeeper-pxe` | 0755 |
| `/usr/lib/systemd/system/hostkeeper.service` (the bootstrap unit) | 0644 |
| `/usr/lib/tmpfiles.d/hostkeeper.conf` | 0644 |
| `/usr/share/hostkeeper/hostkeeper.toml.template` | 0644 |
| `/usr/lib/hostkeeper/gh` | 0755 |
| `/usr/lib/hostkeeper/trusted_root.json` | 0644 |

all owned by root. The last two are what the daemon checks every package's attestation with: a
pinned `gh` (the distribution's own may have no `gh attestation`) and the Sigstore trusted root
captured when the package was built. Before nfpm runs, `cargo xtask release nfpm-stage` checks
that each program of `--binaries` is a static ELF of the architecture, checks the `gh` archive
against its pin, and writes the two programs, `gh` and the trusted root into `target/nfpm/`;
`cargo xtask release check-linux-pkg <package> --staged target/nfpm` then reads each built
package back and refuses a program that has an interpreter or a NEEDED entry (a dynamic
program) and any declared dependency, besides the files and the link below. Where they come from, how
the pin is bumped and what a stale trusted root does are in
`docs/deploy/verify-a-release.md`, "What the Linux packages check packages with".

The packages also install one link, `/etc/issue.d/hostkeeper.issue`, to
`/run/hostkeeper/enrollment.issue`: the console's issue file, which the daemon writes after each
step of its enrollment (the request's state, the coordinator and the key hash), so the login
prompt of the machine's console shows the key hash to check before approving
([docs/operations/enrollment.md](../../docs/operations/enrollment.md#check-the-key-hash)). The
link points at nothing until the daemon first writes the file.

On a first install of the deb or the rpm the post-install script makes the directories of
`hostkeeper.conf` and enables the unit. It does not start it: the daemon first runs at the next
boot, or when the operator runs `systemctl start hostkeeper.service`, from the copy of
`/usr/bin/hostkeeper` its seed puts in the binary directory (above). The package unit
(`units/hostkeeper.service`) is not installed by any of the three packages; hostkeeper's own package
brings it. An upgrade leaves the unit
enabled or disabled as it was. Removing the package disables the unit; it does not stop a
running daemon, and it removes none of the daemon's directories.

The Arch package is installed with `pacman -U hostkeeper-<version>-1-<arch>.pkg.tar.zst`. pacman
passes the scripts only the package's version, so they make the directories and reload systemd
and do not enable the unit, and removing the package runs nothing: the operator runs
`systemctl enable hostkeeper.service` (and `systemctl disable hostkeeper.service` before removing
it). Arch's `systemd` package runs the tmpfiles rules and the reload on install and upgrade as
well.

The packages do not place `/etc/hostkeeper/hostkeeper.toml`, so an upgrade never replaces a
host's config. Whoever installs the package places it:

1. Copy `/usr/share/hostkeeper/hostkeeper.toml.template` to `/etc/hostkeeper/hostkeeper.toml`.
2. Replace each quoted value holding an `@@NAME@@` placeholder, quotes included, with the
   value its comment describes. `hosted_runner` takes `true` or `false` without quotes, and
   `refs` one or more quoted globs.
3. Make it root's, mode 0600: `chown root:root` and `chmod 0600`.

The file holds no secret. The config loader (`crates/hostkeeper-config`) refuses it while a
placeholder is left, while it is owned by another account or writable by the group or others,
or when it holds a key it does not know, and a daemon whose config does not load does not start
(design section 4.1); systemd starts it again for good (see above), so it starts once the
config is fixed, with nothing to reset. What the daemon does on a host that
has a config but no certificate yet, its enrollment, is design section 5.7. The whole procedure
for one node, from its inventory record through installing, filling the config and starting the
unit by hand to its approval and first heartbeat, is
[docs/operations/enrollment.md](../../docs/operations/enrollment.md#bootstrap-a-linux-node).

## The boot-image overlay

`overlay/` is the root of an image, holding only what a boot image adds for hostkeeper:

| path | what |
|---|---|
| `usr/lib/systemd/system/hostkeeper.service` | the bootstrap unit |
| `etc/systemd/system/multi-user.target.wants/hostkeeper.service` | a link to the bootstrap unit (relative, so it resolves in this tree too), which enables it |
| `usr/lib/tmpfiles.d/hostkeeper.conf` | the directories |
| `etc/hostkeeper/hostkeeper.toml.template` | the config template |
| `etc/issue.d/hostkeeper.issue` | a link to `/run/hostkeeper/enrollment.issue`, the console's issue file, as the packages install it |

The image build copies the tree onto the image root (`cp -a overlay/. <image root>/`, keeping
the link), installs the daemon as `/usr/bin/hostkeeper` and `gh` and the trusted root under
`/usr/lib/hostkeeper/` (the deb or the rpm does that too; the daemon, started from there, copies itself into
the binary directory at the first start), and
fills `etc/hostkeeper/hostkeeper.toml.template` into `etc/hostkeeper/hostkeeper.toml` in the
image, root's, mode 0600, as above. Nothing else. The overlay files are the files of this
directory, byte for byte; the test above fails when they differ.

## Debugging a wedged node

The daemon binds no admin socket and `hostkeeper admin` is gone; an older daemon that still runs
under this unit binds `/run/hostkeeper/admin.sock` until the host updates. The status of a host is
`hkctl hosts get --node-id <id>`: whether the host is live, its last heartbeat, and each
service's lifecycle, health and step records (a pending request's `key_sha256`, to check
against the host, is in `hkctl enrollment list`). `hkctl hosts abort` stops a hung step and
`hkctl hosts clear-record` clears a step record that cannot be read. With the coordinator
unreachable, root stops the daemon and reads `journalctl -u hostkeeper.service`; `docs/admin.md`
is the runbook, including the by-hand rename of a record.
