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

Nothing privileged and nothing for macOS runs on a pull request. The workflow lint refuses such a
job in any workflow that runs on `pull_request`, refuses a job as root (`sudo`, a privileged
container) in any workflow but `privileged.yml` (below), and it also refuses an action that is not pinned
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
(`workflow_dispatch`), never on a pull request, so a pull request cannot show these tests passing:
the first run on `main` after the merge is their proof. A release waits for a successful run of it
on its commit. A red run on `main` is a defect: fix it forward in a new pull request.

The tests, in `crates/hostkeeper-privileged/tests/`, drive the production code:

- `transient_unit.rs`, the step launcher (`SystemdLauncher`) and the dead-man:
  `a_step_runs_in_a_real_transient_unit_and_is_ended_at_its_bound`,
  `the_transient_unit_carries_runtime_max_sec_and_nothing_else`,
  `an_unbounded_prepare_gets_runtime_max_sec_infinity`, `the_dead_man_stops_a_unit_by_name`,
  `a_unit_name_is_reusable_after_it_failed` and
  `a_long_output_is_kept_to_its_last_bytes_with_the_last_line`;
- `observer.rs`, the observer's `systemctl show` and `/proc` adapters:
  `the_observer_reads_active_state_and_the_main_pid_digest_of_a_real_unit`.

Each of these files starts with `#![cfg(feature = "privileged")]`. The feature is off by default,
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
it ends, failed or not; `journalctl --unit 'hostkeeper-ci-*'` shows what they logged.

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

## Small pull requests

Keep pull requests small and about one thing, so a reviewer can read the whole diff in one
sitting (rules P7 and R12 of [AGENTS.md](AGENTS.md)).

## Formats change additively

Every format hostkeeper writes and another release of it reads changes additively only (rule B8
of [AGENTS.md](AGENTS.md)). A host may go back to the release before, so each release must start
on data the other wrote.
