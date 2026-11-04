# Linux packaging

What a Linux host needs before hostkeeper can deliver anything to it: the daemon, its one unit
and its config (`docs/design/hostkeeper-design.md`, sections 4.1 and 4.6). The files here are
the sources of a deb, an rpm and a boot-image overlay. Building the daemon, building a boot
image, network boot, signing and publishing the packages are not here.

| file | what |
|---|---|
| `hostkeeper.service` | the unit: the daemon as root, `Type=notify`, with its paths as flags |
| `hostkeeper.tmpfiles.conf` | the daemon's directories and their modes, as `/usr/lib/tmpfiles.d/hostkeeper.conf` |
| `hostkeeper.toml.template` | the daemon config with a `@@NAME@@` placeholder in each value that belongs to a site |
| `nfpm.yaml` | the deb and the rpm, for nfpm |
| `scripts/postinstall.sh`, `scripts/preremove.sh` | the packages' scripts: make the directories, enable the unit, disable it on removal |
| `overlay/` | the same files laid out as an image root, for a boot-image build to copy |

`cargo test -p xtask --test packaging` reads every file here and checks it: the unit's keys and
command line, that the directories are the design's with the design's modes, that the template
is refused until each placeholder is filled and loads once they are, that the packages carry
exactly the four files and no key, certificate, token or config, that the scripts never start
the unit, and that the overlay equals these sources.

## The packages

`nfpm.yaml` says how to build them. `hostkeeper_<version>_<arch>.deb` (`amd64`, `arm64`) and
`hostkeeper-<version>-1.<arch>.rpm` (`x86_64`, `aarch64`) install:

| path | mode |
|---|---|
| `/usr/bin/hostkeeper` | 0755 |
| `/usr/lib/systemd/system/hostkeeper.service` | 0644 |
| `/usr/lib/tmpfiles.d/hostkeeper.conf` | 0644 |
| `/usr/share/hostkeeper/hostkeeper.toml.template` | 0644 |

all owned by root. On a first install the post-install script makes the directories of
`hostkeeper.conf` and enables the unit. It does not start it: the daemon first runs at the next
boot, or when the operator runs `systemctl start hostkeeper.service`. An upgrade leaves the unit
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
(design section 4.1); systemd then stops trying after three failed starts within 300 seconds
(`systemctl reset-failed hostkeeper.service` clears that). What the daemon does on a host that
has a config but no certificate yet, its enrollment, is design section 5.7.

## The boot-image overlay

`overlay/` is the root of an image, holding only what a boot image adds for hostkeeper:

| path | what |
|---|---|
| `usr/lib/systemd/system/hostkeeper.service` | the unit |
| `etc/systemd/system/multi-user.target.wants/hostkeeper.service` | a link to the unit (relative, so it resolves in this tree too), which enables it |
| `usr/lib/tmpfiles.d/hostkeeper.conf` | the directories |
| `etc/hostkeeper/hostkeeper.toml.template` | the config template |

The image build copies the tree onto the image root (`cp -a overlay/. <image root>/`, keeping
the link), installs the daemon as `/usr/bin/hostkeeper` (the deb or the rpm does that too), and
fills `etc/hostkeeper/hostkeeper.toml.template` into `etc/hostkeeper/hostkeeper.toml` in the
image, root's, mode 0600, as above. Nothing else. The overlay files are the files of this
directory, byte for byte; the test above fails when they differ.
