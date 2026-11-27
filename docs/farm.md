# The farm lane: the workspace with Buck2 on the build farm

Cargo is hostkeeper's build of record. CI's gate (`ci.yml`), the coverage gate and every release
use it, on GitHub-hosted runners, and that does not change. The farm lane builds the same
workspace a second way: with Buck2, through [buildrules](https://github.com/komira-ai/buildrules)'
Rust rules, every action on a remote-execution build farm or taken from its cache. It is
additive and advisory: `.github/workflows/farm.yml` runs it on every push to `main` and every pull
request from a branch of this repository, and no check of it is required.

## Status

`.buckconfig` pins buildrules at a693e5c (its main at the merge of komira-ai/buildrules#167). That
commit has the rustfmt check, the Postgres fixture for tests and the build-commit stamp this lane
uses, and the fix for the two gaps of its Rust rules that failed this workspace's analysis
(komira-ai/buildrules#160 items 1 and 2: a member's self dev-dependency, and a member build
script's `links` dependencies). The numbers below were measured with those two fixes applied to an
earlier commit. Each other item of that issue is met here by declaring the file in `extra_srcs`,
or, for `CARGO`, by reading it with `option_env!` in the two tests that run cargo.

Before that pin, at buildrules 7dfc5c2, the analysis of `//:ci` failed on every tree with
`xtask: two dependencies named xtask`: xtask's dev-dependency on itself (for its feature
`test-support`) makes cargo list the same edge to its library twice in each of its test targets,
and the rules took the repeated edge for two different crates of one name. komira-ai/buildrules#167
takes a repeated edge once and still refuses two different units under one name; its analysis test
`edges_take_the_same_edge_listed_twice_once` (rules/rust/tests) fails at 7dfc5c2.

## What it builds

`buck2 build //:ci`: for every member of the workspace, its `:fmt` (the pinned `rustfmt --check`
over the files `cargo fmt` reads, with `rustfmt.toml`), its `:lint` (clippy over the library, the
binaries and every test target, with `[workspace.lints]` and `clippy.toml`, warnings as errors)
and, for the members listed below as farm-tested, its `:test`. A test target runs as a build
action: its `.passed` marker exists only if every test of the target passed, and a cached marker
means the tests passed on byte-identical inputs. The summary line must read
`Commands: N (cached: N, remote: N, local: 0)`: no action ran on the runner. `farm.yml`, after
farm-connect, runs in this order:

1. `cargo xtask farm-workers` (`crates/xtask/src/farm.rs`): buck2 has the farm's worker property
   set, or the job stops before anything is built. It reads buck2's configuration and builds
   nothing.
2. `buck2 build buildrules//cli:buildrules --out <file>`, the pinned `buildrules` command line,
   through komira's buck2-farm action (below).
3. `cargo xtask farm-table` (`crates/xtask/src/farm/table.rs`): the generated table is fresh by
   that command line, or the job hands it over as a patch and stops (below).
4. `buck2 build //:ci` through the buck2-farm action.
5. `buck2 kill`, however the job went, so no daemon outlives it.

## Every build through komira's buck2-farm action

Both builds are steps of komira's buck2-farm action (`.github/actions/buck2-farm` of
komira-ai/komira, pinned to the commit of komira-ai/komira#2439's merge, as farm-connect is). The
action runs `assert_remote run -- <buck2> build ...`: buck2 with its output in the job's log as it
prints it and in a file. When the run ends, the action fails the step unless buck2 succeeded and
its summary line says `local: 0`. While it runs, and after, it does what this repository's own
`farm-build` did once (a second run of a build whose only failures were cut connections), and more:

- **A run that hangs on a dead connection is ended.** Once a second it reads, from the kernel, the
  runner's TCP connections to the farm's address and port, and for each how long since anything
  arrived from the farm. A live farm sends on every open connection at least every 30 s (each open
  `Execute` stream gets its operation again, a quiet connection a PING), and closes one that moved
  nothing for 150 s; a connection silent for 300 s is one the farm has closed, which buck2 would
  wait on for ever. The action then ends buck2 (SIGINT, then SIGKILL) and its daemon (`buck2
  kill`), and treats the run as cut.
- **A cut run is run again once the farm answers**: one whose every failure is the farm's (a cut
  connection, buck2's `Error:` line with `RE channel error: code:`; or a job the farm could not
  start for an unavailable input), or one the watchdog ended. It waits for the farm's port to
  answer, at most farm-connect's wait (20 minutes), keeps the failed run's log beside the next, and
  runs the same command again; at most three runs in all. A real failure, or one mixed with cuts,
  is final at once.
- **What a contributor sees.** Each run again is one line in the log and one in the job summary:

  ```text
  buck2-farm: watchdog: the connection to the farm from local port 41022 has heard nothing from the farm for 5 min 1 s, past the bound of 5 min 0 s: the farm has closed it and buck2 would wait on it for ever; ending buck2 (SIGINT)
  buck2-farm: stopping buck2's daemon, which holds the dead connection (`./buck2 kill`)
  buck2-farm: the first run hung on a dead connection to the farm and was ended; waiting for the farm's port to answer, up to 20 min 0 s
  buck2-farm: the first run of `build` hung on a dead connection to the farm (nothing heard for 5 min 1 s, past the watchdog's bound of 5 min 0 s), so buck2 and its daemon were ended; the farm answered at once; ran again (run 2 of at most 3); the first run's log is buck2-farm.first.log
  ```

  (these are komira's recorded lines; here the program is the pinned buck2's path, not
  `./buck2`). A green step after such a line passed on its second or third run. A step that is red
  after three runs, or after a wait the farm did not answer within, names the farm in its last
  line: run the job again once the farm answers. A slow build whose connection is alive is never
  ended: the watchdog reads the connection, not the log, and no action's runtime is bounded.

The action's tool, assert_remote, is komira's `tools/build/ci/assert_remote` at the same commit,
which the job checks out (its `.zig` files alone, into `buck-out/komira`, which git and buck2 pass
over) and builds with the Zig release komira pins (0.15.2, its tarball checked against its
SHA-256), as komira's `zig_exe` builds it: `-OReleaseSafe`, a static musl executable, stripped. No
copy of its source is kept here. The workflow lint holds farm-connect, both buck2-farm steps and
that checkout to one komira commit.

This replaces `cargo xtask farm-build` and its own rule of a cut (`crates/xtask/src/farm/cuts.rs`,
one second run at once, no wait, nothing for a hang), which are removed: two wrappers around one
build would multiply their runs.

Not covered by the watchdog: a step that runs no buck2 build. Those are `farm-workers` (buck2's
configuration, no farm), `buck2 kill`, `mac-farm.yml`'s build on a farm Mac (its own Remote
Execution client, which has a deadline for its connect alone), and `buildrules sync`, which builds
the pinned toolchain with buck2 on the farm itself, inside `farm-table`, and only for a stale
table. Each of those, as every step of `farm.yml` and `mac-farm.yml`, ends at its own
`timeout-minutes` (below).

## Time limits

Every step of `farm.yml` and `mac-farm.yml` has its own `timeout-minutes`, under its job's; the
workflow lint holds that (`crates/xtask/tests/workflows/step_bounds.rs`). So a step that hangs ends
at its own bound, not at the job's. Each bound is sized from the longest successful run of that
step, measured from GitHub's record of each step's start and end in the runs below, with room for
what the step may legitimately add (a download's retries, a run again):

- `farm.yml`: the 193 successful runs of its job `build` from 2026-11-21 to 2026-11-27 (`gh run
  list --workflow farm.yml --status success`, then each run's jobs from the API). The old build
  step's work before `//:ci` (the worker check, the build of the `buildrules` command line, the
  table check) is read from the 34 of those runs whose log was read: from the step's start to
  buck2's first line of the `//:ci` build.
- `mac-farm.yml`: the 41 successful runs of artifacts.yml's job `compile macos-arm64 on the farm /
  compile` from 2026-11-24 to 2026-11-27.

| step | longest successful run | bound |
|---|---|---|
| checkout, `rustup toolchain install`, each cache restore and save | 0.23 min | 10 |
| buck2's and Zig's download on a cache miss (curl retries for up to 10 min) | 0.02 min (buck2) | 15 |
| the note of a failed download | (one `echo`) | 5 |
| buck2's install (builds xtask first) | 1.25 min | 15 |
| Zig's check and unpack; the checkout of assert_remote's sources | not run before (inferred: under a minute) | 10 |
| the build of assert_remote | not run before (inferred: under a minute) | 15 |
| farm-connect (its own wait for the farm is at most 20 min) | 1.65 min | 30 |
| `farm-workers` | part of the old build step's 2.6 min before `//:ci` (median 5 s) | 10 |
| the build of the `buildrules` command line | the same | 30 |
| `farm-table` (`buildrules sync` on a stale table builds the toolchain on the farm) | the same | 30 |
| `buck2 build //:ci` | 110.0 min (a busy farm: most of it queued), median 1.3 min | 150 |
| `buck2 kill` | (one command) | 5 |
| the upload of a stale table's patch | not run in those runs | 10 |
| the job | 110.8 min | 180 (was 120) |
| `mac-farm.yml`: checkout, `rustup`, `cargo xtask --help`, the tailnet join, the upload | 0.35 min | 10 |
| `mac-farm.yml`: `release set-version` (builds xtask first) | 1.27 min | 15 |
| `mac-farm.yml`: the build on a farm Mac | 6.23 min | 50 (unchanged) |
| `mac-farm.yml`: the job | 8.0 min | 60 (unchanged) |

A buck2-farm step's bound leaves room for its longest run, one hung run's end (5 min) and the wait
for the farm (at most 20 min) before a run again, which the farm's cache makes short: 30 for the
command line, 150 for `//:ci`. The job's bound went from 120 to 180 so that the build's 150 is under
it with the steps before.

The files of the build, all generated or written once by `buildrules`:

| file | what | written by |
|---|---|---|
| `.buckconfig` | the cells, buildrules pinned by commit, the execution platform | `buildrules init`, then by hand (the worker passthrough below) |
| `toolchains/`, `platforms/` | the pinned Rust toolchain and the remote execution platform | `buildrules init` |
| `BUCK`, `crates/*/BUCK`, `examples/hello/BUCK` | `cargo_workspace` and one `cargo_package` per member | `buildrules init` and `buildrules cargo adopt`, once |
| `buildrules.lock.bzl`, `buildrules.bzl`, `third_party/buildrules/` | Cargo's unit graph of every member, and every crates.io crate as a unit pinned by SHA-256 and size | `buildrules sync`; never edited by hand |

`Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml` stay the source: `//:check` fails every
target when the generated table no longer matches them. You do not need `buildrules` to fix that:
the job hands you the fix (next section).

## A stale table: the fix is a patch of the run

Before `//:ci` is built, `cargo xtask farm-table` (`crates/xtask/src/farm/table.rs`) runs
`buildrules check` with the command line the step before built on the farm. When the table is
stale (a pull request changed a `Cargo.toml` or `Cargo.lock` and not the table, or a generated file was edited or
removed), the job runs `buildrules sync` itself, checks again, and fails before the build with:

- an error and a job-summary section that name the inputs that changed and list the files the
  patch changes;
- the patch uploaded as the run's artifact `buildrules-sync` (kept 30 days);
- the one command that applies it, with the run filled in:

```sh
gh run download <run> -R komira-ai/hostkeeper -n buildrules-sync -D buck-out/buildrules-sync-<run> && git apply buck-out/buildrules-sync-<run>/buildrules-sync.patch
```

Read the patch, apply it in your checkout of the branch, commit and push. Nothing runs on your
machine but `gh` and `git`. The checkout of a pull request's run is its merge into main, so if
`git apply` refuses the patch, main has changed the table too: merge main and push, and the next
run carries a new patch.

The job does not push the fix to the branch itself. That would need a token that can write to the
repository in the one job that already holds an OIDC token and runs the pull request's own code; a
push with the job's `GITHUB_TOKEN` starts no workflow run, so the branch's new head would have no
CI; and a generated change of thousands of lines would land unread. A fork's pull request does not
run this job at all.

A sync that fails (crates.io unreachable from the runner), or one that leaves the table stale (a
member with no `cargo_package`), fails the job with its words and uploads nothing.

## What the farm leaves to the Mac

The farm builds and tests for Linux x86_64 alone. Code under `cfg(target_os = "macos")`, and every
test of it or arm of a test behind it, is compiled out there: it is neither run nor failed on the
farm. A test that only makes sense on macOS (one that needs APFS, launchd or a Mac's own programs)
is written that way, and runs in release.yml's job `macos-test` on a Mac, which tests exactly the
crates the `hostkeeper` and `hello` binaries are built from.

The farm-tested members with such code are `hostkeeper-bootstrap`, `-config`, `-daemon`,
`-install` and `-launchd`, and each one's BUCK file says so in one line. The workflow lint
(`crates/xtask/tests/workflows/farm_macos.rs`) fails when a farm-tested member has such code and
its BUCK file does not say so, when `macos-test` does not test such a member (its Mac tests would
run nowhere), or when a BUCK file says so of a member that has none.

The tests of the Mac's install programs under `maintenance_hook::macos` are not among them: they
run a fake program (a child of the test binary) in place of `softwareupdate` and `startosinstall`,
on any Unix, and the farm runs them. One of them,
`the_host_installers_run_with_exactly_their_arguments_input_and_an_empty_environment`, once failed
on a farm worker with one CPU, where libtest writes `test <name> ... ` on the child's first line of
output; it asks the child for `--test-threads=2`, and the farm runs of main pass it.

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
go on unless the farm answers within its wait (20 minutes; it masks the address in the log), and writes the farm's buckconfig under `~/.buckconfig.d`, where buck2
reads it. Its inputs are the organisation's variables `TS_CLIENT_ID` and `TS_AUDIENCE` (the tailnet
identity), `FARM_ADDRESS`, `FARM_INSTANCE`, `FARM_LINUX_X86_64_PROPERTIES` and `FARM_TLS`.
farm-connect writes the worker property set under `[komira_re]`; `.buckconfig` hands it to
buildrules (`[buildrules_re] linux_x86_64_properties = $(config komira_re.linux_x86_64_properties)`)
and holds no value of its own.

That join is why the job holds `id-token: write` on a pull request, the one job of a pull-request
workflow that may (AGENTS.md, B10 of this repository). The workflow lint
(`crates/xtask/tests/workflows/farm.rs`) holds it to: exactly that scope and `contents: read`;
the exact `if:` that skips a fork's pull request (which gets no OIDC token) and Dependabot's; farm-connect's inputs exactly
the variables above; every buck2 build on the farm through komira's buck2-farm action (the build of
the `buildrules` command line and of `//:ci`), no other buck2 command but `farm-workers`,
`farm-table` and `buck2 kill`, and those steps in their order after farm-connect's; farm-connect,
buck2-farm and the checkout of assert_remote's sources at one komira commit; and the triggers, one
job and one runner. It does not fix the job's other steps, each of which could ask for the token:
a change to them is reviewed like any change to a workflow.

A pull request's build and a build of `main` share the farm's action cache, as every build of
every repository on the farm does: a result one writes, the other reads. The farm's cache is
keyed by the action's digest, so a result is reused only for byte-identical inputs and command.

## Measured

The first measurements of the lane, of `//:ci` as it is here, with buildrules plus the two fixes
its `cargo.bzl` needed for this workspace (komira-ai/buildrules#160 items 1 and 2, since merged as
komira-ai/buildrules#167). Wall times are buck2's, on a machine that ran no action itself.

| build | `Commands:` line | wall |
|---|---|---|
| cold `//:ci` after every first-party unit changed (third-party crates already in the farm's cache) | `474 (cached: 0, remote: 474, local: 0)` | 655 s |
| the same, again, with buck2's daemon restarted (nothing in its memory) | `3356 (cached: 3356, remote: 0, local: 0)` | 4.2 s |
| the same, again, daemon kept | no action looked up | 0.07 s |
| one-line change to `hostkeeper-wire`'s library, `buck2 build //crates/hostkeeper-wire:test` | `2 (cached: 0, remote: 2, local: 0)` | 40.6 s, 45.0 s |
| the change reverted | `2 (cached: 2, remote: 0, local: 0)` | 0.2 s |
| the same one-line change, `buck2 build //:ci` (every dependant rebuilt and retested) | `574 (cached: 0, remote: 574, local: 0)` | 318 s |

At buildrules main before #167 (with `:fmt` in `//:ci` and those two changes applied), after
the daemon's two stage tests were changed (`--test-threads=2`):

| build | `Commands:` line | wall |
|---|---|---|
| `//:ci`, the daemon's test units changed | `132 (cached: 0, remote: 132, local: 0)`, BUILD SUCCEEDED | 2,227 s |
| the same, again, buck2's daemon restarted | `3448 (cached: 3448, remote: 0, local: 0)` | 1.9 s |

Its critical path is the daemon's test target `boot_sweep` (its two tests kill the daemon at every
write of an update and start it again): 2,200 s in one action, on one CPU.

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
