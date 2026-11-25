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
`Commands: N (cached: N, remote: N, local: 0)`: no action ran on the runner. `farm.yml` runs the build
as one command, `cargo xtask farm-build` (`crates/xtask/src/farm.rs`): it refuses to start
without the farm's worker property set, hands over a stale generated table as a patch instead of
building (below), runs a build whose only failures are cut connections to the farm once more
(below), fails a build that does not say `local: 0`, and stops buck2's daemon however the build
ended.

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

Before it builds, `cargo xtask farm-build` (`crates/xtask/src/farm/table.rs`) builds the pinned
`buildrules` command line on the farm and runs `buildrules check`. When the table is stale (a pull
request changed a `Cargo.toml` or `Cargo.lock` and not the table, or a generated file was edited or
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

## A cut connection to the farm: the build runs once more

The way from the runner to the farm is a private network over the internet, and a stream of it is
sometimes cut for a moment. buck2 then fails every action it had in flight, each with its own
report:

```text
Action failed: hostkeeper//crates/...
Internal error (stage: remote_call_error): Remote Execution Error on Execute with digest ... for ReSession ...
Error: (Error was returned on the stream by RE: RE channel error: code: 'Unknown error', message: "h2 protocol error: error reading a body from connection", ... BrokenPipe ...)
```

buck2 writes `Error: (Failed to query capabilities of remote: ... tcp connect error ...)` when it
cannot reach the farm at all; that counts as a cut too (it names the farm alone, never the change),
so a farm that is down for both runs fails the job as a second cut, by name.

When the build fails and every failure in what buck2 printed is such a cut
(`crates/xtask/src/farm/cuts.rs`, the rule of buildrules' stages since komira-ai/buildrules#172 and
of komira's farm steps since komira-ai/komira#2155), `farm-build` runs the same build once more, at
once. buck2's own log lines at `WARN` (its `Transient error (attempt 1/5), retrying ...: keep-alive
timed out` comes with every cut) are passed over; one at `ERROR` is a failure. A line in the log
and in the job summary says so, so a green build that needed a second run
is not hidden. The farm's cache keeps every action the first run finished, so the second run does
only what the cut stopped.

A real failure is never run again. The build's failure is final when any report's reason is not a
remote-execution error with the cut under it (a compiler or test that `returned non-zero exit
code`, a timeout), when any other error is printed anywhere (an analysis error, a compiler's
`error:`, a panic, a download that failed, such as a stand-alone `Failed to make BatchReadBlobs
request ... operation was cancelled`), or when buck2 was ended by a signal. A test that only prints
the cut's words is not a cut: they must be buck2's own `Error:` line under the action's
remote-execution error.

Only the build of the target is run again. The short build of the `buildrules` command line before
it, and the audit of the worker property set, fail the job as before when the farm is cut under
them.

The second run is final. If it is cut too, the job fails saying the build was cut on its second run
too. One cut is a moment's break in the way to the farm; a second in a row means the farm or the
network to it is down, and a third try would only spend the job's two hours and hide an outage
behind a slow green. Run the job again once the farm answers.

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
