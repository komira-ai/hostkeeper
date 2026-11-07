# Linux packaging

What a Linux host needs before hostkeeper can deliver anything to it: the daemon, its one unit
and its config (`docs/design/hostkeeper-design.md`, sections 4.1 and 4.6). The files here are
the sources of a deb, an rpm and a boot-image overlay. Building the daemon, building a boot
image, network boot, signing and publishing the packages are not here.

| file | what |
|---|---|
| `hostkeeper.service` | the unit, the daemon's only one: the daemon as root, `Type=notify`, with its paths as flags, started from the binary directory of its own update with `Restart=always` and no start limit of its own, and restarted by a 60-second watchdog its heartbeat loop feeds (`WatchdogSec=`, four heartbeat periods); it conflicts with kbf's update agent and its root half and is ordered after them, so a handoff stops one before it starts the other. hostkeeper's own package carries it too |
| `hostkeeper.tmpfiles.conf` | the daemon's directories and their modes, as `/usr/lib/tmpfiles.d/hostkeeper.conf` |
| `hostkeeper.toml.template` | the daemon config with a `@@NAME@@` placeholder in each value that belongs to a site |
| `nfpm.yaml` | the deb and the rpm, for nfpm |
| `scripts/postinstall.sh`, `scripts/preremove.sh` | the packages' scripts: make the directories, enable the unit, disable it on removal |
| `overlay/` | the same files laid out as an image root, for a boot-image build to copy |

`cargo test -p xtask --test packaging` reads every file here and checks it: the unit's keys and
command lines, that it starts `current` of the daemon's binary directory and restarts the
daemon's exit after its own update, that it agrees with the drop-in the daemon writes, that it is
the one unit file and the one hostkeeper's package is built with, that the directories are the
design's with the design's modes, that the template is refused until each placeholder is filled
and loads once they are, that the packages carry exactly the six files and the console's issue
link and no key, certificate, token or config, that `gh` and the trusted root are installed at the daemon's own paths from
where `cargo xtask release nfpm-stage` writes them, that the scripts never start the unit, and
that the overlay equals these sources.

## The binary the unit starts

The unit does not start `/usr/bin/hostkeeper` itself. It starts
`/var/lib/updater/state/hostkeeper/bin/current`, the binary directory of hostkeeper's own update
(design section 6.6): each version is one executable in `versions/<version>`, and `current` and
`previous` are links to them. Before each start, `ExecStartPre=` runs
`/usr/bin/hostkeeper self-update-seed`, which, when `current` starts no executable file, copies
`/usr/bin/hostkeeper` in as its own version and points `current` at it; otherwise it changes
nothing. So the first start of a host runs the binary the package installed, and every later
start runs the binary the last update chose.

- A self-update stages the new binary in `versions/`, switches `current`, and exits 0;
  `Restart=always` starts the new binary. At each start the daemon writes, where they differ,
  `/etc/systemd/system/hostkeeper-fallback.service` and the drop-in
  `/etc/systemd/system/hostkeeper.service.d/self-update.conf`, whose `OnFailure=` runs that unit,
  and then runs `systemctl daemon-reload`. When the new binary fails three starts within 300
  seconds, the fallback unit points `current` back at `previous` and starts the daemon again.
  A new binary that confirmed its update at its start has run, and is not put back even when
  that start fails later.
  When no update is waiting for its restart (a config that does not load, three restarts for
  another reason), the fallback puts nothing back and still starts the daemon again, so the
  daemon is retried for good, three starts at a time (`RestartMode=direct` keeps systemd from
  running the fallback before the three starts are spent, and the fallback has no start limit
  of its own); before the daemon has first been ready
  there is no drop-in, and the unit, which has no start limit of its own, retries it every
  second.
- Once `current` exists, upgrading the deb or the rpm replaces `/usr/bin/hostkeeper` and does not
  change the binary the daemon runs: hostkeeper's own update does that. A self-update also links
  the unit its package carries in `/etc/systemd/system/`, which then takes the place of the one
  the package installed; the two are the same file.
- The binary directory is under `/var/lib`, so the unit executes a file there: a host whose
  `/var/lib` is mounted `noexec`, or whose SELinux policy does not let systemd execute files of
  that directory, cannot start the daemon this way.
- A host whose unit predates this one (`ExecStart=/usr/bin/hostkeeper`, `Restart=on-failure`)
  gets it from the deb, the rpm or a boot image built from these files, not from a self-update:
  under that unit the daemon's exit after a self-update is final.

## The packages

`nfpm.yaml` says how to build them: one deb and one rpm for each Linux platform of hostkeeper's
packages, `linux-x86_64` (`amd64` for the deb, `x86_64` for the rpm) and `linux-arm64` (`arm64`,
`aarch64`). `hostkeeper_<version>_<arch>.deb` and `hostkeeper-<version>-1.<arch>.rpm` install:

| path | mode |
|---|---|
| `/usr/bin/hostkeeper` | 0755 |
| `/usr/lib/systemd/system/hostkeeper.service` | 0644 |
| `/usr/lib/tmpfiles.d/hostkeeper.conf` | 0644 |
| `/usr/share/hostkeeper/hostkeeper.toml.template` | 0644 |
| `/usr/lib/hostkeeper/gh` | 0755 |
| `/usr/lib/hostkeeper/trusted_root.json` | 0644 |

all owned by root. The last two are what the daemon checks every package's attestation with: a
pinned `gh` (the distribution's own may have no `gh attestation`) and the Sigstore trusted root
captured when the package was built. Before nfpm runs, `cargo xtask release nfpm-stage` checks
the `gh` archive against its pin and writes both into `target/nfpm/`; where they come from, how
the pin is bumped and what a stale trusted root does are in
`docs/deploy/verify-a-release.md`, "What the deb and the rpm check packages with".

The packages also install one link, `/etc/issue.d/hostkeeper.issue`, to
`/run/hostkeeper/enrollment.issue`: the console's issue file, which the daemon writes after each
step of its enrollment (the request's state, the coordinator and the key hash), so the login
prompt of the machine's console shows the key hash to check before approving
([docs/operations/enrollment.md](../../docs/operations/enrollment.md#check-the-key-hash)). The
link points at nothing until the daemon first writes the file.

On a first install the post-install script makes the directories of `hostkeeper.conf` and
enables the unit. It does not start it: the daemon first runs at the next
boot, or when the operator runs `systemctl start hostkeeper.service`, from the copy of
`/usr/bin/hostkeeper` its seed puts in the binary directory (above). An upgrade leaves the unit
enabled or disabled as it was. Removing the package disables the unit; it does not stop a
running daemon, and it removes none of the daemon's directories.

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
| `usr/lib/systemd/system/hostkeeper.service` | the unit |
| `etc/systemd/system/multi-user.target.wants/hostkeeper.service` | a link to the unit (relative, so it resolves in this tree too), which enables it |
| `usr/lib/tmpfiles.d/hostkeeper.conf` | the directories |
| `etc/hostkeeper/hostkeeper.toml.template` | the config template |
| `etc/issue.d/hostkeeper.issue` | a link to `/run/hostkeeper/enrollment.issue`, the console's issue file, as the packages install it |

The image build copies the tree onto the image root (`cp -a overlay/. <image root>/`, keeping
the link), installs the daemon as `/usr/bin/hostkeeper` and `gh` and the trusted root under
`/usr/lib/hostkeeper/` (the deb or the rpm does that too; the unit's seed copies the daemon into
the binary directory at the first start), and
fills `etc/hostkeeper/hostkeeper.toml.template` into `etc/hostkeeper/hostkeeper.toml` in the
image, root's, mode 0600, as above. Nothing else. The overlay files are the files of this
directory, byte for byte; the test above fails when they differ.

## Debugging a wedged node

On the host, as root, `hostkeeper admin status` asks the running daemon over its admin socket
(`/run/hostkeeper/admin.sock`) what it holds, with the coordinator up or down: whether the host
is enrolled (and the `key_sha256` of a pending request, to check against
`hostkeeper ctl enrollment list` before approving it), when its certificate expires
(`enrollment.not_after_unix`), whether its heartbeats reach the coordinator
(`link.last_success_unix`, `link.consecutive_failures`), and each service's lifecycle, health
and step records. `hostkeeper admin abort <service>` stops a hung step and
`hostkeeper admin clear-record <service>` clears a step record that cannot be read.
`docs/admin.md` describes each line of the output and the exit codes. When the command cannot
connect, the daemon is not running: `systemctl status hostkeeper.service` and
`journalctl -u hostkeeper.service` say why.
