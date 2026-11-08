# Packages in any format: the tests of each build item

Status: design, for the maintainers' ruling. The tests of A1 are **Built** (`crates/hostkeeper-release/tests/manifest.rs`,
`crates/hostkeeper-proto/tests/additions_targets.rs`), and so are those of A2
(`crates/hostkeeper-coordinator/tests/targets.rs`, `crates/hostkeeper-proto/tests/additions_releases.rs`, and the
Postgres store's in `crates/hostkeeper-coordinator/src/store/postgres/tests/releases.rs`); nothing else here is built. This is section 12 of
[packages.md](packages.md), kept in a file of its own so that one stays under 1000 lines (rule R12). Section numbers here
are those of packages.md, and A1 to A15 are its build items (section 13).

Each item's tests are behaviour tests in Rust, in the crate the item changes (rules T8, T11, T12). Each runs with no
network and no system path, through the fakes the crates already have (`Fetcher`, `Verifier`, the coordinator's backend
fake, the step launcher's fake, the placer's roots under a scratch directory), and through a command port for the
formats' tools. A host's `os-release` and the tools' paths are read under a scratch root the test makes. None asserts a
time: a bound is tested on an injected clock (rule T10). Real `dpkg`, `rpm`, `apk`, `pacman`, `installer` and `podman`
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
  three platforms, hosts announcing `deb`, `rpm`, only `tarball`, and nothing): a host of a platform with no list is
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
- **Which installers a daemon announces (A6 for `hostkeeper-archive` and `tarball`, A7 to A10, A13, A14 for theirs;
  `crates/hostkeeper-install/tests/announce.rs`).** One case per row of section 5.4's table, with `ID` alone and with
  the family only in `ID_LIKE` (`linuxmint`, `rocky`, `amzn`, `opensuse-leap`, `manjaro`); an `ID` that maps to one
  family ahead of an `ID_LIKE` token of another; a Debian host with `/usr/bin/rpm` present announces `deb` and not
  `rpm`; a Debian host without `dpkg` announces no native format; quoted and unquoted values give the same family;
  `/usr/lib/os-release` is read when `/etc/os-release` is missing; an unknown `ID`, an empty file, both files missing
  and a malformed line (no `=`, an unterminated quote) give no native format, so a Linux host announces
  `hostkeeper-archive` and `tarball` alone; `oci-image` is announced with `/usr/bin/podman` present and not without it
  (A10); a macOS host announces `hostkeeper-archive`, `tarball` and `macos-pkg` and never reads `os-release`. The list is the same at every heartbeat of one start.
- **Installers (A6 to A10, A13, A14; `crates/hostkeeper-install/tests/<installer>.rs`, device tests in
  `crates/hostkeeper-privileged/tests/install_<installer>.rs` and the macOS device suite).** For each installer:
  `stage` refuses a package whose identity is not `package` (and an OCI archive of two images), and runs no script
  (the fake tool records its calls). For `deb`, `rpm` and `arch` (and `apk` in A13), the dependency check of section
  5.4 runs after the identity check and before `prepare`; when the tool's no-change mode refuses, the outcome is `dependency_missing`, the hook never runs and
  nothing is installed. The walk after `stage` refuses an entry under a link, an absolute or escaping link, a hard
  link, a device, one entry over `max_entries` and one byte over `max_unpacked_bytes`; it makes a non-root owner root,
  strips a setuid bit and sets modes; on macOS an unsigned Mach-O file is refused. Every tool run is in a unit with
  `PrivateNetwork=yes` (the fake launcher records the unit's properties). `activate` twice is the same as once, and a
  crash between the tool and the switch resumes to the same state. An `activate` that fails rolls back to `previous`,
  and a first install that fails is removed. A tool past its bound is stopped and the step fails `environment`. Two
  services' `activate` never run the tool at once. Rollback installs the kept previous artifact with no fetch; for
  `macos-pkg`, a file only the newer receipt lists is deleted and a changed one left. `retire` of version N-2 after
  N-1 and N leaves every installed file of N untouched and never calls the tool. `remove` removes what `activate`
  recorded, and leaves and reports a file changed since (`macos-pkg`), a package upgraded by hand (`deb`, `rpm`, `apk`,
  `arch`), or an image another kept version uses or the runtime held before (`oci-image`). Then `package_not_ours`,
  `package_conflict`, `installer_changed`, and `target_changed` for a sealed version ordered again with another
  manifest. The generated container unit for each combination of `args`, ports (loopback or not) and volumes
  (read-only or not) equals its golden text. The observer hashes the program a native unit names in `tree/` and
  reports `version_mismatch` for a process of another version, and the image digest for `oci-image`. The device tests
  install, update, roll back and uninstall a two-version `hello` in each format on the hosted runner (`rpm`, `apk` and
  `arch` in a container of their family), assert that nothing the fixture package's installer put there remains after
  uninstall, and that a `.deb` and an `.rpm` declaring a dependency the host lacks are refused `dependency_missing`
  with nothing installed.
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

This list is complete in this sense: it holds one case for every refusal, outcome, gate reason, alert and bound named
in sections 3 to 7 and 9.1 of packages.md (stages 5 and 6 of section 9.2 are proved by A11's and A12's own tests,
named in the build table's last column), one under and one over every limit, each step of the coordinator's pick of
section 3.3 (first listed, kept installer, no match, old daemon), each row of the announce table of section 5.4, and
every installer's four steps against crash, repeat, failure and removal. Each item's pull request names its tests
(rule T8).
