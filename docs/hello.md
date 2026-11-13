# hello: proving an install, an update and a rollback on the farm

hello (`examples/hello`) is the smallest real service a hostkeeper package carries: one
binary, `hello`, an HTTP server on loopback with an update hook. It goes the whole way a
service goes: a native package, listed by a host group, installed on the hosts of the group,
updated from one version to the next, and rolled back when a release fails. This page is the
proof as an operator runs it, step by step, with what to watch in the UI at each step. Run it
before a real service (tierstore) is deployed with hostkeeper.

## What is built and what each step waits for

Each step below names the surface that does it. A step whose surface is not built yet is
marked **specified, not built**, with the pull request that builds it.

| Needed for | Built | Specified, not built |
|---|---|---|
| the package, its hook, its units and its release assets | `examples/hello`, `cargo xtask release deb --service hello` and `release macos-pkg --service hello`, `artifacts.yml`, for linux-x86_64, linux-arm64 and macos-arm64 | — |
| running a hook step on a Linux host | the daemon runs each step through `SystemdLauncher` | — |
| running a hook step on a Mac | the daemon runs each step as a launchd job through `LaunchdLauncher` (`hostkeeper-launchd`), and links and loads the package's plists (`docs/macos.md`) | — |
| the UI's pages and their calls | the pages, and the HTTP/JSON gateway they call (`docs/gateway.md`) | — |
| placing hello on hosts | a host group that lists hello (`HostGroups.Put`, `examples/hello/host-group.json`), an approval into it (`Enrollment.Approve` or an inventory record naming the group) or `HostGroups.Move` for a host to get into it, the coordinator's `install` order, and the daemon's heartbeat loop, which announces `install-v1` and runs the install | the daemon's heartbeat link to the coordinator: the shipped daemon gives its loop `NoLink`, so no order reaches a host |
| setting the desired version | `hkctl releases set-desired` (with `--host-group` to name the group), CI's set-desired workflow and its hello caller `hello-desired.yml`, the Rollout page's Set desired (through the gateway) | `host_group` as an input of the CI client: the callers' pinned release does not take it (see "The proof from CI") |
| taking hello out with uninstall | the daemon's uninstall verb, hello's `uninstall` step, a `Put` of the group without hello, and the daemon's heartbeat loop, which runs the uninstall | the daemon's heartbeat link to the coordinator (`NoLink`) |

## The package

hello's package is a native one: a `.deb` for each Linux platform and a `.pkg` for macOS. The
binary is `hello [--port <port>] [--fail-ready]`, which serves on `127.0.0.1:<port>` (default
9310); `hello update-hook <step>` is the hook. The hook bounds (a timeout for every step), the
health URL `http://127.0.0.1:9310/readyz`, the two slots as run units and the hook's `[params]`
are the manifest of the release's entry, which `cargo xtask release targets` writes; the
package carries none of them. The committed goldens in `examples/hello/package/linux` and `macos`
list the units and jobs of the slots and their modes.

**The native packages.** hello and its failing build are built as a `.deb` for each Linux
platform (`cargo xtask release deb --service hello`) and a `.pkg` for macOS (`cargo xtask release
macos-pkg --service hello`), which hostkeeper installs with `apt` and `installer` ([design: native packages only](design/packages-native.md), sections
10.2 and 10.6). Neither has a maintainer script, which is hello's choice and not a rule. The manifest's hook
command and bounds, run units, health URL and params are the manifest of the release's entry, not a file of the package:
the hook command is the absolute path of the program the package installs with `update-hook`, `["/usr/lib/hello/hello",
"update-hook"]` for the `.deb` and `["/Library/Application Support/hello/bin/hello", "update-hook"]` for the `.pkg`.

| `.deb` | `.pkg` | What it is |
|---|---|---|
| `/usr/lib/hello/hello` | `/Library/Application Support/hello/bin/hello` | the program |
| `/usr/lib/systemd/system/hello@a.service`, `hello@b.service` | `/Library/LaunchDaemons/hello.a.plist`, `hello.b.plist` | one unit or job per slot |

Their goldens are `examples/hello/package/deb` and `pkg`. The daemon runs the hook program's
sealed copy in the version it staged, so `prepare` runs the new version's program before it is installed. `/readyz` answers 200, or 503 when the server runs with `--fail-ready`.
`/version` answers the version of the package: `<version>`, or `<version>-fail-ready` when the
server runs with `--fail-ready`, as the failing build's units start it. So `/version` alone tells
the failing build from the good one.

**Two slots, one at a time.** Both slots serve on port 9310, so only one runs. A slot runs only
while its marker `/var/lib/updater/hooks/hello/slots/<slot>` exists. The systemd unit checks it
with `ConditionPathExists=`. The launchd job is kept alive while it exists. So the slot that is
not serving never starts, at boot or by a restart. There is no hand-off: the cutover is a
restart, and clients see a short gap.

**The hook.** The daemon switches `current` to the new version before `cutover` and back before
`rollback`, so a slot started in either step runs the right version.

| Step | What hello does |
|---|---|
| `prepare <artifact>` | checks the staged release has its program (`bin/hello`, or in a native package's `tree/` the program's path above); picks the slot that is not serving |
| `cutover` | stops the serving slot, starts the other one |
| `verify` | `GET /readyz`, then `GET /version` on 127.0.0.1:9310. Not listening yet: exit 75, run again. 503: exit 1, reason `not_ready`. Another version: exit 1, reason `wrong_version` |
| `rollback` | stops the new slot, starts the old one again |
| `cleanup` | refuses unless `verify` succeeded; records the new slot as the serving one |
| `uninstall` | stops both slots and removes the hook's record |

No step reports `reversible: false`, because `rollback` can always start the old slot again.
A failed `verify` is therefore a failed update in the phase `gate_release`, class `release`.
The coordinator's automatic rollback counts that phase, as it counts `after_commit`.

The hook keeps its record in `/var/lib/updater/hooks/hello` (the manifest's `[params]`), and
makes that directory, owner only, on its first step.

## Before you start

1. **Releases.** Each release of this repository carries, per Linux platform,
   `hello_<version>_<deb-arch>.deb` and its attestation, and for macOS
   `hello-<version>-macos-arm64.pkg`. The release workflow puts the `.deb` into the `testing`
   suite of the project's package repository ([package-repositories.md](deploy/package-repositories.md)),
   where a host fetches it. It also carries the failing build, version `<version>-fail-ready`, as
   a `.deb` for each Linux architecture (`hello_<version>-fail-ready_<deb-arch>.deb`) with its
   attestation bundle; its `.pkg` is not a release asset.
2. **The targets.** The desired state names the release by its targets, written by
   `cargo xtask release targets --service hello` from the release's assets
   (`docs/design/packages.md`, section 3.7; `docs/design/packages-native.md`, section 10.5): per
   Linux platform, hello's native entry alone, its `.deb` from the repository's `testing` suite
   (docs/deploy/package-repositories.md), and for macOS its signed `.pkg`'s `macos-pkg` entry
   alone. A daemon that announces `apt` (or, on a Mac, `macos-pkg`: its signers file loaded,
   `/usr/sbin/installer` and `/usr/sbin/pkgutil` present) is ordered it; any other is ordered
   nothing (`installer_dropped`, or `no_build_for_host` when the pick finds no build for the
   host). The failing build has no `.pkg`, so it orders no Mac. CI (`hello-desired.yml`, below) writes and sets them. The failing
   build's `.deb` is in the separate suite `testing-fail` and has the Debian version
   `<version>~fail.ready` (`~` sorts below `<version>`, so apt never takes it for the release);
   only the dispatch of step 4 orders it, with the repository entry `komira-testing-fail`.
3. **No rollout entry.** hello needs no entry of a rollout file: `serve` refuses
   `--rollout-file`, and what a package has alone is set through the API. hello comes from the
   default repository `komira`; to take it from another registry entry, call
   `Services.SetRepository` (`hkctl services set-repository`) before the first `SetDesired`.
   The group of `examples/hello/host-group.json` (section 1 below) is what places
   hello and sets its policy.
4. **CI's allow-list.** If CI sets the desired version, the allow-list entry that admits its
   token names `hello` (`services = ["hello"]`), and may limit the groups it deploys to
   (`host_groups = ["farm"]`; left out, the token may deploy to any group that lists hello). The entry for `hello-desired.yml`, below, is in
   `docs/deploy/ci-client.md`, "The allow-list entry". The workflow also reads the repository
   variables `HOSTKEEPER_COORDINATOR_URL`, `HOSTKEEPER_COORDINATOR_CA` (the PEM of the
   coordinator's CA), `HOSTKEEPER_CI_AUDIENCE` (the entry's `audience`),
   `HOSTKEEPER_TAILNET_TAG` (`tag:ci`, passed as `tailnet_tag`; the CI client checks it at its
   first step, so it must be set whether or not the run joins the tailnet), and the tailnet's
   `HOSTKEEPER_TAILNET_CLIENT_ID`, `HOSTKEEPER_TAILNET_AUDIENCE` and
   `HOSTKEEPER_TAILNET_COORDINATOR`: with the client ID set, the CI client joins the tailnet
   before its call, as `release.yml`'s does, so a coordinator served only on the tailnet is
   reachable; empty, it does not join.
5. **The failing build's repository entry** (step 4 only). A host that is to take the failing
   build needs the repository entry `komira-testing-fail` besides `komira-testing`. Put the
   registry's repository `komira` with the suite `testing-fail` added to `testing` and `stable`
   ([repository-keys.md](operations/repository-keys.md#put-the-repository-in-the-registry)); each
   Linux host whose services name `komira` writes
   `/etc/hostkeeper/repositories/komira-testing-fail.sources` at its next heartbeat, with no
   restart. Until then the coordinator refuses the set (`repository_unknown`), since the entry
   names a channel `komira` does not list.

## 1. Put the group `farm`, with hello in it

`examples/hello/host-group.json` is the group of the proof, the body of `HostGroups.Put` in
the canonical JSON of the proto: the group `farm`, the packages `hostkeeper` then `hello`
(`hostkeeper` is listed by every group), and a policy of a lone canary, two baked beats, and
automatic rollback once one failed host or 25 percent of the group's hosts have failed the release
(`rollback_after_failed` with `percent` 25 and `min_hosts` 1); the stages are the default,
1, 10, 50 and 100 percent. Put it:

```sh
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups put --request examples/hello/host-group.json --reason "the hello proof"
```

`cargo test -p hostkeeper-coordinator --lib hello_group` puts this file through the gateway and
reads the group back. A coordinator places by group alone
([hostgroups.md](operations/hostgroups.md#how-the-coordinator-places-hosts)): a host runs the packages the built-in
group `all` lists and then those its group lists, and no others; a host in no group runs `all`'s alone, until it is moved into one. A
coordinator that already holds groups gets `farm` beside them.

A host already enrolled gets into the group by `Move`:

```sh
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id <host> --host-group farm --reason "a host of the proof"
```

A host enrolled after this is approved into the group instead, by an approval or an inventory
record naming it (`hkctl enrollment approve --host-group farm`,
[enrollment.md](operations/enrollment.md)), and installs the group's packages in order from its
first heartbeat. A moved host is then moving: at its heartbeats it is admitted into the plans of the group's packages,
and ordered to install the packages of the group in the order of the list, once the group has a
desired version for them ([hostgroups.md](operations/hostgroups.md#moving-a-host)). The install
goes only to a daemon that announces `install-v1`; the daemon's heartbeat loop announces it and
runs the install. **Specified, not built:** the heartbeat link that carries the order to the host;
the shipped daemon gives its loop `NoLink`. Nothing is ordered for hello until a desired version
is set.

`Services.Assign` and `Services.Unassign` are refused `placed_by_host_group`, the
Rollout page's Assign and Unassign with them: the group is the only placement.

**Watch:** nothing is ordered yet for hello. `hkctl host-groups get --host-group farm` answers
the group with its generation, its hosts, and per package its desired version (none set) and its
convergence. Hosts and Home show a moved host with the group as its role. The Audit page shows a
`host_group_put` and a `host_move` entry by the token's name, which `Audit.List` with
`host_group` `farm` lists.

## 2. Set v1 and watch it converge

Set the desired version to the first release, `<v1>`:

```sh
jq -n --arg version <v1> --slurpfile targets targets.json \
  '{desired: {version: $version}, targets: $targets[0]}' > desired.json
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    releases set-desired --service hello --request desired.json
```

`targets.json` holds the JSON of the `targets=` line that `cargo xtask release targets` prints,
without the prefix:

```sh
cargo xtask release targets --service hello --version <v1> --dir <assets> \
  | sed -n 's/^targets=//p' > targets.json
```

`<assets>` is a directory of the release's `.deb`s and their bundles.

Add `--host-group farm` to deploy to that group alone; with none, the set is one deployment to
each group that lists hello, which is `farm` while it is the only one. A set that carries
`max_updating` or `min_up` is refused `limits_in_host_group`: the limits are the group's policy.
A set naming a group that does not list hello is refused `not_in_host_group`.

Or let CI do it: dispatch `hello-desired.yml` with `version` `<v1>` and `variant` `hello`
(see "The proof from CI" below).

Or use the Rollout page, section `hello`: type `<v1>` in `Version`, press `Set desired` and
confirm.

**Watch:**

- `hkctl releases converge --service hello --host-group farm`: the group's convergence.
- Rollout, section `hello`: `Desired` is `<v1>`. The canary host goes first, then the others by
  the stages of hello's update plan, each baked before the next; a stage's hosts update in
  parallel up to the group's `max_updating` (1 unless the policy sets more), so one at a time by
  default.
  The plan shows "stage k of n" and why it waits. The role table counts `Updating`, then `At
  desired`. `State` ends `converged`.
- Home: each host shows `hello <v1>, healthy`. Healthy is the daemon's own probe of
  `/readyz` on each running slot.
- Host, `Last update`: `Verdict` succeeded, the steps prepare, cutover, verify and cleanup.
  `Assigned services` shows hello with `Installed` and `Desired` both `<v1>`, and `State` `at
  desired`; while the host is behind it shows `behind`.
- Audit: a `set_desired` entry naming the group `farm`, by the token's name.

After the hosts have baked (two healthy beats each), `<v1>` is hello's last good version.

## 3. Set v2

Repeat step 2 with the next release, `<v2>`, and its digest (in the Rollout page's `Set
desired`, or the other ways).

**Watch:** the same pages. On each host the other slot now serves, at `<v2>`, and the one
that served `<v1>` is stopped. When the hosts have baked, `<v2>` is the last good version.

## 4. A failing build: watch the automatic rollback

Set the desired version to `<v2>-fail-ready`: dispatch `hello-desired.yml` with `version` `<v2>`
and `variant` `fail-ready` ("The proof from CI" below). Each Linux platform's list then holds
one `apt` entry of the failing build's `.deb`, from the repository entry `komira-testing-fail`
(the suite `testing-fail`) at the Debian version `<v2>~fail.ready`; the hosts need that entry
("Before you start", item 5). That build runs the server with `--fail-ready`, so `/readyz`
answers 503. Its `/version` answers `<v2>-fail-ready`, its own version, so a probe of `/version`
tells it from `<v2>` (`verify` stops at the 503 and never compares it).

**Watch:**

- Host, `Last update` of the canary: `Verdict` failed, `Phase` `gate_release`, `Reason`
  `not_ready`. The host's own hook rolled it back: the slot of `<v2>` serves again, and Home
  shows `hello <v2>, healthy`.
- Rollout, section `hello`: `Bad versions` lists `<v2>-fail-ready`, and `Desired` is back at
  `<v2>`, the last good version. No other host was ordered the failing build.
- Audit: a `mark_bad` entry by the coordinator, with the reason
  `<v2>-fail-ready failed its release on enough hosts`.

One failed host is enough for the rollback: the group's policy sets `rollback_after_failed` to
`min_hosts` 1. A version marked bad is bad in every group, so a build that fails in `farm` is
not rolled on to another group.

**Another service is not held.** While the failing build is in flight on the canary, hello's
update holds one of the group's live rollouts (the policy's `live_rollouts`, by default the
coordinator's `--live-rollouts`), and another package's new version waits when none is free. Once the canary
reports the failure, the failing build's rollout stops there and hello is rolled back, and the
live rollout is free (komira-ai/hostkeeper#259): the other package's hosts are ordered as usual.

## The proof from CI: hello-desired.yml

`.github/workflows/hello-desired.yml` sets hello's desired version through the CI client
(`set-desired.yml`) with the OIDC token GitHub issues to the job. That is the path a service's
release job takes, so a run of it proves that path, from CI to the coordinator, on hello before a
real service depends on it. Dispatch it from main (Actions, `hello-desired`, `Run workflow`, or
`gh workflow run hello-desired.yml --ref main -f version=<v> -f variant=<variant>`). Its inputs:

| input | values | meaning |
|---|---|---|
| `version` | a release of this repository, `X.Y.Z` | The release whose hello package is set. |
| `variant` | `hello` or `fail-ready` | `hello` sets `<version>`; `fail-ready` sets `<version>-fail-ready`, the build whose `/readyz` answers 503. |

Its first job, `release`, downloads the variant's `.deb` for each Linux architecture with the
`.deb`'s attestation bundle, and for `hello` its signed `.pkg` with the `.pkg`'s bundle, and runs
`cargo xtask release targets --service hello` (with `--fail-ready` for the failing build) on
them: each platform's list holds the variant's native entry alone
(docs/design/packages-native.md, section 10.5). For `hello` a Linux entry names the repository
entry `komira-testing` and the Debian version `<version>`, and the macOS list holds the `.pkg`'s
`macos-pkg` entry, which a Mac whose daemon announces `macos-pkg` takes (a Mac that does not is
ordered nothing); for `fail-ready` the entry names `komira-testing-fail` and
`<version>~fail.ready`, the `.deb` named `hello_<version>-fail-ready_<arch>.deb` in the release,
and macOS has no list (the failing build has no `.pkg`). The verb refuses, naming every problem,
a `.deb`, `.pkg`, control file or bundle that does not check out; then nothing is set. Its second job calls `set-desired.yml` with the targets alone, so each host is ordered its
own platform's `.deb`. It names no host group: the revision it is pinned to has no `host_group`
input, so the set is one deployment to each group that lists hello, `farm` here. (The CI client
takes `host_group` from the release after the one the callers pin; `hello-desired.yml` passes it
once its pin moves there.) It is pinned to a hostkeeper release, as any service pins it
(`docs/deploy/ci-client.md`, "Pinning hostkeeper"), with `wait` left at `true`: the job waits
for the fleet. Once it has set, `hkctl releases get-release --service hello --version <version>`
answers the release's targets, one list per platform.

Run it twice, in this order, with the same `version`, a release the hosts are not already at:

1. **`variant: hello`.** The coordinator stores `<version>` as hello's desired version. The canary
   host updates first, bakes, then the others stage by stage, as in step 2. The job ends green,
   with the outputs `desired_version` `<version>`, `converged` `true` and `reason` `converged`.
   The wait is one call (`hkctl releases wait-converged`) that the coordinator holds until the
   fleet has converged, at most 30 minutes (`max_polls` left at 180).
2. **`variant: fail-ready`.** The coordinator stores `<version>-fail-ready`. The canary host fails
   its `verify` (`not_ready`, phase `gate_release`) and rolls itself back. The coordinator marks
   `<version>-fail-ready` bad and sets the desired version back to `<version>`, the last good one,
   as in step 4, and orders no other host. The job fails, with `desired_version`
   `<version>-fail-ready`, `converged` `false` and `reason` `superseded`: another desired version
   replaced the one it set. That failure is the proof's expected outcome. A second run of the same
   `fail-ready` version fails at once with `reason` `held_bad`, because the version is marked bad;
   use the next release for another run.

A run refused before anything is set says so in its `reason` (`docs/deploy/ci-client.md`, "When it
fails"): `unauthorized` or `forbidden` when the allow-list entry does not admit the token, for
example when `job_workflow_ref` does not name the commit of the `uses:` line.

## 5. Take hello out of the group, with uninstall

Put the group again without hello, naming the generation `Get` or `List` answered:

```sh
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups put --request farm.json --reason "end of the hello proof"
```

with `farm.json` the group's body as `host-group.json` has it, but for `"packages": ["hostkeeper"]`
and `"ifGeneration": "<generation>"` beside `hostGroup`. A package taken out of the list is
uninstalled from the group's hosts that listed it at their last heartbeat to this coordinator, one
uninstall intent for each, written with the `Put`
([hostgroups.md](operations/hostgroups.md#the-calls)). The uninstall goes only to a daemon that
announces `install-v1`, which the daemon's heartbeat loop does. **Specified, not built:** the
heartbeat link that carries the order to the host (`NoLink`).

The daemon then runs hello's `uninstall` step, which stops both slots and removes the hook's
record. It then removes hello's placed units, its package directory and its step records.

**Watch:** Home. The host no longer lists hello. The Audit page shows a `host_group_put` entry
by the token's name.
The Rollout page's `Uninstalling` lists the hosts until each reports hello gone, and the Host
page's `Assigned services` shows hello with `State` `uninstalling`.

## What the proof does not show

- A hand-off without a gap: hello restarts. tierstore's port forward is its own (design
  `docs/design/host-updates.md`, section 4.1).
