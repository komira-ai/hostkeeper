# macOS CI jobs on the build farm's Macs

hostkeeper's macOS jobs run on GitHub-hosted Macs. A job that needs only a Mac (a build or a test as
an ordinary user: no root, no signing key, no network) can run on one of the build farm's Macs
instead. The mechanism is komira-ai/kbf's (its `docs/design/mac-ci-on-the-farm.md`): the job runs
on a hosted Linux runner, and the macOS work is one Remote Execution (REAPI) action on the farm's
Mac platform, sent by `cargo xtask mac-farm`. This page says what hostkeeper takes from it, what it
adds, and which jobs moved.

## The verb: `cargo xtask mac-farm`

`crates/xtask/src/mac_farm.rs`, ported from kbf's verb of that name. One Cargo command of the
workspace, after `--`, runs on a farm Mac:

1. The action's inputs are the files git tracks in the checkout, as they are on disk (so
   `release set-version` before it is seen); the crates of `Cargo.lock` for
   `aarch64-apple-darwin`, fetched on the runner into a Cargo home of the run's own; and the pinned
   Rust toolchain for `aarch64-apple-darwin` (`crates/xtask/src/mac_farm/toolchain.toml`: the
   release `rust-toolchain.toml` names, each component by URL and SHA-256). A farm Mac has the
   Command Line Tools and no Rust, and an action has no network, so all of it travels with the
   action. The farm's CAS is shared, so an unchanged input is sent once, not per run.
2. The Command is that toolchain's `cargo` with the arguments after `--`, offline, with the Mac's
   `/usr/bin` for its C compiler and linker, and each `--env NAME=VALUE` (a release build's
   `RELEASE_COMMIT`); the variables the verb sets itself cannot be set this way.
3. The action is executed and followed to its end (a cut stream is followed again with
   WaitExecution, or executed again when the farm has forgotten the operation); the log shows the
   stage changes and then the command's whole stdout and stderr.

The farm's address, instance, TLS setting and Mac platform are flags, which a workflow fills from
the organisation's variables `FARM_ADDRESS`, `FARM_INSTANCE`, `FARM_TLS` and
`FARM_DARWIN_ARM64_PROPERTIES`; no file of this repository holds an endpoint or a property set.
The verb masks the address in the log before it prints anything. With no address (a fork) it
prints a notice and passes.

The REAPI protos it speaks are vendored under `crates/xtask/proto/third_party`, each file pinned by
upstream commit and SHA-256 (`PINS`, held by a unit test), and compiled by xtask's `build.rs` with
`protox`, in pure Rust, as hostkeeper-proto compiles its own.

### What hostkeeper adds: the outputs

kbf's verb returns the command's output and exit code only. A build has to bring its programs
back, so hostkeeper's verb takes `--output <path>` (relative to the checkout, repeated): the paths
are the Command's `output_paths`, and when the command exits 0 each one is read from the farm's
CAS and written into the checkout at the same path, where a build on the runner would have left it.
Each output is written to a temporary name beside its path as it streams in (a large program is not
held whole), held to the digest the result names (SHA-256 and size), made executable when the
result says it is, and renamed onto its path only when every output has been read and checked. A
path the result does not hold as a file (missing, a directory, a symlink), a file the result holds
that no `--output` names, and a blob that is not its digest fail the run, and no output of it is
left in place. A command that failed brings nothing back.

With `--summary` (a job's `GITHUB_STEP_SUMMARY`) the verb appends what it sent (blobs, how many the
farm lacked, the bytes uploaded), how the run ended and each output it wrote.

## The compile job of artifacts.yml

`compile-macos` compiled `hostkeeper`, `hk`, `hello` and `hostkeeper-bootstrap` on a hosted Mac and
uploaded them as the artifact `macho-unsigned-macos-arm64`, which `sign-code` signs. Now:

- `compile-macos-farm` runs where the farm is configured (`vars.FARM_ADDRESS != ''` in this
  repository) and `compile-macos` everywhere else, under the exact negation of that condition, so
  exactly one of the two runs (the workflow lint evaluates both conditions for each case).
- `compile-macos-farm` calls `.github/workflows/mac-farm.yml`, whose one job, on `ubuntu-24.04`,
  sets the version, builds xtask, joins the farm's tailnet, runs the same `cargo build --locked
  --release` on a farm Mac with `RELEASE_COMMIT` and an `--output` for each program, and uploads
  them as the same artifact.
- `sign-code` and `build-macos` need both compile jobs. GitHub reads an `if:` that names no status
  function as `success() && ...`, which is false once any job it needs, at any depth, was skipped.
  So every job after the compile jobs, not only those that need them directly (`sign-code`,
  `build-macos`, `sign-pkg`, `check-pkg`, `sums`, `attest`, `verify`), starts its `if:` with
  `!cancelled() && !failure()`. `build-macos` and `sums` already did; the workflow lint
  (`mac_farm.rs`) refuses a job after a compile job whose `if:` does not.
- A farm that cannot be reached, or a build that fails there, fails the run as a failed build on a
  hosted Mac fails `compile-macos`; re-run the job once the farm answers.

### Why a reusable workflow: whose token it is

The farm's REAPI port is reachable only on its private tailnet, which the job joins with the run's
OIDC token (workload identity federation, `tag:ci`; nothing stored), so the job holds
`id-token: write`. The job also runs repository code: xtask, and the build scripts of its
dependencies. In artifacts.yml that would break the file's one invariant: hosts and
`docs/deploy/verify-a-release.md` accept an attestation whose certificate names
`.github/workflows/artifacts.yml` as its signer, and Sigstore names the workflow a token was minted
in. Any job of artifacts.yml holding `id-token: write` could have a token minted for that identity,
which is why only `attest`, which runs no repository code, holds it there.

A job of a reusable workflow mints its token as that workflow (its `job_workflow_ref`), so a
certificate for a token of `mac-farm.yml` names `mac-farm.yml`, which no consumer accepts.
`compile-macos-farm` only grants the scope to the job it calls, and no repository code runs in
artifacts.yml while it does. The workflow lint holds all of it: the call is the one local `uses:`
it allows, from that job alone; `mac-farm.yml` runs only on `workflow_call` with its one input; its
one job's steps are exactly the checkout, the toolchain, the version, xtask built before the
tailnet join (so no build script runs on the farm's network), the pinned Tailscale join, the one
`mac-farm` call and the upload; and its `id-token: write` holds only under the same exact `if:`.

## The tests of release.yml's `macos-test`

`macos-test` runs, on a hosted Mac in the release's beta stage, the tests of the crates the
`hostkeeper` and `hello` binaries are built from. Each of those crates' tests was run on a farm Mac
with `mac-farm` (`cargo test --locked --no-fail-fast -p <crate>`), to learn which pass there.

| crate | on a farm Mac | why it fails there |
|---|---|---|
| `hostkeeper-config` | passes | |
| `hostkeeper-launchd` | passes | |
| `hostkeeper-openrc` | passes | |
| `hostkeeper-placed` | passes | |
| `hostkeeper-proto` | passes | |
| `hostkeeper-release` | passes | |
| `hostkeeper-wire` | passes | |
| `hello` | fails | its tests bind loopback sockets |
| `hostkeeper` | fails | the daemon reads the farm Mac's own managed preferences |
| `hostkeeper-bootstrap` | fails | two tests of a file's owner and mode |
| `hostkeeper-daemon` | fails | loopback sockets, and the Mac's own sshd and groups |
| `hostkeeper-enroll` | fails | loopback listeners |
| `hostkeeper-hook` | fails | its test of a consumer crate outside the workspace |
| `hostkeeper-install` | fails | the sandbox strips a setuid bit and refuses a name that is not UTF-8 |
| `hostkeeper-link` | fails | loopback TCP |
| `hostkeeper-observe` | fails | a Unix socket |
| `hostkeeper-package` | fails | loopback HTTP and TLS servers |
| `hostkeeper-steps` | fails | the sandbox strips setuid bits |

`macos-test` stays as it is, on a hosted Mac with every crate. AGENTS.md B9 (this repository)
names `macos-test` and `macos-device` as the Mac's test jobs and `promote-pkg` waits for exactly
those two, so moving the seven crates that pass is a change to that rule, not to this workflow
alone; and while eleven crates stay hosted, the hosted job still runs and compiles the same
dependencies, so moving seven saves few hosted minutes. A loopback network level for a Mac action
(komira-ai/kbf's design lists it as missing) would let most of the rest run there.

## What a Mac action cannot do, and what is still hosted

- The sandbox denies every network operation, loopback TCP and Unix sockets included: a test that
  binds a socket fails there.
- The farm's Macs are machines of their own, not clean images: what a test reads from the Mac it
  runs on (its managed preferences, its users, its launchd) is that Mac's.
- No root, no GUI session, no device and no keychain: `macos-device` (tests under `sudo`, system
  launchd, `installer -target /`) waits for the farm's macOS VM job.
- Signing and notarisation (`sign-code`, `sign-pkg`, and `build-macos` and `check-pkg` with them)
  stay on hosted Macs until a decision on where the keys live.
- No pull-request job reaches the farm: a pull request gets no token for it (AGENTS.md B9 and B10:
  no macOS job and no `id-token: write` on a pull request).
