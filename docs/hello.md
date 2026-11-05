# hello: proving an install, an update and a rollback on the farm

hello (`examples/hello`) is the smallest real service a hostkeeper package carries: one
binary, `hello`, an HTTP server on loopback with an update hook. It goes the whole way a
service goes: a package in the release channel, assigned to the hosts of a role, installed,
updated from one version to the next, and rolled back when a release fails. This page is the
proof as an operator runs it, step by step, with what to watch in the UI at each step. Run it
before a real service (tierstore) is deployed with hostkeeper.

## What is built and what each step waits for

Each step below names the surface that does it. Most are on main. Four are not: the steps
that need them say so.

| Needed for | Built | Not built yet |
|---|---|---|
| the package, its hook, its units and its release assets | `examples/hello`, `cargo xtask release package --service hello`, `artifacts.yml` | the macos-arm64 assets (the verb builds the package; the macOS release job adds it to the release) |
| running a hook step on a host | the daemon's step runner | the systemd step launcher, komira-ai/hostkeeper#9 (on main the daemon refuses every step) |
| the UI's calls | the pages | the HTTP/JSON gateway they call, komira-ai/hostkeeper#51 |
| assigning to a role | the rollout file's `roles` and `install_when_absent` | — |
| setting the desired version | `hostkeeper ctl releases set-desired`, CI's set-desired workflow | a UI control: the Rollout page shows the desired version and never sets it |
| unassigning with uninstall | the daemon's uninstall verb and hello's `uninstall` step | `Services.Unassign`, komira-ai/hostkeeper#45, and a UI control |

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

The commit is the success of `cutover`. A failed `verify` is therefore a failure after the
commit, which is what the coordinator's automatic rollback counts.

## Before you start

1. **Releases.** Each release of this repository carries, per Linux platform,
   `hello-<version>-<platform>.tar.zst` and its attestation, and `hello-index.json`. It also
   carries the failing build, version `<version>-fail-ready`, with
   `hello-fail-ready-index.json`. Copy each index to the channel as
   `<release_channel>/hello/<version>/index.json`, beside its packages. A host fetches nothing
   else.
2. **One platform.** The desired state carries one package digest, and every host checks the
   package it fetches against it. Run the proof on hosts of one platform, and use the digest
   of that platform's package: the `.sha256` file beside it.
3. **The rollout entry.** Append `examples/hello/rollout.toml` to the coordinator's rollout file
   (`--rollout-file`). Replace the role `farm` with the role of the hosts that run the proof,
   and restart the coordinator. `cargo test -p hostkeeper-coordinator --test hello_rollout`
   checks that the coordinator accepts this entry.
4. **CI's allow-list.** If CI sets the desired version, the allow-list entry that admits its
   token names `hello` (`services = ["hello"]`, see `docs/deploy/ci-client.md`).

## 1. Assign hello to a role

The rollout entry assigns hello to every host of the role. `install_when_absent = true` makes
the coordinator order an install on a host of the role that has never run hello. Nothing is
ordered until a desired version is set.

A host enrolled after this joins the role through its approval:
`hostkeeper ctl enrollment approve ... --role <role>`.

**Watch:** Fleet, Hosts table. Each host of the role shows `hello not installed`.

## 2. Set v1 and watch it converge

Set the desired version to the first release, `<v1>`:

```sh
echo '{"desired":{"version":"<v1>"}}' > desired.json
hostkeeper ctl --endpoint <coordinator> --ca-file <ca.pem> --token-file <token> \
  releases set-desired --service hello --package-digest <sha256 of hello-<v1>-<platform>.tar.zst> \
  --request desired.json
```

or let CI do it with the set-desired workflow (`docs/deploy/ci-client.md`), with
`service: hello`.

**Watch:**

- Rollout, section `hello`: `Desired` is `<v1>`. The canary host goes first, then the others
  one at a time. The role table counts `Updating`, then `At desired`. `State` ends `converged`.
- Fleet: each host shows `hello <v1>, healthy`. Healthy is the daemon's own probe of
  `/readyz` on each running slot.
- Host, `Last update`: `Verdict` succeeded, the steps prepare, cutover, verify and cleanup.

After the hosts have baked (two healthy beats each), `<v1>` is hello's last good version.

## 3. Set v2

Repeat step 2 with the next release, `<v2>`, and its digest.

**Watch:** the same pages. On each host the other slot now serves, at `<v2>`, and the one
that served `<v1>` is stopped. When the hosts have baked, `<v2>` is the last good version.

## 4. A failing build: watch the automatic rollback

Set the desired version to `<v2>-fail-ready`, with the digest of its package. That build runs
the server with `--fail-ready`, so `/readyz` answers 503.

**Watch:**

- Host, `Last update` of the canary: `Verdict` failed, `Phase` after the commit, `Reason`
  `not_ready`. The host's own hook rolled it back: the slot of `<v2>` serves again, and Fleet
  shows `hello <v2>, healthy`.
- Rollout, section `hello`: `Bad versions` lists `<v2>-fail-ready`, and `Desired` is back at
  `<v2>`, the last good version. No other host was ordered the failing build.
- Audit: a `mark_bad` entry by the coordinator, with the reason
  `<v2>-fail-ready failed its release on enough hosts`.

One failed host is enough for the rollback: the entry sets `rollback_after_failed` to
`min_hosts = 1`.

## 5. Unassign with uninstall

Not built yet: the coordinator's `Services.Unassign` is komira-ai/hostkeeper#45, and the UI has
no control for it. When it lands, unassign hello from each host of the role:

```sh
hostkeeper ctl ... services unassign --node-id <host> --service hello --uninstall true
```

The daemon then runs hello's `uninstall` step, which stops both slots and removes the hook's
record. It then removes hello's placed units, its package directory and its step records.
Remove the entry from the rollout file afterwards.

**Watch:** Fleet. The host no longer lists hello.

## What the proof does not show

- A hand-off without a gap: hello restarts. tierstore's port forward is its own (design
  `docs/design/host-updates.md`, section 4.1).
- The macOS hosts: the daemon does not run launchd jobs yet (`docs/macos.md`).
- More than one platform in one rollout: see "One platform" above.
