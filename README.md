# hostkeeper

hostkeeper is the generic host lifecycle system: it installs, updates, rolls back,
health-observes and retires the services that run on a host. It knows no service by name. Each
service tells it what to do through a hook that ships in the service's own package.

It is made of two programs:

- **one daemon per host**, `hostkeeper daemon`, a single root process. Its config,
  `/etc/hostkeeper/hostkeeper.toml`, is the only trust root of the host: the coordinator, the
  release channel, the signers it trusts and its bounds. A service ships as a package, an
  attested archive holding `manifest.toml` (the hook command, its timeouts and its `[params]`),
  `bin/`, `units/` and optional `files/`. The daemon unpacks each version read-only under
  `/var/lib/hostkeeper/packages`, keeps `current` and `previous`, and links the units and files
  itself ([package format](docs/package-format.md)). It runs the hook (`prepare`, `cutover`,
  `verify`, `cleanup`, `rollback`) as root, each step in a transient systemd unit bounded by
  `RuntimeMaxSec` only. That is safe because the hook came byte for byte from the package that a
  trusted signer attested. The conformance suite runs in the service's own CI
  ([docs/conformance.md](docs/conformance.md)); nothing confines a hook at runtime;
- **one coordinator**, `hostkeeper-coordinator serve`, which assigns services to hosts and holds
  the desired version of each, and moves hosts to it stage by stage, each service by its update
  plan. Operators use it through the operator API, the generated `hostkeeper ctl`
  ([docs/ctl.md](docs/ctl.md)) and the web UI (`ui/`). A new host asks the coordinator to enroll
  it, and an operator approves the request. Macs are to be managed through a self-hosted MDM
  server; that part is not built ([macOS](docs/macos.md), [design](docs/design/hostkeeper-mdm.md)).

Every API is a protobuf service, and the command line is generated from those definitions.

**hostkeeper is pre-release.** There is no release yet. Two programs are built from source: the
one host program, `hostkeeper daemon`, which serves its admin socket (`Status`, `Abort`,
`ClearRecord`) and runs the orders the coordinator sends in its heartbeat replies (see
[Running the daemon](#running-the-daemon)), and the
update coordinator, `hostkeeper-coordinator serve`. The parts are being moved here from
[kbf](https://github.com/komira-ai/kbf), where they were developed first
([MIGRATION.md](MIGRATION.md)).

## What is built

| crate | what it holds |
|---|---|
| [`hostkeeper`](crates/hostkeeper) | the binary: `hostkeeper daemon`, `hostkeeper ctl`, `hostkeeper ci`, and the two plumbing verbs of its own update; it only forwards to `hostkeeper-daemon` |
| [`hostkeeper-daemon`](crates/hostkeeper-daemon) | the daemon: start-up and its lock, the order executor with its locks and try rules, the held halts, the root-only admin socket, and the enrollment and heartbeat loops |
| [`hostkeeper-config`](crates/hostkeeper-config) | the strict loaders of `hostkeeper.toml` and of a package's `manifest.toml`, the owner and mode check of a trusted file, and the name and path grammars |
| [`hostkeeper-hook`](crates/hostkeeper-hook) | the contract between the host and a service's update hook, and the conformance harness a service tests its hook with (feature `conformance`) |
| [`hostkeeper-steps`](crates/hostkeeper-steps) | the step runner: the step record and its resume, the dead-man, the layout of versions with `current` and `previous`, the seal of a version, and the transient systemd unit of one step |
| [`hostkeeper-package`](crates/hostkeeper-package) | fetching a package from the release channel within bounds, its size and digest check, and the offline check that a trusted signer attested it |
| [`hostkeeper-unpack`](crates/hostkeeper-unpack) | the unpack of a verified `.tar.zst` package into a sealed version directory, in one streaming pass under the archive bounds |
| [`hostkeeper-placed`](crates/hostkeeper-placed) | linking a package's units and files from `current` into their system paths, with `placed.json` written ahead |
| [`hostkeeper-observe`](crates/hostkeeper-observe) | the service observer: liveness from the service manager, the version confirmed by the running executable, and the health URL |
| [`hostkeeper-link`](crates/hostkeeper-link) | the daemon's heartbeat loop over ports: facts out, orders to the executor, verdicts resent until acknowledged; gRPC over mutual TLS |
| [`hostkeeper-enroll`](crates/hostkeeper-enroll) | how a daemon gets and keeps its host certificate: the P-256 key, the request, the signed poll, and renewal at half life |
| [`hostkeeper-launchd`](crates/hostkeeper-launchd) | the macOS side behind existing ports: launchd labels and plists, the `launchctl` port, service manager, unit control, process probe and the step launcher (`LaunchdLauncher`) ([macOS](docs/macos.md)) |
| [`hostkeeper-wire`](crates/hostkeeper-wire) | what the coordinator and a host exchange: the heartbeat and its reply, versions, host ids, role names and platforms, the certificate identity, the secret-file rule |
| [`hostkeeper-proto`](crates/hostkeeper-proto) | the protobuf services of the coordinator (`hostkeeper.agent.v1`, `hostkeeper.operator.v1`) and of the daemon's local admin socket (`hostkeeper.local.v1`), the command table of `hostkeeper ctl`, and the compatibility test |
| [`hostkeeper-ctl`](crates/hostkeeper-ctl) | `hostkeeper ctl`, the command line generated from those services: one command per RPC, over gRPC or the coordinator's HTTP/JSON gateway |
| [`hostkeeper-ci`](crates/hostkeeper-ci) | `hostkeeper ci set-desired`, the CI client a service's release job runs once it has verified the hostkeeper release ([docs/deploy/ci-client.md](docs/deploy/ci-client.md)) |
| [`hostkeeper-coordinator`](crates/hostkeeper-coordinator) | the update coordinator: the services and roles of a rollout file, the desired version of each service, the heartbeat endpoint, the operator endpoint, and the convergence of a fleet |
| [`hostkeeper-enrollment`](crates/hostkeeper-enrollment) | the coordinator's certificate authority (profile, certificates, key file, CA bundle) and its durable enrollment store |
| [`hostkeeper-oidc`](crates/hostkeeper-oidc) | the verifier of OIDC tokens (CI tokens) against an allow-list, with its issuer keys fetched over HTTPS |
| [`hostkeeper-tls`](crates/hostkeeper-tls) | TLS server configuration reloaded from certificate files, with its reload metrics |
| [`hostkeeper-ui-tests`](crates/hostkeeper-ui-tests) | the tests of the operator web app in [`ui/`](ui), run in a JavaScript engine against a fake gateway, and each call of [`ui/calls.json`](ui/calls.json) sent to the real coordinator with an operator's token; not shipped |
| [`hello`](examples/hello) | the example service: an HTTP server on loopback and its update hook, packaged for each release, which proves an install, an update and a rollback on the farm ([docs/hello.md](docs/hello.md)) |
| `hostkeeper-testkit` | helpers the tests share |
| [`hostkeeper-privileged`](crates/hostkeeper-privileged) | the tests that run as root against a host's real systemd, in `privileged.yml` only, after a merge to main, with the workload fixture `hostkeeper-fixture-hook` they drive, and the end-to-end update of a tierstore-shaped package through the real coordinator and daemon; not shipped |
| `coverage-gate`, `xtask` | the 99% coverage gate, and the CI steps and release verbs |

The operator web app (`ui/`) is static files: HTML, one stylesheet and JavaScript modules, with
no framework and no build step. Its pages (Fleet, Host, Rollout, Holds, Enrollment, Audit) call
the coordinator's HTTP/JSON gateway with the operator's token, each button one method call. The
coordinator serves it under `/ui/` of its private listener: the coordinator's container image
holds it at `/usr/share/hostkeeper-coordinator/ui`, which the coordinator serves when started
without `--ui-dir`, and `--ui-dir` names another directory
([docs/ui-serving.md](docs/ui-serving.md)).

The design is [docs/design/hostkeeper-design.md](docs/design/hostkeeper-design.md).
[docs/design/host-updates.md](docs/design/host-updates.md) remains the long-form reference for
the hook contract and the step record.

Every command of `hostkeeper ctl`, with its flags and exit codes, is in [docs/ctl.md](docs/ctl.md),
which a test regenerates from the protos and compares, so it cannot drift from them.
Enrolling a node, from its inventory record to its first heartbeat, with every refusal on the
way, is [docs/operations/enrollment.md](docs/operations/enrollment.md); a test parses each of
its `hostkeeper ctl` lines with ctl's own parser.

## Build order

The items B1 to B28 of section 12.2 of the design, by theme. "Built" means the code is on `main`.

- **Host daemon.** Built: B2 (the config and manifest loaders), B4 (the daemon and its admin
  socket), B8 (the service observer), B9 (the heartbeat loop), B13 (pruning to two versions),
  B22 (self-update of hostkeeper), B3 (`SystemdLauncher`, in `hostkeeper-steps`, which the daemon
  runs each hook step through), B20 (the verbs `abort`, `clear_halt` and `clear_record`, see
  [Operator verbs on the host](#operator-verbs-on-the-host)).
- **Packages and placement.** Built: B5 (fetch, verify, unpack, seal), B27 (placed-path
  linking), B28 (hookless force), B23 (the `install` and `uninstall` orders in the daemon, and
  the assignment of services by the coordinator: roles in the rollout file, and the operator
  API's `Services.Assign` and `Services.Unassign`, which are served).
- **Coordinator and rollout.** Built: B6 (the coordinator, its rollout file and operator tokens),
  B7 (holds, halts, abort, exclude, force with targets, set-desired refusals).
- **Enrollment.** Built: B16 (certificate authority, enrollment and renewal), B17 (approval).
  Planned: B21 (adopting hosts that run kbf's agent).
- **Operator surfaces.** Built: B1 (the protobuf services, every method of which has a handler:
  none answers `UNIMPLEMENTED`), B10 (generated `hostkeeper ctl`, gRPC beside the gateway), B24
  (the CI client), B25 (the UI).
- **Platforms and packaging.** Built: B11 (deb, rpm, unit, boot-image overlay). Partly built:
  B18 (macOS: `hostkeeper-launchd`, the daemon's Mac adapters, package check and claims, and a
  device run of the daemon under launchd after each merge; no update has run through it on a
  device).
- **Release.** Partly built: B14 (the release workflows and the release verbs of `xtask`; no
  release is published). Built: B12 (the privileged workflow with real systemd,
  `privileged.yml`), B15 (the privileged end-to-end proof,
  `crates/hostkeeper-privileged/tests/tierstore_shaped.rs`).
- **Clean-up.** Built: B19 (the wall-clock lint). B26 (the removal of the old per-service crate
  and its vocabulary) is this change.

## Running the daemon

`hostkeeper daemon` is the one root process of a host. It takes its configuration from flags
only, never from the environment:

| flag | default | what it is |
|---|---|---|
| `--config` | `/etc/hostkeeper/hostkeeper.toml` | the daemon config, owned by root and not writable by group or others |
| `--state-dir` | `/var/lib/updater/state` | a directory per service with its step record, params file and held halt generation, and the daemon's lock file `lock`; the directory of the service `hostkeeper` also holds its binaries (`bin/`) and its list of bad versions (`bad-versions`), see [Updating hostkeeper itself](#updating-hostkeeper-itself) |
| `--packages-dir` | `/var/lib/hostkeeper/packages` | a directory per service with its version directories and the links `current` and `previous` |
| `--admin-socket` | `/run/hostkeeper/admin.sock` | the admin socket, mode 0600 in a directory of mode 0700, served to uid 0 only |
| `--notify-systemd` | off | once the boot work is over, send `READY=1` to the notify socket systemd sets for a `Type=notify` unit; without that socket the daemon exits 1 |
| `--heartbeat-secs` | `15` | seconds between two heartbeats, at least 1; the end of a running order is reported within a second, without waiting for the period |
| `--trusted-root-max-age-days` | `90` | days after which the Sigstore trusted root that package attestations are checked against is reported `trusted_root_stale` in the admin `Status`, at least 1; a signal only, package checks go on (see [docs/deploy/verify-a-release.md](docs/deploy/verify-a-release.md#the-trusted-root-on-a-host)) |

At start it loads the config first: a config that does not load stops the daemon with an error
naming the key, and nothing on the host changes. It then takes the lock on the state directory
(one daemon per state directory) and does its boot work. It removes the registration socket
kbf's updater served (`updater/register.sock` beside the admin socket's directory, `/run/updater`
by default), removes what a crashed package fetch or unpack left, finishes or undoes each switch
of `current` a step record says was begun, and relinks the placed paths that differ from `current`.
It then reads every step record against the host's boot id (kept in `.boot-id` in the state
directory), runs each update left unfinished by a crash or a reboot to its verdict, restarts the
post-success watchers, runs each service's `[hook] boot` step from `current`, and only then tells
systemd it is ready. Only then does it start its two loops, the enrollment loop first: the
enrollment loop gets the host's certificate from the coordinator (a P-256 key made on the host,
a request an operator approves, renewal at half life) and keeps it under `/etc/updater/pki`; the
heartbeat loop reads the certificate under that directory's `current` link at every heartbeat,
and while there is none it says once what it waits for and sends nothing. It then serves the
admin socket, until an update of hostkeeper itself asks it to exit so that its unit starts the
new binary. This build fetches, verifies and unpacks packages, runs each step of a hook in a
transient systemd unit of its own (the hook's standard error goes to the daemon's journal, under
the unit's name), stops, starts and reloads units through `systemctl`, and observes each
service for `Status`. Its launcher does not yet run an additive step (`[hook] boot`, `uninstall`) or the self-update
protocol probe (each fails without starting). Once the host is enrolled, its heartbeats go to the
config's `coordinator` over mutual TLS, pinned to `ca_sha256` (JSON, then gRPC once the coordinator
advertises it), and the coordinator's orders come back in the replies.

| exit status | meaning |
|---|---|
| 0 | the daemon exited for its own update (see [Updating hostkeeper itself](#updating-hostkeeper-itself)) |
| 1 | the daemon could not start or serve for another reason (the state directory, the admin socket) |
| 2 | a usage error: an unknown flag or subcommand |
| 3 | the daemon config did not load |
| 4 | another daemon holds the state directory's lock |

### Operator verbs on the host

Root on the host has two verbs besides `Status` on the admin socket, because they must work with
the coordinator down or a hook hung: `abort` and `clear_record` (the admin service's `Abort` and
`ClearRecord`). The socket carries nothing else: no `clear_halt`, and nothing that installs,
updates or switches a version.

- `abort <service>` stops the service's running step and ends its update. The daemon asks the
  update's runner to launch nothing more (no `rollback`, no `cleanup`), stops the unit of the
  step that runs by name, trying three times, and the update ends `Halt` with the reason
  `aborted_by_operator`. It is the only end of a `prepare` declared unbounded (`prepare = 0`)
  that does not return. When every try to stop the unit fails, the update ends `Halt` with the
  reason `unstoppable`, the daemon logs an alert (`alert=unstoppable_unit`) naming the unit, and
  the unit may still be running: stop it by name (`systemctl stop <unit>`). A step the daemon's
  dead-man could not stop at its bound keeps the service locked (`Status` shows the update under
  way with the reason `unstoppable_unit`), and `abort` is what ends it and lets the service go.
  With no step running, `abort` is refused `nothing_running` and changes nothing.
- An aborted update ends in a halt: the service takes no other order until the coordinator lifts
  the halt (an operator's halt clear, which reaches the host as a halt generation greater than
  the one it holds, in a heartbeat reply or a `clear_halt` order). A coordinator's `abort` order
  is answered the same way as the local one, and takes no farm slot; this build's heartbeat loop
  carries only `update` and `force` orders.
- `clear_record <service>` sets aside a step record that does not read (the record problem
  `RECORD_PROBLEM_UNREADABLE` in `Status`), so that orders run for the service again: the file is
  renamed `step-record.json.corrupt` in the service's state directory, for the operator to read,
  and nothing else changes (not the params file, the held halt or the package directory). A
  record that reads, in any state, or that a newer hostkeeper wrote, is refused `record_reads`.
  A unit that the lost record had started is not stopped, since nothing names it any more: find
  it (`systemctl list-units 'updater-<service>-*'`) and stop it by name before the next order.

## Force on a host

A `force` order (design section 5.6) switches one service to a version with no hook, because a
broken hook is often the thing that is broken. It names the service, the version, the package
digest and a halt generation. **A force cuts running work**: a hooked step of the service that is
running is stopped and its units stop without a drain, the one exception to "drains never cut
work". It takes the service's lock only, never the step lock, so it never waits for another
service's step.

Before anything changes the daemon refuses a force of `hostkeeper` itself (`self_service`), an
order whose service, version or digest breaks its grammar (`invalid_order`), and a resend whose
try conflicts with the step record (`try_conflict`, `stale_try`); a resend of a force that
ended is answered from the record. A force is not refused for a running hooked update of its
service (it stops it), for a standing halt (an order whose halt generation is above the held
one lifts the hold), or for a version below the running one: the host keeps no floor of its
own.

| Step | What changes | What a crash there leaves | What resumes |
|---|---|---|---|
| 1. Have the package | the package is fetched, verified, unpacked and sealed if the host does not hold it under a seal of the order's digest | at most a staging directory, removed later | the resend starts again |
| 2. Read the manifest | nothing | nothing | the resend starts again |
| 3. Check the paths against `placed.json` | nothing | nothing | the resend starts again |
| 4. Flag the running hooked step | the step record of the service gets `forced` | a flagged record; its runner, or the force, closes it `forced_over` | the resend flags it again, which writes nothing |
| 5. Stop that step's unit by name | the unit stops (tried twice) | a stopped unit | the resend stops it again |
| 6. Wait for the runner to yield | the record is closed `Failed`, `forced_over`; no `rollback` or `cleanup` runs | the closed record | the resend goes on to step 7 |
| 7. Take the service's lock | nothing on disk | nothing | from step 7 |
| 8. Write the force record | the step record becomes the force's journal, phase `started` | the journal | the start pass, or the resend, runs steps 9 to 16 from the top |
| 9. Stop the units the version left runs | its `run` units, and those `placed.json` holds enabled, stop | stopped units | the same, again from step 9 |
| 10. Unlink what the target does not list | units are stopped, disabled and unlinked, files unlinked, `daemon-reload` | some links gone, `placed.json` saying which | again from step 9; an absent link is no error |
| 11. Switch `current` | `previous` names the version left, `current` the target | either link whole | again from step 9; a switch to the version `current` names changes nothing |
| 12. Link the target | its units, `WantedBy=` links and files are linked, written ahead in `placed.json` | links that `placed.json` lists | again from step 9; a link already right is left |
| 13. `daemon-reload` | the service manager reads the units | a reload owed in `placed.json` | again from step 9 |
| 14. Start the target's `run` units | the units start | started units | again from step 9 |
| 15. Wait until each is active | each must be seen active once within `force_start_secs`, polled every second | nothing more | again from step 9 |
| 16. Record the verdict | phase `verdicted` with `Succeeded`, or `Failed` with its reason | the verdict | nothing: a resend is answered from it |
| 17. Report | the verdict, with the order's id and try, is what the daemon's status gives the heartbeat until the next order | the verdict | nothing |
| 18. Prune | when `current` moved, the versions but `current` and `previous` are deleted (see below); a failed prune is an alert and leaves the verdict | a version renamed to `.stage-prune-*`, or a seal with no tree, both removed later | nothing; the next confirmed switch prunes again |

The reasons a force reports: before anything changed, `Failed` before the commit with the
reason of what refused it (`digest_mismatch`, `package_not_found`, `no_space`, `package_failed`,
`manifest_mismatch`, `daemon_too_old`, `path_conflict`), or `unstopped` when the running step's
unit could not be stopped; once the host began to change, `Failed` with `link_failed` (steps 9
to 12, before the commit up to the switch and after it from then on), `daemon_reload_failed`
(step 13) or `start_failed` (steps 14 and 15, blamed on the release). After a failure the host
stays where it is: nothing is rolled back and the daemon does not try again. The repair is a
second force, for instance to the version `previous` names, which is on the host and needs no
fetch.

## Two versions on a host

A host keeps two versions of each service: the one `current` names and the one `previous`
names, so a rollback to `previous` is a local switch with no fetch, and anything older is
fetched again. No setting changes this. After a confirmed switch (an update that ends
`Succeeded`, `promotion_pending` or not, or a force that moved `current`, whatever its verdict)
the daemon deletes every other version directory of the service, with its seal, under the
service's lock and no other. It keeps the target of the switch, and anything a record says is
still in use: the release an unfinished update replaces, the target of a switch under way, and
a version `placed.json` still lists as `pending`. A version staged by an update that did not
switch stays until the next confirmed switch, so a retry needs no fetch. Nothing is pruned
after a failed, deferred, halted or `forced_over` update, at start, or to make room: the disk
need is two trees per service plus the one being staged. When `current` or `previous` is
missing or names no version, nothing is deleted. A version is renamed to `.stage-prune-*`, then
its seal is removed, then the tree, so a crash never leaves a half-deleted version; the next
prune, or the unpack's own clean-up, finishes the job. A removal that fails is an alert and
never changes the verdict; the next confirmed switch tries again.

## Updating hostkeeper itself

hostkeeper updates itself with an `update` of the service named `hostkeeper` (design section
6.6). A `force` or a `force_rollback` of it is refused (`self_service`). The update goes through
five states, and the step record of the service `hostkeeper`
(`<state-dir>/hostkeeper/step-record.json`) says which one it is in:

| state | what is true | what the daemon answers to the order |
|---|---|---|
| ordered | the order arrived; nothing is staged | while another service's step record is `pending`, `running`, or between two steps, or cannot be read: `busy`, and nothing is written. Otherwise the daemon waits for the step lock (a running step of another service is never interrupted) and goes on |
| staged | the new binary's protocol was read, the package's hook steps ran, and the new binary is in `<state-dir>/hostkeeper/bin/versions/<version>` | nothing yet: the old binary never reports the update succeeded |
| awaiting_restart | the record is `done` at `cleanup` with `awaiting_restart`, written before `<state-dir>/hostkeeper/bin/current` is switched to the new binary; the daemon then exits with status 0 and its unit starts `current` | `Deferred`, reason `awaiting_restart` |
| confirmed | the new binary started, found the record waiting for itself with `current` naming it, and rewrote it `Succeeded`; the `Deferred` records of other services whose reason is `updater_old` or `daemon_too_old` are removed, so their next order runs | `Succeeded`, from the record |
| fell back | the new binary did not start: after three failed starts in 300 seconds the fallback unit ran the previous binary's `hostkeeper self-update-fallback --dir <state-dir>/hostkeeper/bin`, which points `current` at the previous binary again, adds the version to the local list of bad versions, and rewrites the record `Failed` after the commit, of class release, reason `self_update_fallback`. A start of any binary that is not the one the record waits for settles it the same way | `Failed` (`self_update_fallback`), from the list, for that version and every later order of it |

The local list of bad versions is `<state-dir>/hostkeeper/bad-versions`, a JSON object
`{"schema":1,"versions":["1.5.0"]}`, mode 0600, keeping the last 64 versions. hostkeeper never
removes a version from it; a list that cannot be read refuses every update of hostkeeper
(`bad_versions_unreadable`) until it can. The coordinator stops ordering a version to a host that
reported it failed.

Before anything is staged, the daemon runs the new binary's `hostkeeper self-update-protocol`
(in a transient unit, at most 10 seconds, at most 64 bytes of output read), which prints the
binary's self-update protocol and nothing else. **For release authors:** a daemon of protocol
`N` accepts a new binary of protocol `N` or `N + 1` only, because it must be able to fall back
from it: the previous binary must read every file the new one writes. A release that changes
what hostkeeper writes therefore goes out in two steps: the release of protocol `N + 1` adds the
readers and still writes only what protocol `N` reads; only the release after it writes the new
formats. A binary of another protocol is refused `Failed` before the commit, of class release
(`protocol_unsupported`); a probe that fails, times out, or prints anything but one line of a
decimal number is refused `Failed` before the commit, of class environment
(`protocol_unreadable`); a release without `bin/hostkeeper` is refused `Failed` before the
commit, of class release (`self_binary_missing`). In each case nothing on the host changes.

hostkeeper is licensed under [Apache-2.0](LICENSE). How to contribute is in
[CONTRIBUTING.md](CONTRIBUTING.md), and how to report a vulnerability is in
[SECURITY.md](SECURITY.md). The rules every change to this repository is written and reviewed
against are in [AGENTS.md](AGENTS.md).
