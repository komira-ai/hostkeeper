# The farm lane: the workspace with Buck2 on the build farm

Cargo is hostkeeper's build of record. CI's gate (`ci.yml`), the coverage gate and every release
use it, on GitHub-hosted runners, and that does not change. The farm lane builds the same
workspace a second way: with Buck2, through [buildrules](https://github.com/komira-ai/buildrules)'
Rust rules, every action on a remote-execution build farm or taken from its cache. It is
additive and advisory: `.github/workflows/farm.yml` runs it on every push to `main` and every pull
request from a branch of this repository, and no check of it is required.

## Status

`.buckconfig` pins buildrules at its main (7dfc5c2), which has the rustfmt check, the Postgres
fixture for tests and the build-commit stamp this lane uses. It cannot yet analyse this workspace:
two gaps of its Rust rules, komira-ai/buildrules#160 items 1 and 2, fail every target at load.
Until a buildrules commit has them, `farm.yml` is red at that step. That changes nothing for a pull
request, which ci.yml gates. The numbers below were measured with those two changes applied. Each
other item of that issue is met here by declaring the file in `extra_srcs`, or, for `CARGO`, by
reading it with `option_env!` in the two tests that run cargo.

## What it builds

`buck2 build //:ci`: for every member of the workspace, its `:fmt` (the pinned `rustfmt --check`
over the files `cargo fmt` reads, with `rustfmt.toml`), its `:lint` (clippy over the library, the
binaries and every test target, with `[workspace.lints]` and `clippy.toml`, warnings as errors)
and, for the members listed below as farm-tested, its `:test`. A test target runs as a build
action: its `.passed` marker exists only if every test of the target passed, and a cached marker
means the tests passed on byte-identical inputs. The summary line must read
`Commands: N (cached: N, remote: N, local: 0)`: no action ran on the runner. `farm.yml` runs the build
as one command, `cargo xtask farm-build` (`crates/xtask/src/farm.rs`): it refuses to start
without the farm's worker property set, fails a build that does not say `local: 0`, and stops
buck2's daemon however the build ended.

The files of the build, all generated or written once by `buildrules`:

| file | what | written by |
|---|---|---|
| `.buckconfig` | the cells, buildrules pinned by commit, the execution platform | `buildrules init`, then by hand (the worker passthrough below) |
| `toolchains/`, `platforms/` | the pinned Rust toolchain and the remote execution platform | `buildrules init` |
| `BUCK`, `crates/*/BUCK`, `examples/hello/BUCK` | `cargo_workspace` and one `cargo_package` per member | `buildrules init` and `buildrules cargo adopt`, once |
| `buildrules.lock.bzl`, `buildrules.bzl`, `third_party/buildrules/` | Cargo's unit graph of every member, and every crates.io crate as a unit pinned by SHA-256 and size | `buildrules sync`; never edited by hand |

`Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml` stay the source: `//:check` fails every
target when the generated table no longer matches them, and prints `buildrules sync` as the fix.
After a change to a manifest or the lock, run `buildrules sync` and commit what it wrote.

## What stays in ci.yml, and why

`//:ci` runs the tests of 28 of the 33 members: `coverage-gate`, `hostkeeper`,
`hostkeeper-bootstrap`, `-config`, `-console`, `-daemon`, `-enroll`, `-enrollment`, `-install`,
`-launchd`, `-link`, `-measure`, `-observe`, `-oidc`, `-openrc`, `-package`, `-placed`, `-proto`,
`-pxe`, `-pxe-packets`, `-pxe-seed`, `-release`, `-steps`, `-testkit`, `-tls`, `-ui-tests`,
`-wire` and `examples/hello`. A farm action has its declared inputs and nothing else: no network,
no `PATH` but buildrules' busybox, no git checkout, not root. The other five members are
format-checked, built and linted there, and their tests run in ci.yml (or as root in
privileged.yml and pr-device.yml) alone; each says so in its
`[package.metadata.buildrules.actions]` (`ci = ["fmt", "lint"]`):

| member | why its tests stay hosted |
|---|---|
| `hostkeeper-coordinator` | buildrules gives each of its test targets a private Postgres (`[package.metadata.buildrules.postgres]`, the URL in `HOSTKEEPER_TEST_POSTGRES_URL`), but that server runs with `fsync` off, which the coordinator's start refuses by design: every test that starts it fails on the farm (komira-ai/buildrules#166) |
| `hostkeeper-hk` | its ssh tests run `ssh`, `ssh-agent`, `ssh-add` and `ssh-keygen` |
| `hostkeeper-hook` | its outside-consumer test builds a crate with cargo against crates.io |
| `hostkeeper-privileged` | its tests run as root against systemd, OpenRC, Docker and apt |
| `xtask` | its tests read the git checkout (workflows, `AGENTS.md`, every `Cargo.toml`) and run git, cargo, bash and jq |

What else only ci.yml measures, for every member:

- Coverage. The 99% line and branch gate is cargo-llvm-cov's; buildrules has no Rust coverage
  yet.
- Doctests. buildrules does not run them.
- Feature unification across the workspace. ci.yml builds the workspace at once, so a feature one
  member turns on in another is on for all; buildrules builds each member's tests with that
  member's own dependency graph (its dev-dependencies included). A test behind such a feature is
  compiled out on the farm, not failed.

A file a crate reads beyond its unit's own sources is declared in its BUCK file (`extra_srcs`): a
library sees `src/` and `build.rs`, a test target `tests/`, a build script `build.rs` alone. So a
library that includes a test fixture, a test that includes a library file, a build script that
compiles `proto/`, a `[[bin]]` under `tests/bin`, and a test that reads `docs/`, `packaging/` or
another member's fixtures each name the file or directory. A file read by another member is
exported by reference in its owner's BUCK file (or the root `BUCK`), so it stays at its path.

The coordinator's `build.rs` stamps the commit it reads with `git` (`HOSTKEEPER_BUILD_COMMIT`),
and a farm action has no `git`. Its BUCK file gives that value as an input instead, the file
`crates/hostkeeper-coordinator/build_commit.txt` (buildrules' `stamp_rustc_env`). The committed
text is `unknown`, what `build.rs` says for a source archive, so the coordinator's units keep one
cache key from commit to commit. A build that wants the commit in `--version` writes its 12 hex
digits there first. Releases are cargo's and stamp `RELEASE_COMMIT` as before.

## Running it yourself

You need buck2 (the release in buildrules' `tools/buck2`) and a `.buckconfig.local` (gitignored)
that names the farm: `[buck2_re_client]` with its endpoints and instance, and
`[buildrules_re] linux_x86_64_properties` with its worker property set. This repository holds
neither. Then:

```sh
buck2 build //crates/hostkeeper-wire:test     # one crate's tests
buck2 build //:ci                             # what farm.yml builds
buck2 log what-failed                         # the failing actions and their commands
```

Read the summary line: `local` must be 0.

## How the job reaches the farm

The farm is on a private network. komira's farm-connect action, pinned by commit, joins it with
the job's OIDC token (workload identity federation: no secret is stored), refuses to
go on unless the farm answers (it masks the address in the log), and writes the farm's buckconfig under `~/.buckconfig.d`, where buck2
reads it. Its inputs are the organisation's variables `TS_CLIENT_ID` and `TS_AUDIENCE` (the tailnet
identity), `FARM_ADDRESS`, `FARM_INSTANCE`, `FARM_LINUX_X86_64_PROPERTIES` and `FARM_TLS`.
farm-connect writes the worker property set under `[komira_re]`; `.buckconfig` hands it to
buildrules (`[buildrules_re] linux_x86_64_properties = $(config komira_re.linux_x86_64_properties)`)
and holds no value of its own.

That join is why the job holds `id-token: write` on a pull request, the one job of a pull-request
workflow that may (AGENTS.md, B10 of this repository). The workflow lint
(`crates/xtask/tests/workflows/farm.rs`) holds it to: exactly that scope and `contents: read`;
the exact `if:` that skips a fork's pull request (which gets no OIDC token) and Dependabot's; farm-connect's inputs exactly
the variables above; a step after farm-connect's that runs `cargo xtask farm-build` of `//:ci`; and the triggers, one
job and one runner. It does not fix the job's other steps, each of which could ask for the token:
a change to them is reviewed like any change to a workflow.

A pull request's build and a build of `main` share the farm's action cache, as every build of
every repository on the farm does: a result one writes, the other reads. The farm's cache is
keyed by the action's digest, so a result is reused only for byte-identical inputs and command.

## Measured

The first measurements of the lane, of `//:ci` as it is here, with buildrules plus the two fixes
its `cargo.bzl` needs for this workspace (komira-ai/buildrules#160, items 1 and 2). Wall times are buck2's, on a machine that ran no action itself.

| build | `Commands:` line | wall |
|---|---|---|
| cold `//:ci` after every first-party unit changed (third-party crates already in the farm's cache) | `474 (cached: 0, remote: 474, local: 0)` | 655 s |
| the same, again, with buck2's daemon restarted (nothing in its memory) | `3356 (cached: 3356, remote: 0, local: 0)` | 4.2 s |
| the same, again, daemon kept | no action looked up | 0.07 s |
| one-line change to `hostkeeper-wire`'s library, `buck2 build //crates/hostkeeper-wire:test` | `2 (cached: 0, remote: 2, local: 0)` | 40.6 s, 45.0 s |
| the change reverted | `2 (cached: 2, remote: 0, local: 0)` | 0.2 s |
| the same one-line change, `buck2 build //:ci` (every dependant rebuilt and retested) | `574 (cached: 0, remote: 574, local: 0)` | 318 s |

The critical path (`buck2 log critical-path`) is a chain of whole-crate compiles followed by one
test run, because a test target is one action:

- the cold build with `hostkeeper-ui-tests` among the changed units: the coordinator's build
  script (37 s), its library (120 s), then `hostkeeper-ui-tests`' test target, all 495 of its
  tests in one action, 1,115 s. That one action is the lane's wall time while it runs.
- the one-line change through `//:ci`: `hostkeeper-wire` (31 s), `hostkeeper-console` (10 s),
  `hostkeeper-coordinator` (159 s), the daemon's test binary (118 s).

An action's outputs stay in the farm's content-addressed store: buck2 materializes a file on
this machine only when something here reads it (`materializations = deferred`). After all the
builds above, the local `buck-out` held 1.4 GB, 594 MB of it `buildrules sync`'s crate cache.
