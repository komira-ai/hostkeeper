# Contributing to hostkeeper

Thank you for helping. The rules every change is written and reviewed against are in
[AGENTS.md](AGENTS.md), cited by id (T1, B8, ...). This file says how to meet them here: signing
commits, building and testing, and what CI checks on a pull request.

## Developer Certificate of Origin (DCO)

Every commit must carry a `Signed-off-by` line. By adding it you certify the
[Developer Certificate of Origin](https://developercertificate.org): that you wrote the change,
or otherwise have the right to submit it under the project's licence ([Apache-2.0](LICENSE)).

Git adds the line for you with `-s`:

```sh
git commit -s -m "coverage-gate: name the crate a stray source belongs to"
```

which produces:

```
coverage-gate: name the crate a stray source belongs to

Signed-off-by: Your Name <you@example.com>
```

The name and email must match the commit author (rule P4 of [AGENTS.md](AGENTS.md)). Until an
automated DCO check is set up, reviewers check the trailer by hand.

## Building and testing

hostkeeper is a Cargo workspace; the crates live under `crates/`. Every build and test uses
`cargo`, with the toolchain pinned in `rust-toolchain.toml`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

There is no other build system in this repository. Tests are hermetic: they need no network,
no fixed port and no root, and they write only under Cargo's target directory.

## What CI checks on a pull request

`.github/workflows/ci.yml` runs two jobs on GitHub-hosted runners:

- `lint`: the format, `cargo deny check bans licenses sources` (the licence allow-list and the
  sources are in `deny.toml`), clippy with warnings as errors (the banned methods are in
  `clippy.toml`), the build of `hostkeeper-hook` alone without its `conformance` feature
  (`cargo check -p hostkeeper-hook --lib`), actionlint, and the workflow lint
  (`cargo test -p xtask --test workflows`).
- `test`: every test under `cargo llvm-cov` with branch coverage, then `coverage-gate`, which
  fails when any crate of the workspace is under 99.00% of its lines or of its branches. Every
  crate is judged; there is no way to exclude one (rules T1 to T4).

Security advisories are not checked on a pull request: a new advisory against a crate already in
`Cargo.lock` would fail every open pull request for something none of them did. Instead
`.github/workflows/advisories.yml` runs `cargo xtask advisories` daily, on every push to `main`
and by hand. It runs `cargo deny check advisories` and keeps what it finds (advisories and yanked
crates) in one issue, "Security advisories in hostkeeper's dependencies": it opens the issue,
rewrites its table when the findings change, and comments and closes it when none remain. The
workflow lint holds that workflow to those triggers, to `issues: write` on its one job of `main`
only, and to the cargo-deny pin of `ci.yml`.

To see the coverage table locally, with the nightly named in `ci.yml` and `cargo-llvm-cov`
installed:

```sh
cargo +nightly-2026-10-01 llvm-cov --workspace --locked --branch --lcov --output-path target/lcov.info
cargo run -p coverage-gate --locked -- --lcov target/lcov.info --root .
```

Nothing privileged and nothing for macOS runs on a pull request, but in `pr-device.yml` (below),
on every pull request. The workflow lint refuses such a
job in any other workflow that runs on `pull_request`, refuses a job as root (`sudo`, a privileged
container) in any workflow but `privileged.yml`, `net-install.yml` and the two copies of
`privileged.yml`'s jobs in `pr-device.yml`,
refuses a write scope, a secret or the token in every workflow that runs on `pull_request`, and it
also refuses an action that is not pinned
by commit SHA, a runner that is not GitHub-hosted, a workflow without a top-level
`permissions: {}`, `pull_request_target` and `secrets: inherit`. It also holds
`set-desired.yml`, the CI client services call ([docs/deploy/ci-client.md](docs/deploy/ci-client.md)),
to the order of its steps: the release is verified before any step can request an OIDC token or
run the hostkeeper program. Besides `artifacts.yml`'s `attest`, which signs, that job is the only
one that may hold `id-token: write`.

## The privileged workflow: real systemd, after a merge

Everything a pull request runs proves hostkeeper against fakes of the host.
`.github/workflows/privileged.yml` is the one place where the adapters behind those fakes meet the
real thing: it runs the tests of `crates/hostkeeper-privileged` as root against the systemd of a
GitHub-hosted `ubuntu-24.04` runner. It runs after a merge to `main` and by hand
(`workflow_dispatch`), never on a pull request. A release waits for a successful run of it on its
commit. A red run on `main` is a defect: fix it forward in a new pull request.

Network install end to end (below) runs as root too, in `.github/workflows/net-install.yml`, on its
own triggers, and no release waits for it: an emulated install takes hours. It runs on a push to
`main` that changes the network-install code (the paths the workflow lint lists:
`crates/hostkeeper-pxe*`, `packaging/netboot`, `packaging/linux`, the network-install tests and
their rig, the generators of `netboot.json` and of the report, and the workflow itself), nightly
on `main` whatever changed, and by hand (`workflow_dispatch`), on `main` or on a pull request's
branch (`gh workflow run net-install.yml --ref <branch>`); never on a pull request. A newer run of
the same branch cancels the older, so on `main` only the newest commit's run is kept. A red run on
`main` is a defect, as above.

## The macOS and privileged jobs of a pull request

`.github/workflows/pr-device.yml` runs on every pull request, beside `ci.yml`. Its three jobs are
each a check of its own, required of every pull request:

- `macos-test`: the tests of the crates the `hostkeeper` and `hello` binaries are built from (the
  code a Mac runs: the daemon, the launchd adapters, the step launcher, placement, the package
  check, the observer, the enrollment claims and what they use) on a GitHub-hosted `macos-15`
  runner, as the runner's user. The rest of the workspace (the coordinator's MDM and
  network-install servers, the release tooling) runs on Linux only and does not build for macOS;
  the workflow lint holds the list to those binaries' path dependencies, so a crate added to
  either joins it.
- `macos-device`: the device tests as root under launchd.
- `privileged`: the privileged tests as root against systemd.

`macos-device` and `privileged` are copies of `privileged.yml`'s jobs of those names, key for key
(the workflow lint compares them), so a pull request runs what will run after its merge. A newer
push to the same pull request cancels the older run. These jobs hold no write scope and name no
secret or token, and save no cache.

The tests, in `crates/hostkeeper-privileged/tests/`, drive the production code:

- `transient_unit.rs`, the step launcher (`SystemdLauncher`) and the dead-man:
  `a_step_runs_in_a_real_transient_unit_and_is_ended_at_its_bound`,
  `the_transient_unit_carries_runtime_max_sec_and_nothing_else`,
  `an_unbounded_prepare_gets_runtime_max_sec_infinity`, `the_dead_man_stops_a_unit_by_name`,
  `a_unit_name_is_reusable_after_it_failed` and
  `a_long_output_is_kept_to_its_last_bytes_with_the_last_line`;
- `observer.rs`, the observer's `systemctl show` and `/proc` adapters:
  `the_observer_reads_active_state_and_the_main_pid_digest_of_a_real_unit`;
- `unit_control.rs`, the Linux unit control's `systemctl`:
  `the_linux_unit_control_reloads_starts_and_stops_a_real_unit`.

In the job `first-boot`, the programs themselves: the tests start the coordinator and the daemon
as child processes on loopback and drive them only through their flags, files and `hostkeeper
ctl` (`tests/support/pair.rs`):

- `first_boot.rs`: `a_daemon_enrolls_beats_and_keeps_its_identity_across_both_restarts`
  (enrollment, approval, the host certificate, heartbeats over mutual TLS, a restart of the
  coordinator and of the daemon), and `a_short_lived_certificate_is_renewed_and_the_heartbeat_goes_on`,
  ignored until the coordinator has a flag for the host certificates' validity
  (komira-ai/hostkeeper#100);
- `install_uninstall.rs`, a package served by a release channel of the test over HTTPS:
  `an_assigned_package_installs_once_across_restarts_and_the_boot_pass_keeps_it`,
  `a_new_desired_version_is_installed_over_the_installed_one` and
  `an_uninstall_takes_the_package_its_file_and_its_unit_off_the_host`.

The daemon fixes paths on the host that these tests cannot move. Both tests claim its pki
directory `/etc/updater`, its self-update units and `/var/lib/hostkeeper`; `install_uninstall.rs`
also claims the verifier's directory `/usr/lib/hostkeeper` and the fixture service's unit. A test
refuses to start if the host already has a path it claims, and removes them when it ends.
`install_uninstall.rs` appends the release channel's CA to the host's CA bundle and writes the
bundle back as it was when it ends. The verifier is the one stand-in:
`/usr/bin/true` in place of `gh`, since an attestation of a package the test builds cannot be made
without the network.

In the jobs `net-install` and `net-install-arm64` of `net-install.yml`, network install of a new machine end to end
(docs/design/network-install.md section 11): a QEMU guest with UEFI firmware, an empty disk and
its NIC first in the boot order, on a Linux bridge with no route out, each box of the LAN (the
router, whose DHCP the test fakes, and the real `hostkeeper-pxe`) in a network namespace of its
own, with the real coordinator's LAN listener (`tests/netinstall/`). The job installs QEMU and the
firmware from Ubuntu's archive (its only other use of `sudo`, which the workflow lint allows in
these two jobs for the packages it lists), downloads the ISO by its pinned URL, and, for x86-64,
builds this commit's deb; `hostkeeper-pxe fetch` checks and seals the boot set from a release the
test serves and signs. Every wait is on a line of the guest's serial console, of
`hostkeeper-pxe`'s log or of the router's DHCP, or on an answer of the coordinator to the test's own call; the job's
`timeout-minutes` is the only bound. What each run kept, passed or not, is the job's artifact:

- `net_install.rs` (under KVM):
  `an_x86_64_machine_installs_by_network_enrolls_gets_its_service_is_reinstalled_and_retired`
  (proxy mode; enrollment by the console's key hash; the fixture service; router mode for the
  enrolled host; a reinstall with `--reinstall` and `--replace`; a retired host after a coordinator
  restart, in both modes);
- `net_install_arm64.rs` (under TCG, as the hosted arm64 runner has no KVM): `arm64_boots_to_seed_fetch`.

The one stand-in is on the installed guest: before its approval the test gives it the release
channel's CA and the trusted root of the test's signer, by its operator account over SSH, so that
its own `gh` checks the fixture package's attestation. `tests/net_install_rig.rs` runs the parts
of the rig that need no root (the router's DHCP answers, the console's lines) in the pull-request
set.

The files of the job `privileged` start with `#![cfg(feature = "privileged")]`, those of
`first-boot` with `#![cfg(all(target_os = "linux", feature = "privileged"))]`, `net_install.rs` with
`#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "privileged"))]` and
`net_install_arm64.rs` with `#![cfg(all(target_os = "linux", feature = "privileged",
not(target_arch = "aarch64")))]`. The feature is off by default,
so `cargo test --workspace` and CI's `test` job compile none of them, and they count toward no
coverage gate; the crate's library (the workload fixture `hostkeeper-fixture-hook`, the host
check, the rig) is tested without root in the pull-request set like any crate. A privileged test
refuses to start unless it runs as root on a host whose service manager is systemd.

To run one yourself, use a throwaway VM with systemd, never a workstation: the tests start, stop
and link units of the host's systemd. The workflow's two steps are the way: build as your own
user, then run as root through the cargo runner (no `sudo -E`, no build as root):

```sh
cargo test -p hostkeeper-privileged --features privileged --locked --no-run --test transient_unit
cargo test -p hostkeeper-privileged --features privileged --locked --config "target.'cfg(unix)'.runner = 'sudo'" --test transient_unit -- --test-threads=1 the_dead_man_stops_a_unit_by_name
```

Each test names its units `hostkeeper-ci-<test>-<pid>-<n>` and stops, unlinks and resets them when
it ends, failed or not; `journalctl --unit 'hostkeeper-ci-*'` shows what they logged. The
first-boot tests also add a CA to the host's CA bundle and write under `/etc`, `/usr/lib` and
`/var/lib`, and the network-install tests make network namespaces, a bridge, veth pairs and a tap
on the host and run QEMU: run them only on a throwaway VM, never on a host that runs hostkeeper.

### The tierstore-shaped end-to-end tests

`tierstore_shaped.rs` runs the whole update path with nothing faked: the real coordinator (in the
test process) and the real `hostkeeper daemon` (under a `Type=notify` unit, started with
`systemctl`) update a package shaped like tierstore, two instantiated run units whose hook drains
in `prepare` and starts the new release beside the old in `cutover`. The package is fetched from
a release channel the test serves over HTTPS on loopback, its attestation is checked by the pinned
`gh` (the job's step `Install gh` puts it at `target/privileged-tools/gh`) against a trust root
the test makes, and it is unpacked, sealed and switched by the daemon; the test sets the
desired version and reads the coordinator's answer through the generated tonic clients
(`hostkeeper-testkit`'s `operator` helper). The tests:

- `a_tierstore_shaped_package_updates_end_to_end`: from `1.0.0` to `1.1.0`, the old release
  serving while `prepare` drains, then the new one serving and the host converged;
- `a_failed_verify_rolls_back_and_the_old_release_serves`: `1.2.0`, whose `verify` fails, is
  rolled back by its hook, the old release serves again and the coordinator marks `1.2.0` bad;
- `force_back_to_previous_needs_no_fetch`: a force to the previous version with the release
  channel stopped fetches nothing, and exactly two versions stay on disk;
- `the_daemon_is_ready_under_a_notify_unit_only_after_its_boot_pass`: `systemctl start` of the
  daemon returns after its boot pass relinked a placed path;
- `fixture_attestation_is_checked_by_the_production_verifier`: the fixture's attestation passes
  the production fetch and proof with the pinned `gh`, and another digest or signer does not.

These tests use the host's fixed paths the daemon uses (`/usr/lib/hostkeeper`, `/etc/updater`,
`/var/lib/hostkeeper`, the CA bundle and the unit directory): what is already there, left by
another test, is moved aside and put back, and everything the test wrote is removed or restored
when it ends, failed or not; a failed test
first copies its hook records, step record and journal to `target/privileged-artifacts/`, which the
workflow uploads. `tierstore_rig.rs` checks the fixture package against the production unpacker
and the release channel's log without root, in the pull-request set.

The fixture package is source: `crates/hostkeeper-privileged/fixtures/tierstore-shaped/` (its
manifest and unit template) and the program `tests/bin/tierstore_like.rs`, built into a
`.tar.zst` at test time by `PackageBuilder` (`tests/e2e/package.rs`). To add a fixture version,
add a `FixtureVersion` to `Scenario::start` (`tests/e2e/scenario.rs`): its version, two ports
(chosen at run time) and whether its `verify` fails; the scenario builds, signs and publishes it
with the others. Nothing binary is committed.

### The cross-release rig

`self_update_across_releases.rs` runs hostkeeper's update of itself from a **published release** to
this build, on Linux in the job `privileged` and on a Mac in the job `macos-device` (one target in
both: its first line names both platforms). The release is the one
`crates/hostkeeper-privileged/previous-release.toml` pins: its tag and version, and for each
release platform the URL of its binary tarball (`hostkeeper-<version>-<platform>.tar.gz`) and that
tarball's SHA-256. The jobs' steps `Download the previous release` and `Check the previous
release (pinned checksum)` fetch the URL with `curl` and check it with `cargo xtask install-tool`
into `target/previous-release/hostkeeper.tar.gz`; the workflow lint holds each job's
`PREVIOUS_RELEASE_URL` and `PREVIOUS_RELEASE_SHA256` to the pin. The test checks the file against
the pin again and fails by name when it is missing or differs; it fetches nothing itself.

The released daemon runs as the host's own daemon (`hostkeeper.service` where the deb installs
it, or the launchd job `dev.hostkeeper.hostkeeper` where the pkg puts it), on the end-to-end rig:
the real coordinator ordering hostkeeper, the loopback release channel, the pinned `gh` against
the test's trust root, which the released daemon reads from the host's fixed path as this build
does. This build is the program `hostkeeper-named-daemon` (the shipped daemon, its hand-over
acted on first, its version the name of the file it runs from, so it runs as versions newer than
the release) and `hostkeeper-failing-daemon` (whose `daemon` exits 1). The pin is release R1,
v0.1.163, the last release that updates itself the old way; the tests, each from the released daemon
converged on its own version's package, with its fallback unit and drop-in (or job) armed:

- `the_released_daemon_updates_to_this_build_and_this_build_settles_it_succeeded`: the released
  daemon updates the old way, and this build removes the released daemon's fallback files at its
  start;
- `this_build_made_to_fail_its_start_is_fallen_back_from_to_the_released_binary`: by the released
  binary's own fallback unit or job, the update `Failed`, `self_update_fallback`, reported to the
  coordinator;
- `this_build_updates_to_itself_under_a_newer_version_name`: by the hand-over.

To move the pin, in the pull request that needs it: set `tag` and `version`, and each platform's
`url` and `sha256` from the release's `SHA256SUMS`, in `previous-release.toml`, and the same URL
and SHA-256 in the `env` of the jobs `privileged` and `macos-device` of both `privileged.yml` and
`pr-device.yml`. `cargo test -p hostkeeper-privileged --lib across` checks the pin's shape and
`cargo test -p xtask --test workflows` checks the workflows against it.

## Tests

Tests assert behaviour, cover failure and boundary cases, and are hermetic. A bug fix ships with
a test that failed before the fix. A new lint or check comes with a case it refuses. The rules
are T1 to T19 in [AGENTS.md](AGENTS.md).

Nothing the updater decides reads the wall clock: decisions read `hostkeeper_steps::Clock`
(monotonic seconds and the boot id). `cargo test -p xtask --test wallclock` refuses a wall-clock
read (`SystemTime::now()` and its equivalents, a realtime clock id) in any non-test source under
`crates/*/src/`, and a rename of a wall-clock name (`use .. as ..`, `type .. = ..;`), except in a
path listed, with its reason, in `crates/xtask/tests/wallclock/allowed.txt`: certificate, OIDC or
enrollment-nonce validity, and the time stamp of the coordinator's audit file, which no decision
reads.

A command an operator doc gives is an `hkctl` command ([docs/cli.md](docs/cli.md)), written out
in full in a fenced block: `hkctl --endpoint https://<private-listener> --ca-file <ca.pem>
--token-file <token-file> <service> <method> [flags]`. `cargo test -p xtask --test enrollment_doc`
reads each such line of every Markdown file but the design documents (`docs/design/`) as hkctl
reads it, against its command table and global flags, and the method and flags an inline
`` `hkctl ...` `` span names; it refuses a fenced `hostkeeper ctl` line.

## Small pull requests

Keep pull requests small and about one thing, so a reviewer can read the whole diff in one
sitting (rules P7 and R12 of [AGENTS.md](AGENTS.md)).

## Formats change additively

Every format hostkeeper writes and another release of it reads changes additively only (rule B8
of [AGENTS.md](AGENTS.md)). A host may go back to the release before, so each release must start
on data the other wrote.
