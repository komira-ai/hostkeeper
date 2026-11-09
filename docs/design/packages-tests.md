# Packages in any format: the tests of each build item

Status: design, for the maintainers' ruling. The tests of A1 are **Built** (`crates/hostkeeper-release/tests/manifest.rs`,
`crates/hostkeeper-proto/tests/additions_targets.rs`), and so are those of A2
(`crates/hostkeeper-coordinator/tests/targets.rs`, `crates/hostkeeper-proto/tests/additions_releases.rs`, and the
Postgres store's in `crates/hostkeeper-coordinator/src/store/postgres/tests/releases.rs`), and so are those of A3
(`crates/hostkeeper-coordinator/tests/target_orders.rs`, `crates/hostkeeper-proto/tests/additions_installers.rs`, and
the pick's own bounds in `crates/hostkeeper-coordinator/src/picks_tests.rs`), and so are those of A4
(`crates/hostkeeper-daemon/tests/stores.rs`, `crates/hostkeeper-package/tests/fetch.rs`) and of A16 (below), and
of A6 the announce cases of `hostkeeper-archive` (`crates/hostkeeper-install/tests/announce.rs`) and the contract
and registry (`crates/hostkeeper-install/tests/installer.rs`), of A9 the steps and signers file of `macos-pkg`
(`crates/hostkeeper-install/tests/macos_pkg.rs` and the crate's unit tests, the device tests in
`crates/hostkeeper-privileged/tests/install_macos_pkg.rs`), and of A7 the repository configuration
(`crates/hostkeeper-install/tests/repositories.rs`), the `apt` installer against the command port's fake
(`crates/hostkeeper-install/tests/apt.rs`) and its device test (`crates/hostkeeper-privileged/tests/install_apt.rs`),
the announce rule of `apt` (`tests/announce.rs`), the daemon's order path of `apt`
(`crates/hostkeeper-daemon/tests/apt*.rs`, `repositories.rs`, and the daemon case of `install_apt.rs`), and `ServiceBeat.drift`
(`crates/hostkeeper-proto/tests/additions_installers.rs`, `crates/hostkeeper-link/src/proto_map_tests.rs`,
`crates/hostkeeper-coordinator/tests/drift_shown.rs`); nothing else here is built. This is section 12 of
[packages.md](packages.md), kept in a file of its own so that one stays under 1000 lines (rule R12). Section numbers here
are those of packages.md, and A1 to A15 are its build items (section 13). A16, A17 and the re-scoped A7 and A9 are
[packages-repositories.md](packages-repositories.md)'s (its section 8); the `deb`, `rpm`, `arch` and `apk` cases
are withdrawn with those installers, and that file's cases are the five items before A15's: A16's, A7's two, A9's
(`macos-pkg`) and A17's. Revised by [packages-native.md](packages-native.md): its items A18 to A28, and what it changes
in A6, A7 and A9, have the cases of the last section below; the `tarball` and `oci-image` cases are withdrawn with those
installers, and the archive's cases go with the code A25 and A28 delete.

Each item's tests are behaviour tests in Rust, in the crate the item changes (rules T8, T11, T12). Each runs with no
network and no system path, through the fakes the crates already have (`Fetcher`, `Verifier`, the coordinator's backend
fake, the step launcher's fake, the placer's roots under a scratch directory), and through a command port for the
formats' tools. A host's `os-release` and the tools' paths are read under a scratch root the test makes. None asserts a
time: a bound is tested on an injected clock (rule T10). Real `apt`, `dpkg`, `installer`, `pkgutil` and `podman`
run only in the device jobs (`privileged`, `macos-device`), whose tests count toward no coverage gate.

- **Manifest and target checks (A1; `crates/hostkeeper-release/tests/manifest.rs`).** One case per check of sections
  3.4 and 4.4: each field an installer requires missing, each it refuses present; an unknown installer; `min_hostkeeper`
  above the daemon (`daemon_too_old`, not `manifest_invalid`, even with another field wrong); every bound at 0 and at
  its limit, and `verify_deadline_secs` one below `verify + rollback`; a hook path with `..`, `.`, a leading `/`,
  through a link, a directory, missing from `tree/`; a hook `command` for `oci-image`; a unit name of another service,
  without a suffix, with a bad instance; a container `unit` not in `run`; a file path under each refused prefix and one
  beside it; a volume host path outside `/var/lib/<service>/` or with `..`; a host port 1023 and 65536; a health URL
  to a non-loopback host; an `updater.` param; each count limit one under and one over; a manifest of 16 KiB and one
  byte over; an entry of 24 KiB and one byte over; 2 KiB of locators and one byte over; an attestation absent, of
  0 bytes, of 1 MiB and one byte over; 9 locations. On the lists: a platform with an empty list, with 8 entries and
  with 9; two entries of one installer in one list; an entry whose installer is empty, of 32 bytes and of 33, or not of
  `[a-z0-9-]`; a digest of 63 hex digits or with an upper-case letter; `bytes` 0 and 1.
- **Deploy call (A2; `crates/hostkeeper-coordinator/tests/targets.rs`).** Every row of section 3.4, one refusal each, a
  fourth platform key, no target at all, `hostkeeper` with a `tarball` entry and with two entries. An identical repeat
  is accepted; the same entries in another order, or a different artifact or manifest, under the same version is
  `release_exists`. An index that disagrees by one digest, one size or one platform is refused, and one that agrees is
  accepted, including an index of two platforms with the third null, and a list whose archive entry is second. The
  JSON route with `"linux-arm64": null` and with the key left out give the same release. A body of 256 KiB is accepted
  and one byte over refused, on both routes, and every other method is still refused over 4 KiB. A set with targets by
  CI and by an operator each writes one audit entry with each entry's digest and its manifest's digest. `GetRelease` of
  an unknown service is `unknown_service`, of a version never set with targets `release_unknown`, and answers the
  lists in the order set. The previous coordinator starts on the new state, on the file store and on Postgres, and shows
  the version `no_digest`.
- **Ordering (A3; `crates/hostkeeper-coordinator/tests/target_orders.rs`).** Over a mixed fleet (old and new daemons,
  three platforms; the installer names are opaque strings to the pick, so the test keeps `deb` and `rpm`; hosts announcing `deb`, `rpm`, only `tarball`, and nothing): a host of a platform with no list is
  `no_target`, not failed, not missing, and does not hold convergence; with the list `[deb, rpm, tarball]`, a host
  announcing `deb` and `tarball` gets the `deb` entry, one announcing `rpm` and `tarball` the `rpm` entry, one
  announcing only `tarball` the tarball, and with `[tarball, deb]` every one of them the tarball; with `[deb, rpm]` the
  tarball-only host is `no_build_for_host`, not failed, not missing, not holding convergence, and listed with its
  installers; an old daemon gets the list's `hostkeeper-archive` entry wherever it stands, as today, with the index
  digest and with none when the set had no index, and is `no_build_for_host` when the list has none; a host that runs
  the service from its `tarball` gets the list's `tarball` entry although a `deb` entry comes first, and is
  `installer_dropped`, named by `Converge`, when the list has no `tarball` or its daemon no longer lists `tarball`; an
  old daemon that runs the service from archives, given a list with no archive entry, is `installer_dropped`; a list
  with a misspelt installer that no host is picked for leaves the version not converged, with the counts in
  `Converge`; a `target-v1` host is picked nothing before its first gRPC beat, after its own start and after the
  coordinator's; a `ForceRollback` to a version with no entry of the host's installer is not ordered
  (`installer_dropped`), and to a version set with an index is ordered as today; a target is sent only to a daemon that
  announced `target-v1`, only over gRPC, and the feature is echoed; one host beating over gRPC, then JSON, then gRPC
  keeps its first installers list, gets an archive order in today's form and no `deb` order over JSON, and the `deb`
  order in the next gRPC reply; a host whose next gRPC beat lists other installers is picked for again from the new
  list; an order answered `installer_not_listed` is picked for again at the next beat; a `Force` and an `Install` to a
  version set with targets follow the same rules; a reply with the largest valid entry fits in 32 KiB.
- **Daemon fetch and proof (A4; `crates/hostkeeper-daemon/tests/stores.rs`, `crates/hostkeeper-package/tests/fetch.rs`).**
  Every row of section 7.4's table: a mirror wins; a mirror with wrong bytes then a right location; only mirrors wrong;
  a hung mirror then a location; every source down; every location wrong; no store; one byte over `bytes`; a redirect to
  `http://` and one past the count; a credential not sent after a cross-origin redirect; a realm on another origin; a
  bundle over 1 MiB; each refused `stores.toml`; no `stores.toml`. On the order: an entry whose digest is not
  `package_digest`, a locator with `..`, `%`, a scheme, `//`, a query or 1025 bytes (`order_invalid`, reported); an
  entry of an installer the daemon does not list (`installer_not_listed`, deferred, not `manifest_invalid`, nothing
  fetched); a manifest
  refused before any fetch; a bundle that does not list the digest (`not_attested`), and one bundle that lists the
  digests of every entry of the release accepted for each; an order without a target takes the channel; a reply of
  32 KiB read and one byte over refused. On earlier binaries: the mixed cases of section 9.3.
- **Which installers a daemon announces (A6 for `hostkeeper-archive` and `tarball`, A7 to A10 for theirs;
  `crates/hostkeeper-install/tests/announce.rs`).** One case per row of section 5.4's table, with `ID` alone and with
  the family only in `ID_LIKE` (`linuxmint`, `rocky`, `amzn`, `opensuse-leap`), and `manjaro` (whose `arch` family no longer maps) announcing no native installer; an `ID` that maps to one
  family ahead of an `ID_LIKE` token of another; a Debian host with `/usr/bin/rpm` present announces `apt` and no
  other native installer; a Debian host without `dpkg`, with an apt older than 2.2, or with no repository entry,
  announces no native installer; quoted and unquoted values give the same family;
  `/usr/lib/os-release` is read when `/etc/os-release` is missing; an unknown `ID`, an empty file, both files missing
  and a malformed line (no `=`, an unterminated quote) give no native format, so a Linux host announces
  `hostkeeper-archive` and `tarball` alone; `oci-image` is announced with `/usr/bin/podman` present and not without it
  (A10); a macOS host announces `hostkeeper-archive` and `tarball`, and `macos-pkg` with a valid `macos-pkg.toml`, and never reads
  `os-release`. The list is the same at every heartbeat of one start.
- **Installers (A6 to A10; `crates/hostkeeper-install/tests/<installer>.rs`, device tests in
  `crates/hostkeeper-privileged/tests/install_<installer>.rs` and the macOS device suite).** For each installer:
  `stage` refuses a package whose identity is not `package` (and an OCI archive of two images), and runs no script
  (the fake tool records its calls). For every repository installer, the dependency check of section 5.4 runs after the
  identity check and before `prepare`; when the simulation refuses, the outcome is `dependency_missing`, the hook
  never runs and nothing is installed. The walk after `stage` refuses an entry under a link, an absolute or escaping link, a hard
  link, a device, one entry over `max_entries` and one byte over `max_unpacked_bytes`; it makes a non-root owner root,
  strips a setuid bit and sets modes; on macOS an unsigned Mach-O file is refused. Every tool run but a repository
  installer's two fetch commands is in a unit with `PrivateNetwork=yes` (the fake launcher records the unit's properties). `activate` twice is the same as once, and a
  crash between the tool and the switch resumes to the same state. An `activate` that fails rolls back to `previous`,
  and a first install that fails is removed. A tool past its bound is stopped and the step fails `environment`. Two
  services' `activate` never run the tool at once. Rollback installs the kept previous artifact, or a repository
  installer's kept index and package, with no fetch. `retire` of version N-2 after
  N-1 and N leaves every installed file of N untouched and never calls the tool. `remove` removes what `activate`
  recorded, and leaves an image another kept version uses or the runtime held before (`oci-image`). Then
  `package_conflict`, `installer_changed`, and `target_changed` for a sealed version ordered again with another
  manifest. The generated container unit for each combination of `args`, ports (loopback or not) and volumes
  (read-only or not) equals its golden text. The observer hashes the program a native unit names in `tree/` and
  reports `version_mismatch` for a process of another version, and the image digest for `oci-image`. The device tests
  install, update, roll back and uninstall a two-version `hello` with each installer on the hosted runner (a later
  manager's in a container of its family), and assert that nothing the installer put there remains after uninstall.
- **Repository entries (A16; `crates/hostkeeper-release/tests/manifest/repository.rs`, in the crate's one test binary,
  `crates/hostkeeper-coordinator/tests/targets/repository.rs` and `tests/target_orders.rs`, the previous coordinator's
  reader in `crates/hostkeeper-coordinator/src/releases_tests.rs` and `src/store/postgres/tests/releases.rs`, and
  `crates/hostkeeper-proto/tests/additions_repository.rs`).** One refusal per row of packages-repositories.md section 2.2:
  a `repository` empty, of 32 bytes and of 33, or with an upper-case letter; a `package_version` absent, of 128 bytes
  and of 129, starting with a letter, or with a space; a `package_version` on an entry without `repository`; a
  repository entry with one location, with `bytes` 1, with an attestation; a pin of 63 hex digits and one with an
  upper-case letter, and none, each refused; `manifest.package` absent, of 128 bytes and of 129, starting with `-`; the
  service `hostkeeper` with a repository entry. A list `[apt, tarball]` is accepted and stored; set again identical it
  is accepted, and with another `package_version` or pin it is `release_exists`. The audit entry names the repository,
  the package, its version and the pin. The previous coordinator's reader starts on a
  releases file and a Postgres table holding a repository entry and writes the entry back unchanged. A reader built
  from the baseline keeps fields 7 and 8 and writes them back; the new reader reads an entry without them as an
  artifact entry. The pick orders an `apt` entry only to a host whose daemon lists `apt`.
- **The repository configuration (A7; `crates/hostkeeper-install/tests/repositories.rs`).** A valid entry loads with its
  name, URI, suite, components and keyring; the start is refused for each defect of packages-repositories.md section
  3.1: a name outside the grammar, a file or keyring not root-owned or group- or other-writable, two stanzas, a type
  other than `deb`, two URIs, an `http://` URI, two suites, no component, a `Signed-By` that is relative, outside the
  directory or an inline key, and each of `Trusted`, `Allow-Insecure`, `Allow-Weak`, `Allow-Downgrade-To-Insecure` and
  `Check-Valid-Until: no`. An empty directory and a missing one load as no repository.
- **The `apt` installer (A7; `crates/hostkeeper-install/tests/apt.rs` with the command port's fake, device tests in
  `crates/hostkeeper-privileged/tests/install_apt.rs`).** Every apt run's command line carries every option of
  packages-repositories.md section 4.2, its work directory and no host path for lists or caches (the fake records each
  call). Each row of section 4.5 gives its outcome and changes nothing on the host: `repository_unknown`;
  `fetch_failed`; `signature` for each of apt's signature errors; `not_in_repository`, deferred, for a missing version
  and for a stanza of another architecture only; `package_digest_mismatch` for the index's digest; `store_digest_mismatch` for a downloaded file that is not the pin;
  `package_too_large` at `max_package_bytes` plus one, and a package of exactly `max_package_bytes` accepted;
  `manifest_invalid` for each control field and for a `run` unit not in the tree, and a
  package of architecture `all` accepted; the simulation run before any download, and a held package updated;
  `dependency_missing`; `dependency_changes` for an extra install, upgrade, downgrade and removal in the simulation,
  and for a purge refused by a dependent; `package_manager_busy`; `dpkg_interrupted`; `dpkg_error` for a failing
  maintainer script and for a version other than V after the install, each followed by the rollback to the previous
  version, and on a first install by the purge. An `apt` entry with no `repository` is `coordinator_too_old`, deferred;
  an `apt` entry with a location, and a `tarball` entry with a `repository`, are `order_invalid`. Only `stage`'s two
  fetch commands run without `PrivateNetwork=yes`. `activate` sets the hold after the install and records the version;
  run twice, the second run changes nothing; a crash between the install and the record resumes to the same state.
  A rollback runs no `apt-get update` and no download, uses the previous version's sealed copy, and succeeds on a kept
  index whose `Valid-Until` has passed. A later version that adds a `run` unit has it enabled, and one that drops a
  unit has its links taken away. `remove` lifts the
  hold and purges the package at whatever version is installed, reporting drift when it is not the recorded one. Drift, for a version moved by hand and for a package installed with no record: the heartbeat's `drift`
  report names the version found and the one recorded (or none); the next `install` or `update` installs the desired
  version over it (a downgrade included), holds and records it, and ends `Succeeded`, `drift_repaired`, counting toward
  no rollback; and `package_conflict` for a package another service records. The first install enables the `run` units by recorded links and
  `remove` takes them away; an enablement link the package made is left unclaimed. The device tests, against a
  fixture repository on the runner's disk (`file:` URI) signed by a key the test makes, on the oldest and newest
  Debian and Ubuntu releases the project lists: install, update, roll back with the repository removed from disk, and
  uninstall a two-version `hello`; an unattended `apt-get upgrade` with the same repository in the system's sources
  leaves the held version; a suite signed by another key is `signature`; a re-uploaded package under the same version
  is `package_digest_mismatch`; a package moved by hand with `apt-get install` is put back to the desired version and
  reported as drift; after the uninstall `dpkg-query` knows no `hello` and none of its files
  remain.
- **The `macos-pkg` installer (A9; `crates/hostkeeper-install/tests/macos_pkg.rs` with the command port's fake, device
  tests in the macOS device suite, `install_macos_pkg.rs`, in `macos-device`).** The start is refused for each defect of
  `macos-pkg.toml` (packages-repositories.md section 4.7): a file not root-owned, one writable by group or others, an
  unknown key, no signer, five signers, and a signer of 63 hex digits or with a letter past `f`; four signers start,
  and the leaf's fingerprint as `pkgutil` prints it (upper case, spaced, over two lines, after an intermediate's and
  the root's in the same output) matches its entry, while an intermediate's fingerprint as the entry does not. A Mac
  without the file starts and does not announce `macos-pkg`. Each row of section 4.7's failure table gives its outcome
  and changes nothing: `order_invalid` for an entry with `repository` and one with `package_version`; `signature` for
  an unsigned package, an untrusted chain and a leaf no signer names, with nothing expanded; `manifest_invalid` for a
  product archive of two components, a component not selected by default, a `Distribution` with a script, an
  identifier other than `package`, a version other than the order's, an `install-location` other than `/`, a
  `relocate`, a `bundle`, a `Bom` that lists a file the payload lacks and a payload file with a setuid bit the `Bom`
  does not show, a `Bom` entry not owned by root, of group staff, group-writable, under `/usr/local` and under another
  service's directory, a `run` label with no plist, one whose `Label` differs and one without `Disabled`;
  `package_conflict` for a second service with the same identifier and for a payload file a receipt of another
  identifier lists, while a file of the same identifier's receipt (installed by hand) and a file no receipt lists are
  installed over; `unsafe_directory` for a parent that another account owns or can write; `package_manager_busy` in
  the form the fake gives; and `installer_error` for a non-zero exit, a version not shown after it and an installed
  file that differs from `tree/`, each followed by the rollback, or on a first install the removal. No tool run
  fetches, and `installer` runs with `-allowUntrusted` (the fake launcher records each). `activate` twice is the same
  as once; an update deletes the files of the previous `Bom` that the new one does not list and no other, records in
  `created_dirs` only directories that were not there, and enables each `run` label and disables a dropped one. A
  rollback installs the kept `.pkg` with no fetch and no signature check, with the signer taken out of the file, and
  deletes the newer version's files and the directories it created. `retire` never runs `installer` or `pkgutil`.
  `remove` deletes every file the receipt lists, a changed one too, leaves a file the receipt does not list and one a
  receipt of another identifier also lists, deletes nothing through a linked parent, removes the created directories
  left empty and never a directory that was there before, and forgets the receipt. Drift: a receipt at another
  version, a receipt with no record, a record with no receipt, a payload file deleted and one with other bytes each
  give a `drift` report, a file whose size, mode and time are unchanged is not hashed again, and the next update
  repairs each and ends `Succeeded`, `drift_repaired`. The device tests, as root, against packages signed by an
  identity the test makes and trusts for the run, served from an `https` store on loopback (section 4.7 there): install,
  update, roll back with the store stopped and with the test's root taken out of the trust settings, and uninstall a
  two-version `hello`; the installed plist's job shows disabled (`launchctl print-disabled system`) until `activate` enables it; an `installer
  -pkg` of the older version by hand is put back and reported as drift; the job runs the installed program and the
  observer matches it to the tree; after the uninstall `pkgutil --pkgs` lists no `hello` and none of its payload files
  remain.
- **Host setup (A17; **Built**).** `hostkeeper-pxe serve --repositories` with a directory the daemon's loader accepts
  serves a seed whose user data writes each file byte for byte, root:root 0644, before `hostkeeper.toml`
  (`crates/hostkeeper-pxe/src/serve/tests/repositories.rs`, the golden `user-data-repositories.yaml` of
  `crates/hostkeeper-pxe-seed/tests/seed.rs`); a directory the loader refuses, a missing or empty one, and a keyring
  over the cap are refused at the start with the loader's reason, before any socket; the loader judges a source
  directory as it will be placed (`crates/hostkeeper-install/tests/repositories_placed.rs`). The network-install guest
  test serves with `--repositories` and checks the installed host's `/etc/hostkeeper/repositories/`
  (`crates/hostkeeper-privileged/tests/net_install.rs`, in `net-install.yml`). A Mac's daemon renders `stores.toml`
  and `macos-pkg.toml` from `dev.hostkeeper.stores` and `dev.hostkeeper.macos-pkg`, replacing a file that differs,
  keeping the operator's files without the domains, and refusing what its loaders refuse
  (`crates/hostkeeper-daemon/tests/mac_files.rs`); the gate's profile carries both domains, checked by the loaders
  at the coordinator's start, and is sent again once per change of its settings
  (`crates/hostkeeper-mdm-gate/tests/profile.rs`, `config_resend.rs`, `store.rs`); and the profile for a Mac renders
  to the signers and stores the `macos-pkg` installer's loaders read (`crates/hostkeeper-daemon/tests/mdm_profile_files.rs`).

- **One build, every release (A15; `crates/xtask/tests/release_linux.rs`, and the `privileged` device job).**
  `cargo xtask release check-linux-pkg` refuses a package whose binary has a `NEEDED` entry (a fixture binary linked to
  `libssl`), one that declares a dependency, and a tarball holding a glibc build, and accepts the static build. The device job installs the same `.deb` on
  the oldest and the newest Debian and Ubuntu releases the project lists and the same `.rpm` on the oldest and newest
  RHEL-family and openSUSE releases, each in a container, and starts the binary.
- **Tree and seal (A6; `crates/hostkeeper-unpack/tests/formats.rs` for the digest goldens,
  `crates/hostkeeper-steps/tests/artifact.rs` for the walk).** A tree with a link has a fixed digest, and a changed link
  target changes it; `the_tree_digest_has_a_fixed_value` holds unchanged for a tree without links; `Staged::check`
  accepts a link inside `tree/`, refuses one elsewhere, and refuses a hook path or `place` source through a link.
- **Built-in steps and uninstall (A6; `crates/hostkeeper-daemon/tests/builtin_steps.rs`, `uninstall.rs`).** `verify`
  passes with every unit active and health 2xx, defers on a unit inactive or a non-2xx, and ends in `rollback` at the
  deadline; `cutover` fails when a unit is not active within its bound; a service on built-in steps with no `uninstall`
  bound is uninstalled; one with a hook program and no bound is `uninstall_unsupported`; an uninstall stopped after
  `hook_done` resumes and runs `remove` again; the previous binary started on an `uninstalling_installed` tombstone
  leaves the package and the records. A heartbeat names each installed service's installer, and none for a service of
  `hostkeeper-archive`.
- **Rollback of hostkeeper (A6; `crates/hostkeeper-daemon/tests/selfupdate.rs`).** A rollback of `hostkeeper` to a
  release before an installer of a version the host holds is `rollback_below_installer`, and one to a release that has
  it goes on.

### The items of packages-native.md

Every test runs with the fakes of the general rules above; real `apt`, `dpkg`, `apk`, `installer` and `pkgutil` run
only in the device jobs, against a signed fixture repository and signed fixture packages on the runner's disk.

- **The hook, scripts and paths (A7, A9, A27; `crates/hostkeeper-install/tests/apt.rs`, `macos_pkg.rs`, `apk.rs`).** A
  package with `hostkeeper-hook` at its path runs it for each step with today's arguments, and one without it gets the
  built-in steps; a hook path that is a link, a directory or not executable is `manifest_invalid`; `hook.boot` with no
  hook in the package is `manifest_invalid` at `stage`; an entry with a non-empty `hook.command` is `manifest_invalid`.
  A `.deb` whose control archive holds each of `preinst`, `postinst`, `prerm`, `postrm`, `config` and `triggers`, one
  case each, a `.pkg` with a `Scripts` archive, and an `.apk` with a script, are `package_scripts` before anything is
  installed; a `.deb` with only `control`, `md5sums` and `conffiles` goes on. A file at each allowed path goes on, and
  one at each refused kind of path (an enablement link under `/etc/systemd/system/`, `/etc/cron.d/`,
  `/etc/sudoers.d/`, a drop-in directory, a link or a unit not named for the service in `/usr/lib/systemd/system/`) is
  `manifest_invalid`; a unit named for the service that is not in `run` goes on (packages-native.md section 10.2). Scripts in a package named `hostkeeper` for the
  service `hostkeeper` go on; the same package set for another service, and another package set for `hostkeeper`, are
  `package_scripts`. A `macos-pkg` order with an attestation is `order_invalid`, and one without is fetched and
  installed with no `gh` run (the command port's record is empty).
- **The coordinator (A18; `crates/hostkeeper-coordinator/tests/target_move.rs`, `crates/hostkeeper-release/tests/manifest/native.rs`).**
  The pick over a fleet of every kind of host: a host running the service from archives that lists `apt` is picked the
  `apt` entry wherever it stands in the list; one that lists no native installer of the list, and a daemon before
  `target-v1`, are picked the archive entry; a host running `apt` is never picked the archive entry; a list with no
  native entry its daemon lists leaves an archive host on the archive entry. A rollback of a host whose installer is
  native to a release with no native entry carries the archive entry, and one to a release with a native entry
  carries that. `hostkeeper`'s list: one native entry accepted, a native entry and one archive entry after it
  accepted, the archive entry first, two archive entries and two native entries each `target_invalid`. A `macos-pkg`
  entry with an attestation, and a native entry with a hook command, are `target_invalid`; a native entry with `boot`
  and an empty command is accepted. `Hosts.List` gives each host's services with their installer, for a host that
  heartbeats and for one that has not since the coordinator's start; `additions_hosts.rs` is the B8 test of the new
  field in both directions.
- **hostkeeper's own update (A19; `crates/hostkeeper-daemon/tests/selfupdate_native.rs`, device tests
  `self_update.rs` and `device_mac_selfupdate.rs` re-pointed).** The new binary is taken from the tree's packaged path,
  and a package without it is `self_binary_missing`; the package database is not touched before the new binary
  confirms; after the confirm `activate` runs once and the verdict follows it; a crash between the confirm and the end
  of `activate` is resumed by the next start, which runs it once; a deferred `activate` (`package_manager_busy`) stays
  pending and runs at the deferral's retry; a failed one is `Failed` with its class, the new binary keeps running,
  nothing is rolled back and drift is reported; a new binary that cannot start three times leaves the package
  database at the old version; an older reader reads a record with the pending field as having none. At start, a host
  whose package database lists `hostkeeper` at the running version and holds no archive version records it installed
  by its installer and holds it; one at another version, or one with an archive version of `hostkeeper`, records
  nothing. The device job upgrades the `.deb` of the release X1 pins to the new one while the daemon runs, and the
  daemon's process id is unchanged. A Mac's `installer` of hostkeeper's own `.pkg` with the job loaded leaves it running
  (same process id), and with no job loaded bootstraps it.
- **The move (A19; `crates/hostkeeper-daemon/tests/move_native.rs`, device test `move_native.rs` in `privileged`).**
  A service on archives with a placed unit moves to `apt`: the placed unit and files are gone before the package's unit
  is enabled, the package's unit is the one systemd runs, and the archive version is `previous`; `installer_changed`
  for `apt` to archive, for `apt` to `macos-pkg`, and for a native installer to another; the move's rollback before
  and after it settled purges the package, places the archive's paths again and records `hostkeeper-archive`, with no
  fetch (the fetch port's record is empty); after the next update's `retire`, a rollback to the archive version is
  refused. hostkeeper's move: the archive's package unit is gone from `/etc/systemd/system/`, the `.deb`'s unit is
  enabled, the running daemon is not restarted, and its next start runs `current` through `/usr/bin/hostkeeper`; a
  move whose new binary cannot start leaves the archive's unit and version in place. A rollback of `hostkeeper` to a
  release before A19 on a host where it moved is `rollback_below_installer`, and one to A19's release goes on. X1 runs
  the previous release's binary on a host holding a native version and the move's records: it reports the service as
  unreadable and leaves it (the B8 test of the old reader on new data).
  hostkeeper's own update through its `.deb`, the activate after the confirmation (its failure, its deferral and its
  retry, a crash between the two), the adoption of the bootstrap install, hostkeeper's own move and its rollback, and
  the refusal of a rollback below the compiled table before anything is staged are
  `crates/hostkeeper-daemon/tests/selfupdate_native.rs`; the table's rows are `src/rollback_floor_tests.rs`; the step
  record's pending activate and its B8 test against the previous release's reader are
  `crates/hostkeeper-steps/tests/selfupdate_activate.rs`; hostkeeper's own `.deb` and `.pkg` held to their own lists,
  with their scripts, are `crates/hostkeeper-install/tests/apt/own.rs` and `tests/macos_pkg/own.rs`, tied to
  `packaging/linux/nfpm.yaml` and the `.pkg` builder by `crates/xtask/tests/packaging/own_list.rs` and
  `src/release/macos_pkg/own_tests.rs`.
- **Packaging (A20; `crates/xtask/tests/packaging/`, `check-linux-pkg`, `check-macos-pkg`).** `hello`'s `.deb` and
  `.pkg`, its fail-ready build's, and `mdm`'s `.deb` carry the hook at its path, no script, files only at allowed paths
  and units installed disabled; `check-linux-pkg` refuses a fixture `.deb` with a `postinst`; the `.pkg`'s postinstall
  text is held byte for byte (`crates/hostkeeper-launchd/tests/postinstall.rs`, A19).
- **Release workflows (A21, A23, A24; the workflow lint, `crates/xtask/src/release/targets/tests.rs`).** `release
  targets` writes the native entry before the archive one for each platform (A21, A23) and the native entry alone
  (A24: the Linux `.deb`'s, no list for macOS while no daemon announces `macos-pkg`, and hello's failing build refused
  by name); the lint holds the publish steps to the release machinery's call (`crates/xtask/tests/workflows/packages.rs`:
  every job that signs or puts the release's files checks them against `SHA256SUMS` and their attestations first, A23)
  and, from A24, refuses the channel wait and any index output while it keeps the `targets` job that `set-desired`
  needs (`desired.rs`, `desired_tests.rs`, `tailnet_join_tests.rs`; the job run with bash in `targets_run_tests.rs`,
  hello's in `hello_run_tests.rs`, held to one input by `callers.rs`); X1's pin is a release from A19 on.
- **The runbook (A22; `docs/operations/move-to-native.md`, `crates/xtask/tests/enrollment_doc.rs` and
  `enrollment_doc/move_runbook.rs`).** Every `hkctl` command of the runbook is one of `hkctl`'s table, as for the
  enrollment runbook. Each query is one block of one `hkctl` call and one `jq` program reading the file the call
  wrote, and each program, as the runbook writes it, is run with jaq on `Hosts.List` and `Hosts.Get` answers
  serialized as the gateway serializes them: stage 9's two counts each count a host once, compare versions as
  numbers, count a host not known to be past them (no daemon known, an installer not known, nothing known of what it
  runs), refuse a floor that is not a version, and stop on a second page; each stage of 10.6 has its section with
  when it starts and its back-out.
- **`apk` (A27; `crates/hostkeeper-install/tests/apk.rs`, and `openrc_device.rs` and `selfupdate_openrc.rs` on an
  `.apk` fixture repository).** The cases its design (A26) names, one per check and failure row, as A7's for `apt`; the
  OpenRC device tests keep every case they have today.
- **Deletion (A25, A28).** The layering test without the deleted crates' rows; `the_protos_keep_every_rule_against_the_baseline`
  unchanged; a set with `package_digest` or `release_index` accepted and ordering nothing, every host
  `no_build_for_host`, and `Converge` not converged; a stored release with archive entries and an index loads, and its
  archive entries are never picked; a daemon from A25 started on a host with an archive version of a service it runs
  natively removes that version's directory and nothing else (the B8 test of the new reader on old data);
  `hostkeeper.toml` with `release_channel` and `[trusted_signers]` loads, and `hostkeeper-pxe` still checks a seed's
  attestation against them; the coverage gate at 99% on every crate that lost code.

This list is complete in this sense: it holds one case for every refusal, outcome, gate reason, alert and bound named
in sections 3 to 7 and 9.1 of packages.md (stages 5 and 6 of section 9.2 are proved by A11's and A12's own tests,
named in the build table's last column), one under and one over every limit, each step of the coordinator's pick of
section 3.3 (first listed, kept installer, no match, old daemon), each row of the announce table of section 5.4, and
every installer's four steps against crash, repeat, failure and removal. For a repository installer it also holds
one case for every check of packages-repositories.md section 2.2, every refusal of its section 3.1, and every row of
its section 4.5, which are all the outcomes that file names. For packages-native.md it holds one case for every check
of its section 10.2's list and path rule, each new refusal (`package_scripts`, the move's `installer_changed`, the
rollback below A19), each row of the pick with the move's exception, each step of hostkeeper's own update against
crash, deferral, failure and repeat, the B8 tests in both directions for each format the change touches, and each
stage's condition of 10.6. Each item's pull request names its tests (rule T8).
