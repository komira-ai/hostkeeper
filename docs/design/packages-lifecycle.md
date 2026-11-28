# Packages in any format: an update, a rollback and an uninstall

Status: design, for the maintainers' ruling. **Built** for a repository installer's version (`crates/hostkeeper-daemon/src/target.rs`, `repository.rs`, `builtin.rs`): section 6.1's `target.json` and its reader (`target::manifest`, `manifest.toml` for an archive version), the `l` tree record, `target_changed`; the built-in steps of 6.3; 6.4's `apt` row; 6.5's `remove` of `apt` with the `installed` entry's package keys; 6.6 for a repository package. `install-record.json`, `uninstalling_installed` and the other installers' rows are not built. This is section 6 of [packages.md](packages.md), kept
in a file of its own so that one stays under 1000 lines (rule R12). Section numbers here are those of packages.md.
Amended by [packages-repositories.md](packages-repositories.md): a Linux distribution's package comes from a
repository through the `apt` backend of one repository installer, whose version directory, rollback, hold, purge
and drift are that file's section 4; a Mac's `.pkg` comes through the `macos-pkg` installer of that file's section
4.7; the `deb`, `rpm`, `arch` and `apk` cases below are withdrawn with those installers. Revised by
[packages-native.md](packages-native.md): the `tarball` and `oci-image` cases below are withdrawn, the
`hostkeeper-archive` cases hold only until the move of its section 10.6 and are deleted with it, a hook is the
manifest's `hook.command`, run from its program's sealed copy in the tree (its section 10.2), and hostkeeper's own update installs its package after the
new binary confirms (its section 10.4).

## 6. An update, a rollback and an uninstall

### 6.1 The version directory

For every installer but `hostkeeper-archive` (a repository installer adds `apt/`, and `macos-pkg` adds `macos-pkg/`,
packages-repositories.md sections 4.2 and 4.7):

```
/var/lib/hostkeeper/packages/<service>/<version>/     0700, sealed
  target.json        the order's entry (installer, digest, manifest...), as canonical JSON
  artifact           the verified bytes, 0400
  tree/              what stage extracted: the hook, and the files the observer hashes
```

A `hostkeeper-archive` version keeps today's layout exactly: the unpacked archive at the version root (`manifest.toml`,
`bin/`, `units/`, `files/`), no `target.json`, no `artifact`, no `tree/`. Its seal's `package_digest` is the entry's
digest, and its hook program is relative to the version root, as today. So an older binary reads it unchanged, and a
version directory with no `target.json` is read as `hostkeeper-archive`, which is also how a new daemon reads every
version staged before A4. One loader, `VersionDir::read` in A6, replaces today's eight `Manifest::load` calls on a
version directory (order, boot resume, boot step, force, force switch, uninstall, observer, placer); it reads
`target.json` when present and `manifest.toml` otherwise.

The seal and `Staged::check` cover the whole directory, so the hook program, the kept artifact and the manifest are
re-hashed before every step, as the tree is today. The tree digest gains one record kind, `l`, for a symbolic link,
hashed by its target text, and `Staged::check`'s walk accepts a link only inside `tree/`, never follows one, and still
refuses a foreign owner, a setuid or setgid bit and a group- or other-writable entry (the walk of section 5.2 made the
tree so). Extracted package trees hold links, and today's walk refuses them. A tree with a link was
never readable by an older daemon, which is never ordered one (section 9.1), so the change is additive. A hook program
and a `place` source still must not pass through a link.

**A version already on the host.** An order for a version whose directory is sealed uses the sealed `target.json`, not
the order's manifest. The daemon compares the order's entry with it field for field and refuses a difference,
`Failed`, `release`, `target_changed`. A rollback to a version on the host reads only the sealed copy.

The step record is unchanged. An installer step's progress is kept in a file of its own,
`/var/lib/updater/state/<service>/install-record.json`, which an older daemon does not read. Every installer step is
idempotent, so a step found running at start is run again.

### 6.2 The boundary: the installer puts bytes on the host, the hook hands work over

An update runs, in order:

1. Fetch the artifact and its bundle, verify both (section 7.4).
2. Check the manifest (section 4.4). Installer `stage`, then seal.
3. Hook `prepare`, from the new version's tree.
4. Installer `activate` of the new version. This takes the place of today's switch of `current` before `cutover`.
5. Hook `cutover`, `verify`, `cleanup`, as today.
6. After a confirmed switch: installer `retire` of every version but `current`, `previous`, the pinned one and the newest
   `--keep-versions` others (today's prune, hostkeeper-design.md Q14).

The installer changes what is on disk. The hook changes what runs: it stops the old processes, drains, migrates, starts
the new ones and says whether the new release serves. A package whose own scripts restart the service (a `postinst` running `deb-systemd-invoke restart`)
would do the hand-over outside the hook. On `apt` the installer prevents it for a script that goes through `invoke-rc.d` or `deb-systemd-invoke`,
as debhelper's do: it holds a `policy-rc.d` that exits 101 around every dpkg run that can run a script (packages-native.md section 10.2), so the packager need not build its
packages with `--no-start --no-restart-after-upgrade`; the hook's `cutover` or the built-in steps start the units. On a
`macos-pkg` nothing holds a script back, and a job it loads starts before `cutover`. The daemon writes the `[Install]`
links of the `run` units it places (`tarball`, `hostkeeper-archive`) and of a repository package's units whose files are
in the tree, leaves a link a package's scripts made, and starts a `run` unit that has no link at `cutover` and at every
daemon start (packages-repositories.md sections 5 and 4.7). The placer's unit lint applies only to placed units.

### 6.3 Built-in steps, for a service with no hand-over of its own

A manifest whose `hook.command` is empty gets the daemon's built-in steps (a non-empty command is the absolute path of a file the package installs, with leading arguments, packages-native.md section 10.2), which are shipped code like an installer:
`prepare` and `cleanup` do nothing; `cutover` restarts every `run` unit and waits within its bound for the run set to be
live (one of its units active, as hostkeeper-design.md section 5.8 reads it, so a standby unit kept inactive fails
nothing); `verify` passes when the run set is live and `health_url`, if any, answers 2xx, and defers otherwise until
`verify_deadline_secs`; `rollback` restarts the units again, after the switch back; `uninstall` does nothing. This is
what most packaged services need, and what an OCI image needs, since an image carries no hook.

A service on built-in steps can always be uninstalled, whatever `timeout_secs.uninstall` says, because its `uninstall`
runs nothing. A service with a hook program and no `uninstall` bound is refused `uninstall_unsupported`, as today.

### 6.4 What "previous version kept for rollback" means

The previous version's directory keeps its verified artifact (a repository installer: the index and package it
fetched), so a rollback never fetches, for every format:

| Installer | Rollback of an update, before the hook's `rollback` step (a first install's removal comes after it, below the table) |
|---|---|
| `hostkeeper-archive`, `tarball` | Switch `current` back and relink, as today |
| `apt` (and each later repository backend) | A downgrade by one version from the previous version's kept index and package, with no fetch (`apt-get install --no-download --allow-downgrades`, packages-repositories.md section 4.4), running the older package's scripts. A service that ships these packages keeps its scripts able to go back one version, as its hook must read the record one version back today |
| `macos-pkg` | `installer -pkg` of the previous version's kept `.pkg`, with no fetch and no second signature check, then delete the files of the newer version's `Bom` that the older one does not list (packages-repositories.md section 4.7) |
| `oci-image` | The previous image is still loaded (`retire` keeps it while `previous` names it). Re-point the generated units at it and reload |

A first install that fails has no previous version: `remove` takes it off the host, as `remove_all` does today.

**A package that is already installed is adopted.** A failed first install never purges a package hostkeeper did not install. When an
`install` order for a repository service finds its package installed whole in the host's package manager and no record of
the service in `placed.json` (installed by hand, or with the host's setup), the order is not a first install: the
daemon adopts the package and handles the order as an `update` from the installed version. In the order's checks it
stages the installed version from the order's repository, as a version directory of the service (its name the version
the package manager shows; its `target.json` the order's entry with that package version, the digest the signed
index gives and no hook command, so it runs the built-in steps); the package is held and recorded as installed by the
installer, and `current` names the adopted version before the step runs. The update's failure then restores that
version (section 6.4's downgrade) and purges nothing; a verdict that needs the rollback is the update's, never a
first install's. An order for the version that is installed adopts it, and its rollback installs it again. A package
that is not installed, is installed half-way, or that `placed.json` already records for the service from a repository (an earlier try
of this install left it, so it is hostkeeper's own) is a first install as before; the record an adoption writes names no repository, so an adoption that stopped before it made `current` is done again by the next order. A version the repository does not
list ends the order `Deferred`, `not_in_repository`, and one whose text is no version name of a service ends it
`Failed`, `manifest_invalid`; both leave the package as it was. The adopted version is pinned to the digest the signed
index gives for that version string, whatever pin the order carries, so a package built by hand at the same version
string as the repository's is taken as that release, and a later rollback installs the repository's copy over it. Once
adopted, the package is the installer's own record: an uninstall (section 6.5) removes it as it removes any installed
package. `macos-pkg` adopts nothing: `prepare` finds no installed package there, so a first install over an
existing receipt that fails still rolls back by removing the package; this is open for the owner to decide, and is not
what packages-repositories.md section 4.7 describes (it refuses a package found on the host).

**The order of a first install's rollback: the hook first, then the package.** The hook's `rollback` runs while the
package, its unit files and its loaded units are still on the host, so the hook can stop and inspect them; only after
it has ended does the daemon do the package-level rollback (`apt-mark unhold`, `dpkg --purge`, the daemon reload, and
the removal of `current`). The removal is made when the hook's `rollback` succeeded or declined the install
(`no_previous_release`). A `rollback` that fails or defers leaves the package where it is: it is run again within its
retries, and when it ends in a `halt` the package stays for whoever looks at the halt. A removal that fails ends the
switch as before (`link_failed`); the hook's `rollback` is already recorded done, so the removal alone is run again, also after a crash, and the hook never runs on a purged package. A hook that cannot be started leaves nothing to stop: the package is removed at once. A removal
that ends the step itself (a package manager that stayed busy) gives its exit in place of the hook's. An update's
rollback is unchanged: the previous version is activated again before the hook's `rollback`, which restarts it.

### 6.5 Uninstall and closed-world removal

Uninstall keeps today's order (`crates/hostkeeper-daemon/src/uninstall.rs`): tombstone, stop the `run` units, the hook's
`uninstall`, then installer `remove` of the service, then delete the version directories, the step record,
`install-record.json` and the service's `installed` entry. Each is durable before the next and resumes at the
tombstone; `remove` is idempotent, so a resume after `hook_done` runs it again. A service of any installer but
`hostkeeper-archive` is tombstoned under a new key, `uninstalling_installed`, which an older binary keeps when it
rewrites `placed.json` and does not act on. So an older binary after a fallback never finishes such an uninstall by
deleting the version directories while the package stays installed.

Each installer removes what the format's tool records for the package, plus what hostkeeper recorded at `activate`,
and nothing else. hostkeeper records in `placed.json`, under a new additive key `installed`, each service's installer
and, for a repository package, the package identity, the version it installed and the repository (for
`macos-pkg`, the identifier, the version and `created_dirs`, the directories its installs created; and, for
`oci-image`, whether it loaded the image).

- `tarball` and `hostkeeper-archive`: the links `placed.json` lists, while each is still the recorded link, as today.
- `apt` (and each later repository backend): the package the manifest names, unheld and purged with `dpkg --purge`
  at whatever version is installed (packages-repositories.md section 4.4). The
  format's database is the list of the package's files. hostkeeper refuses a package that another service records
  (`package_conflict`). A package installed with no record, or moved by hand to another version, is drift: hostkeeper
  installs the desired version over it, holds and records it, and reports the drift (packages-repositories.md
  section 4.4).
- `macos-pkg`: every file and link the receipt or a kept version's `Bom` lists, whatever its content now, but one a
  receipt of another identifier lists; the
  directories hostkeeper's installs created, when empty; then `pkgutil --forget` of the identifier
  (packages-repositories.md section 4.7). A package installed by hand, or at another version, is drift, corrected and
  reported as for `apt`.
- `oci-image`: the generated units, and the image only if its own `activate` loaded it.

What stays, by each format's own rules: files a package's scripts made that the format does not list (a user, a cache,
an alternative), and the
service's data under `/var/lib/<service>`, which is the hook's business, never the daemon's, as today. A package's own
removal scripts may delete what they made; that is the service's packaging choice. The device tests of section 12 (packages-tests.md) show
the fixture packages leave nothing behind.

**A file nobody owns is kept at a first install** (**Built**: `crates/hostkeeper-install/src/kept.rs`, called by the
`activate` of `apt` and `macos-pkg` and by their removal). A first install, one with no record of the service yet, can
land on a file an operator placed by hand at one of the package's payload paths, such as a flags file written when the
node was set up. Before the package manager replaces it, the installer's `activate` keeps an exact copy of each such
file, its bytes, mode, owner and group, and those of its parent directories, in hostkeeper's own root-only store,
`/var/lib/hostkeeper/kept/<service>/` (0700, each copy 0600, beside `kept.toml`, which names the package and each
file's path and SHA-256). A file is kept only when no package owns it: `dpkg-query --search` names no package for
the path on `apt`, and no receipt lists it on `macos-pkg` (`pkgutil --file-info-plist`, already asked of every
payload path on the host). A file another package owns is not kept, and the install is refused as it was before
this store: dpkg refuses to overwrite another package's file (`dpkg_error`), and `macos-pkg` refuses it
(`package_conflict`). A path with nothing on it, a link or a directory is not kept either. The copies are made before
the pending record is written, so an `activate` run again after a crash finds no record and keeps the same files again;
one run again after its install failed finds the pending record and keeps nothing new. An update keeps nothing new
and leaves the first install's copies as they are: the file is still the operator's if the package is ever removed.
The removal of the service, however it comes (a failed first install's rollback, after the hook's `rollback`, or an
uninstall), puts each copy back at its path once the package manager has removed the package (after the purge on
`apt`; after the receipt's files are deleted and the receipt forgotten on `macos-pkg`), making again any parent
directory the removal took away, as it was, and only then drops the service's record; the copies are dropped after the
record. A removal run again after a crash between the two puts the files back again; one with no record only drops
the copies, since either the install they were kept for never ran or a removal already put them back. A copy that
cannot be put back (missing, unreadable, its bytes no longer its recorded SHA-256, or its path unwritable) is, on
purpose, neither a silent loss nor a failed removal that would leave the package half there: the removal completes,
the path stays as the removal left it, the service's copies are set aside in `/var/lib/hostkeeper/kept/.lost-<service>/`
(still root's alone; a later loss of the same service replaces them), and the daemon logs an alert naming the path and
where its copy is, or that none is left (`alert=kept_file_lost`, at error level, one line per path), so the operator
can put the file back by hand.

### 6.6 Observation

The observer finds the first `run` unit's main program, as today, and hashes the running process's executable against
the same path in the version's sealed tree. For `tarball` and `hostkeeper-archive` the unit file is the placed one, as
today. For a repository package it reads the unit text from the tree extracted from the downloaded package, at the path the
package installs it (`tree/lib/systemd/system/<unit>` or `tree/usr/lib/systemd/system/<unit>`),
and maps its program onto the tree: for a `.deb` that installs `/usr/bin/tierstore`, `tree/usr/bin/tierstore`. For
`macos-pkg` it reads the program from the plist in the tree (`tree/Library/LaunchDaemons/<label>.plist`) and maps it
onto the tree the same way. The
extracted tree is the attested (or, for a repository package, signed) package's files, so a process left from another version still shows `version_mismatch`.
For an `oci-image` unit, the running version is the image digest the runtime reports.

### 6.7 A halted install whose package is gone

A `Finished{Halt}` step record holds every order of its service until `clear_halt` (design/host-updates.md, 3.8). One order is let through: an `install` of a service with no release installed (no `current`) that is not being uninstalled. The halt protected a host state that is no longer there (a first install's `rollback` takes the package off the host, or an operator removed it), so the daemon turns the record into the `halt_cleared` deferral, as `clear_halt` does, logs `alert=halt_forgotten` and runs the install. The same order again (the same package and try) is still answered from the record with its halt. An `update`, a `force_rollback` and a service with a release installed stay halted. The coordinator's side, which clears its own halt when the halting host reports the service absent, is in design/host-updates.md, 3.8; it clears only a halt of the order's first try, and its new generation lifts the daemon's hold through `clear_halt` as for an operator's clear.
