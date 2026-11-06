# hello: proving an install, an update and a rollback on the farm

hello (`examples/hello`) is the smallest real service a hostkeeper package carries: one
binary, `hello`, an HTTP server on loopback with an update hook. It goes the whole way a
service goes: a package in the release channel, assigned to the hosts of a role, installed,
updated from one version to the next, and rolled back when a release fails. This page is the
proof as an operator runs it, step by step, with what to watch in the UI at each step. Run it
before a real service (tierstore) is deployed with hostkeeper.

## What is built and what each step waits for

Each step below names the surface that does it. A step whose surface is not built yet is
marked **specified, not built**, with the pull request that builds it.

| Needed for | Built | Specified, not built |
|---|---|---|
| the package, its hook, its units and its release assets | `examples/hello`, `cargo xtask release package --service hello`, `artifacts.yml`, for linux-x86_64, linux-arm64 and macos-arm64 | — |
| running a hook step on a Linux host | the daemon runs each step through `SystemdLauncher` | — |
| running a hook step on a Mac | `LaunchdLauncher` (`hostkeeper-launchd`) | the daemon running its steps through it (`docs/macos.md`) |
| the UI's pages and their calls | the pages, and the HTTP/JSON gateway they call (`docs/gateway.md`) | — |
| assigning to a role | the rollout file's `roles`, `Services.Assign` (komira-ai/hostkeeper#45), the Rollout page's Assign, the coordinator's `install` order, and the daemon's heartbeat loop, which announces `install-v1` and runs the install | the daemon's heartbeat link to the coordinator: the shipped daemon gives its loop `NoLink`, so no order reaches a host |
| setting the desired version | `hostkeeper ctl releases set-desired`, CI's set-desired workflow, the Rollout page's Set desired (through the gateway) | — |
| unassigning with uninstall | the daemon's uninstall verb, hello's `uninstall` step, `Services.Unassign` (komira-ai/hostkeeper#45), the Rollout page's Unassign, and the daemon's heartbeat loop, which runs the uninstall | the daemon's heartbeat link to the coordinator (`NoLink`) |

## The package

| File | Content |
|---|---|
| `bin/hello` | the binary: `hello [--port <port>] [--fail-ready]` serves on `127.0.0.1:<port>` (default 9310); `hello update-hook <step>` is the hook |
| `manifest.toml` | the hook `["bin/hello", "update-hook"]` with a timeout for every step, the health URL `http://127.0.0.1:9310/readyz`, the two slots as run units, and the hook's `[params]` |
| `units/hello@.service` (Linux) | the template of the slots `hello@a.service` and `hello@b.service` |
| `units/hello.a.plist`, `units/hello.b.plist` (macOS) | the launchd jobs `hello.a` and `hello.b`, rendered by `hostkeeper-launchd`'s `service_plist` |

The committed goldens in `examples/hello/package/` are these files byte for byte, and list each
entry and its mode. `/readyz` answers 200, or 503 when the server runs with `--fail-ready`.
`/version` answers the version.

**Two slots, one at a time.** Both slots serve on port 9310, so only one runs. A slot runs only
while its marker `/var/lib/updater/hooks/hello/slots/<slot>` exists. The systemd unit checks it
with `ConditionPathExists=`. The launchd job is kept alive while it exists. So the slot that is
not serving never starts, at boot or by a restart. There is no hand-off: the cutover is a
restart, and clients see a short gap.

**The hook.** The daemon switches `current` to the new version before `cutover` and back before
`rollback`, so a slot started in either step runs the right version.

| Step | What hello does |
|---|---|
| `prepare <artifact>` | checks the staged release has `bin/hello`; picks the slot that is not serving |
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

1. **Releases.** Each release of this repository carries, per platform,
   `hello-<version>-<platform>.tar.zst` and its attestation, and `hello-index.json`. It also
   carries the failing build, version `<version>-fail-ready`, with
   `hello-fail-ready-index.json`. Stage each version into the channel's tree with
   `cargo xtask release channel-stage --service hello --version <version> --from <assets>
   --index <assets>/hello-index.json --out <tree>` (and `--version <version>-fail-ready
   --index <assets>/hello-fail-ready-index.json` for the failing build), which lays out
   `hello/<version>/index.json` beside its packages and their bundles
   ([docs/deploy/release-channel.md](deploy/release-channel.md)). A host fetches nothing else.
2. **The release index.** The desired state names the release by its index,
   `hello-index.json`, and the SHA-256 of that file (`sha256sum hello-index.json`). The
   coordinator orders each host the package its own platform has in the index, so the proof
   can run on hosts of any platform the index lists. A host of a platform the index does not list
   is ordered nothing and shows the hold `no_package_for_platform`. A fleet of one platform may
   name the one package digest instead (the `.sha256` file beside the package).
3. **The rollout entry.** Append `examples/hello/rollout.toml` to the coordinator's rollout file
   (`--rollout-file`). Replace the role `farm` with the role of the hosts that run the proof,
   and restart the coordinator. `cargo test -p hostkeeper-coordinator --test hello_rollout`
   checks that the coordinator accepts this entry.
4. **CI's allow-list.** If CI sets the desired version, the allow-list entry that admits its
   token names `hello` (`services = ["hello"]`, see `docs/deploy/ci-client.md`).

## 1. Assign hello to a role

The rollout entry assigns hello to every host of the role, and the coordinator orders an
install on each host of the role that does not run hello (`docs/operations/assignments.md`).
The install goes only to a daemon that announces `install-v1`; the daemon's heartbeat loop
announces it and runs the install. **Specified, not built:** the heartbeat link that carries the
order to the host; the shipped daemon gives its loop `NoLink`. Nothing is ordered until a desired
version is set.

An alternative to the rollout entry: in the Rollout page, section `hello`, type the role in
`Role` and press `Assign to the role`.

A host enrolled after this joins the role through its approval:
`hostkeeper ctl enrollment approve ... --role <role>`.

**Watch:** nothing is ordered yet. The Rollout page's section `hello` shows `Desired` as
`none set`. After an Assign in the page, `Assigned roles` lists the role and `Operator
assignments` shows `role <role>`. The Audit page shows an `assign` entry by the token's name. The
Host page's `Assigned services` lists hello for each host of the role, with `Desired` as
`none set`.

## 2. Set v1 and watch it converge

Set the desired version to the first release, `<v1>`:

```sh
jq -n --arg version <v1> --arg digest "$(sha256sum hello-index.json | cut -d' ' -f1)" \
  --rawfile index hello-index.json \
  '{desired: {version: $version}, releaseIndex: $index, releaseIndexDigest: $digest}' > desired.json
hostkeeper ctl --endpoint <coordinator> --ca-file <ca.pem> --token-file <token> \
  releases set-desired --service hello --request desired.json
```

`--rawfile` keeps the index byte for byte, which its digest covers. The coordinator refuses an
index whose SHA-256 is not the digest (`index_digest_mismatch`) and one that is not hello's at
`<v1>` (`index_invalid`).

Or let CI do it with the set-desired workflow (`docs/deploy/ci-client.md`), with
`service: hello`.

Or use the Rollout page, section `hello`: type `<v1>` in `Version`, the SHA-256 of the index in `Release
index digest` and the text of `hello-index.json` in `Release index`, press `Set desired` and
confirm. The page keeps the limits CI set earlier.

**Watch:**

- Rollout, section `hello`: `Desired` is `<v1>`. The canary host goes first, then the others
  one at a time. The role table counts `Updating`, then `At desired`. `State` ends `converged`.
- Fleet: each host shows `hello <v1>, healthy`. Healthy is the daemon's own probe of
  `/readyz` on each running slot.
- Host, `Last update`: `Verdict` succeeded, the steps prepare, cutover, verify and cleanup.
  `Assigned services` shows hello with `Installed` and `Desired` both `<v1>`, and `State` `at
  desired`; while the host is behind it shows `behind`.
- Audit: a `set_desired` entry by the token's name, when an operator set it in the page.

After the hosts have baked (two healthy beats each), `<v1>` is hello's last good version.

## 3. Set v2

Repeat step 2 with the next release, `<v2>`, and its digest (in the Rollout page's `Set
desired`, or the other ways).

**Watch:** the same pages. On each host the other slot now serves, at `<v2>`, and the one
that served `<v1>` is stopped. When the hosts have baked, `<v2>` is the last good version.

## 4. A failing build: watch the automatic rollback

Set the desired version to `<v2>-fail-ready`, with the digest of its package (the Rollout page's
`Set desired` does it). That build runs
the server with `--fail-ready`, so `/readyz` answers 503. (Its `/version` answers `<v2>`: `verify`
stops at the 503 and never compares it.)

**Watch:**

- Host, `Last update` of the canary: `Verdict` failed, `Phase` `gate_release`, `Reason`
  `not_ready`. The host's own hook rolled it back: the slot of `<v2>` serves again, and Fleet
  shows `hello <v2>, healthy`.
- Rollout, section `hello`: `Bad versions` lists `<v2>-fail-ready`, and `Desired` is back at
  `<v2>`, the last good version. No other host was ordered the failing build.
- Audit: a `mark_bad` entry by the coordinator, with the reason
  `<v2>-fail-ready failed its release on enough hosts`.

One failed host is enough for the rollback: the entry sets `rollback_after_failed` to
`min_hosts = 1`.

## 5. Unassign with uninstall

The uninstall goes only to a daemon that announces `install-v1`, which the daemon's heartbeat
loop does. **Specified, not built:** the heartbeat link that carries the order to the host
(`NoLink`).
Unassign hello in the Rollout page, section `hello`: type the role in `Role`, tick `Uninstall
from each host that loses its last assignment`, press `Unassign from the role` and confirm. Or
unassign it from the role (`--role <role>`) or from each host with ctl:

```sh
hostkeeper ctl ... services unassign --node-id <host> --service hello --uninstall true
```

The daemon then runs hello's `uninstall` step, which stops both slots and removes the hook's
record. It then removes hello's placed units, its package directory and its step records.
Remove the entry from the rollout file afterwards.

**Watch:** Fleet. The host no longer lists hello. The Audit page shows an `unassign` entry by the
token's name. The Rollout page's `Uninstalling` lists the hosts until each reports hello gone,
and the Host page's `Assigned services` shows hello with `State` `uninstalling`.

## What the proof does not show

- A hand-off without a gap: hello restarts. tierstore's port forward is its own (design
  `docs/design/host-updates.md`, section 4.1).
- The macOS hosts: the daemon does not run its steps through launchd yet (`docs/macos.md`).
- More than one platform in one rollout: see "One platform" above.
