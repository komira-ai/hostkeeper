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

To see the coverage table locally, with the nightly named in `ci.yml` and `cargo-llvm-cov`
installed:

```sh
cargo +nightly-2026-10-01 llvm-cov --workspace --locked --branch --lcov --output-path target/lcov.info
cargo run -p coverage-gate --locked -- --lcov target/lcov.info --root .
```

Nothing privileged and nothing for macOS runs on a pull request. The workflow lint refuses such a
job in any workflow that runs on `pull_request`, and it also refuses an action that is not pinned
by commit SHA, a runner that is not GitHub-hosted, a workflow without a top-level
`permissions: {}`, and `pull_request_target`.

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
