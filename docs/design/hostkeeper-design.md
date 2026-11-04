# hostkeeper design

Status: design, for the maintainers' comments. This is the single current design for hostkeeper and the entry point for every other hostkeeper document.

hostkeeper is the generic host lifecycle system: it installs, updates, rolls back, health-observes and retires the services that run on a host. kbf, tierstore and jobscheduler hosts are installed and updated by it. It knows no service by name. Each service tells it what to do through a hook that ships in the service's own release.

## What changed in this revision

- **One process per host, and it is root.** No unprivileged agent, no root part, no socket protocol between them. The daemon speaks mutual TLS to the coordinator, heartbeats and executes orders. Local administration is the generated CLI on a root-only socket.
- **No rights filtering.** Hook user, capabilities, write paths, polkit rules, the widening check, approvals and their keys, and the `approve` verbs are deleted. What decides is the release signature.
- **Compiled-in verbs.** An order is a verb plus typed arguments, never a command, a path, a URL or a script.
- **One root-owned services file** per host says which services run and who signs each; the coordinator cannot change it (open question Q3).
- **A simple coordinator,** and a **UI** on the same API as the CLI (section 5.10, designed, not built). Other cuts and keeps are in Q6.

## How to read this document

Each statement is tagged where it matters:

- **Built** means the code is on `main` of this repository, with a path.
- **Open PR** means the code exists in a pull request that is not merged: PR #3 (the protobuf crate) and PR #5 (the coordinator import).
- **In kbf** means the code exists today in the kbf repository and has not moved yet (the hostkeeper crates on `main` are an import of kbf's updater at one release tag; kbf `main` has moved past it).
- **Designed** means nothing implements it yet. Section 12 says which build item does.

Counts and line numbers are not carried here, because they rot. A test name in code font is a test that exists on `main` today unless tagged **(to build, Bn)**, **(open PR #n)** or **(in kbf)**. Places where this document deliberately differs from a ruling are marked **Deviation**.

## What this document replaces

| Document | Status after this one |
|---|---|
| `docs/design/host-updates.md` | Superseded as the entry point. It stays as the long-form reference for the hook contract (its section 3.2), the step record and dead-man (3.5), and the tierstore and jobscheduler hooks (4.1, 4.2). Void: the declaration with rights (2.4), "Which binary runs" trust and rights (3.3, 3.4), the registration socket (2.1 to 2.3, 3.7), per-service slots, and its migration order (8). Where it disagrees with this document, this document wins. |
| An earlier draft of the hostkeeper design, never published | Superseded entirely. |
| `README.md`, `MIGRATION.md`, `AGENTS.md` | Remain. `README.md` and `MIGRATION.md` still describe the agent, root part and rights crate and are updated with the first build item that changes them. |

---

## 1. Problem

A farm of machines runs several long-lived services. Today each service has found its own way to be updated, and the ways differ in the ways that hurt:

- Update scripts run per release, are written for that release, and are tested only by running them on the farm.
- A service with live work is updated by cutting that work, or by a script that waits for it in a way nobody can see.
- Two services updated at once on one machine, or on two machines that depend on each other, can take the farm down together.
- A broken release of the system that builds the releases cannot build its own fix.
- A new machine is installed by hand, with a setup tool per product.
- Nothing says in one place which code may run as root on a host and who vouched for it.

hostkeeper replaces all of that with one daemon per host, one coordinator per farm, and a small contract that every service meets.

## 2. Goals and non-goals

### 2.1 Goals

1. **One updater that knows no service.** Per-service behaviour lives in a hook shipped in the service's own release.
2. **A fixed, small contract.** Five steps, four exit codes, one JSON line. Every step is safe to run twice. A hook is a subcommand of the service's own binary.
3. **Trust is the release.** Code runs as root on a host only if it came from a release whose digest and attestation verified against the signer pinned for that service in the host's root-owned services file. There is no filter between the root daemon and the hook.
4. **One live update at a time across the whole farm,** every service and every role. No setting widens it.
5. **A release never runs code from the release it leaves,** and a bad release can be put back with the coordinator down.
6. **Work is never cut to finish an update.** A service with live work drains in its own `prepare`. The default is to wait with no deadline; a deadline is a per-service option.
7. **Enrollment and remote install.** A host that boots a hostkeeper image appears as pending; an operator approves it; the coordinator installs the services the host's services file names through the same path that updates them.
8. **macOS hosts follow the same flow** (section 5.9).
9. **Service health is observed, not reported by the service.**
10. **Every failure has a stated outcome and a test.** Section 8.

### 2.2 Non-goals

- Not a package manager, a configuration management tool, a scheduler, or a high-availability coordinator (one coordinator, section 11). Not a Windows product.
- No protection of a host from its own root, and none of the release signer's repository (section 5.1 says what a compromise of each gets).
- No remote way to widen what a hook may do. There is nothing to widen: hooks run as root, and the only way to put code on a host is a signed release of a service the host's services file names.
- No network boot in scope (section 12.1).

## 3. The design in one picture

![Components and trust boundaries: coordinator, one root daemon per host, hook, service, signer, operator and CI, with the trust boundary drawn at each crossing and what each side may send](diagrams/components-and-trust.svg)

- **The coordinator** (`hostkeeper-coordinator serve`) holds the desired version of each service, the farm's membership and the operator's marks (excluded hosts, halts, bad versions). It decides, per heartbeat, whether to order a host to do something. An order is a verb and typed arguments. It also serves the API, the generated CLI's endpoint and the UI (section 5.10).
- **The daemon** (`hostkeeper`) runs on every host as root. It connects out to the coordinator over mutual TLS, heartbeats, and executes orders. It listens on no network port. Its local interface is a root-only unix socket used by the generated CLI. It executes only the verbs compiled into it (section 4.1).
- **The hook** is an executable in a service's release. The daemon runs it, one step at a time, as root, in a transient unit that exists for timeout, kill and log capture (section 5.3).
- **The services file** (`/etc/hostkeeper/services.toml`) is root-owned and says which services this host runs, each service's binary, who signs it (repository and workflow), its step timeouts and its `min_version`. The coordinator cannot change it (section 4.7).

Trust is placed in three things: the release artifact whose digest and attestation verified against the pinned signer, the root-owned services file, and the coordinator's authority to order the compiled verbs. Section 5.1 states what each compromise gets.

### 3.1 What is built

| Piece | State | Where |
|---|---|---|
| Hook contract, exit codes, last-line JSON, params file, next-step table, conformance harness | Built | `crates/hostkeeper-hook` |
| Declaration with rights, loader, widening check, approval store and gate, `approve` command, transient-unit launch with user, capabilities and sandbox properties | Built, and the build order deletes or shrinks it: the services file replaces the declaration, and the widening check, gate, approval store, `approve` command, account and reserved-path checks and the rights properties of the launch go (B2, B3) | `crates/hostkeeper-rights` |
| Step runner, step record, artifact seal, dead-man, self-update layout (link switching, staging, unit generation helpers) | Built | `crates/hostkeeper-steps` |
| Test scratch directories | Built | `crates/hostkeeper-testkit` |
| 99% line and branch coverage gate, workflow lint, layering and hygiene tests | Built | `crates/coverage-gate`, `crates/xtask` |
| Coordinator (HTTP/JSON over TLS), wire types, OIDC verifier, TLS reload | Open PR #5 | `crates/hostkeeper-coordinator`, `-wire`, `-oidc`, `-tls` |
| Protobuf APIs, descriptor compatibility check (today including the root-part and approval services that this revision removes) | Open PR #3 | `crates/hostkeeper-proto` |
| Root program (boot pass, `Locked` error and lock file, `Update` and `Status`), agent loop, enrollment client, renew | In kbf (`kbf-updater root` and `agent`, two processes), to be imported and merged into one daemon (B4, B9) | |
| Release fetch, verify, stage | Designed (kbf has it only inside its own installer) | section 5.5 |
| Services file, compiled verbs, local admin socket, kept-release activation, pin, enrollment surface, remote install, uninstall | Designed | sections 4, 5 |
| The UI | Designed | section 5.10 |
| macOS daemon | Designed | section 5.9 |
| Packaging, privileged workflow, release automation | Designed | sections 4.6, 12 |

The imported crates keep kbf's wire and disk names on purpose: exit codes 0/1/75/76, the `update-hook` default argument, the reserved `updater.*` parameter keys, `/var/lib/updater/{state,hooks,releases}`, unit names `updater-<service>-<step>-<generation>`, step record schema 1. Tests pin them (`constants_match_kbf_at_the_tag`, `reads_step_record_written_by_kbf_tag`). Renaming a stored format in place is forbidden; formats change by adding fields only.

### 3.2 Naming

Names that carry the product move to `hostkeeper`: binaries, the `hostkeeper.*.v1` protobuf packages, `/run/hostkeeper/`, `/etc/hostkeeper/`, the macOS reverse-DNS prefix. Names that are product-neutral and already stored on disks stay `updater` (section 10).

## 4. Configurations

### 4.1 A Linux host

One unit, `hostkeeper.service` (root, `Type=notify`), one binary. One local socket, `/run/hostkeeper/admin.sock`, mode 0600, in a directory of mode 0700: root only, for the generated CLI. Protobuf on the socket like every hostkeeper API.

**The verbs** are code. The coordinator can send exactly the first eight rows; the local CLI can run those and the last two (marked local). There is no other way to make the daemon do anything:

| Verb | Typed arguments | Meaning |
|---|---|---|
| `update` | service, version, release digest, try | fetch, verify, stage, run the five steps; refused below the running version and below `min_version` |
| `install` / `uninstall` | service (+ version, digest for install) | add a service the services file names; or run its `uninstall` hook and remove its release tree and step records. The daemon never deletes data directories; that is the hook's business |
| `force_rollback` | service, version, release digest | an update to a lower version, still at or above `min_version` |
| `activate` | service, version, pin flag | switch to a release already kept on the host; no fetch, no trial |
| `pin` / `unpin` | service | hold or release a service |
| `abort` | service | stop the running step and end the order `Halt` |
| `clear_halt` | service, halt generation | lift a halt |
| `status` | none | report; also sent with every heartbeat |
| `fetch` (local) | service, version, digest | stage a release without switching |
| `clear_record` (local) | service | clear a corrupt step record |

Updating hostkeeper itself is `update` for the service named `hostkeeper` followed by the self-update follow-on (section 6.6).

An order never carries a command, a path, a URL or a script. Release location, binary name and hook arguments come from the services file (the entry's `release_url`, `binary` and `hook_args`); the order gives a service name that must be in it, a version that matches the version grammar, and a digest.

Per service on the host: its entry in the services file, a release tree `/var/lib/updater/releases/<service>/<version>/`, a step record directory under `/var/lib/updater/state/<service>/`, and a hook record directory named by the entry's `state_dir`.

### 4.2 A macOS host

One launchd daemon, `com.<reverse-dns>.hostkeeper`, as root. Paths follow macOS conventions. Section 5.9 states what differs.

### 4.3 The coordinator

One process with two listeners: a **heartbeat listener** (mutual TLS, client certificate chaining to the farm CA; host identity from the certificate only) and one **private listener** (TLS, bound to the private network, off the public one) that serves the operator API, the generated CLI's endpoint and the UI (section 5.10). On it a CI OIDC token may set a desired version for services its allow-list entry names; every other action needs an operator token. A token either operates or does not; there are no per-token grants.

Operator tokens are one per person, listed by name in a tokens file the coordinator reads, each revocable and replaceable without a restart; every action records the token's name in the audit log (an append-only file, `<ops-file>.audit`). A shared anonymous token is not supported (**Designed, B6**; PR #5 has one token today).

State: the desired file, the ops file and its halts file, and the enrollment store, all files written whole by temp file, `fsync` and rename (section 10). There is no database. The coordinator's own list of services is a **rollout file** (`--rollout-file`; not the hosts' services file): for each service its `version_scheme` (`semver` by default), canary, bake and backoff settings, and an optional copy of `min_version` used only to refuse a bad `set-desired` early. It has no built-in service. The hosts' services file is the authority for what may run: the heartbeat lists the services each host's file names, an order is made only for a service in that list, and the daemon's own `min_version` check decides. The rollout file's `min_version` is the operator's copy and can drift; a drift is caught by the daemon, never trusted.

### 4.4 The CA

Designed. Certificate issuance is a module of the coordinator, not a second process (Q5 asks whether it should stay a separate process). It signs two things: a host client certificate (7 days) and the coordinator's server certificate. It enforces its own profile: P-256 keys only, subject `host:<id>`, the matching extended key usage, a validity cap, and one certificate per request id (section 5.7).

### 4.5 A CI client

A release job in a service's repository downloads the hostkeeper release, verifies its provenance (checksums, attestation, signer workflow, attested source commit equal to the pin), and only then requests an OIDC token and runs `hostkeeper ctl releases set-desired --service X --version V`, then polls convergence.

### 4.6 Packages

Linux: one deb and one rpm carrying the unit and a default `services.toml` with only the `hostkeeper` entry; the daemon runs as root and needs no service account. macOS: a signed pkg. Both publish into the operator's shared package repositories. Designed.

### 4.7 The services file

`/etc/hostkeeper/services.toml`, root-owned, not writable by group or others, installed with the hostkeeper package or boot image and editable by local root. It is the one place that says which services this host runs and who may sign them. **Designed format**: the loader on `main` reads the older per-service declaration with rights, not this; B2 replaces it.

```toml
[service.tierstore]
binary = "tierstore"
hook_args = ["update-hook"]
state_dir = "/var/lib/updater/hooks/tierstore"
min_version = "1.4.0"
boot = "restore"
verify_deadline_secs = 1600
promotion_ceiling_secs = 3600
cleanup_retries = 3
max_release_bytes = 536870912
observe_units = ["tierstore@9301.service", "tierstore@9302.service"]   # observed (5.8) and restarted by activate; grants the hook nothing
health_url = "http://127.0.0.1:{port}/healthz"
release_url = "https://<release channel>/tierstore/{version}.tar"
[service.tierstore.signer]
repository = "komira-ai/tierstore"
workflow = ".github/workflows/artifacts.yml"
[service.tierstore.timeout_secs]
prepare = 0     # drain: no deadline (the default stance, section 8.9)
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300
[service.tierstore.params]   # opaque scalars handed to the hook in its params file
```

`prepare = 0` is the unbounded form and is valid for `prepare` only; every other step needs a bound from 1 up. Every table refuses unknown keys. `verify_deadline_secs` must be at least the verify and rollback bounds together. Optional `fetch_timeout_secs` and `attestation_timeout_secs` raise the fetch bounds (defaults 900 and 60). An entry that fails a load rule makes that one service unmanaged, with an alert; the rest loads. There is no `user`, `capabilities`, write-path or polkit field: a hook is root (section 5.3). `hostkeeper` itself is an entry like any other, with its own signer.

The file is larger than the minimum (name, binary, signer, timeouts, boot behaviour, `min_version`, opaque params) because an order can carry no URL or path: `release_url` says where a release is; `hook_args` and `state_dir` say how the hook is called and where its record lives; `max_release_bytes`, `verify_deadline_secs`, `promotion_ceiling_secs` and `cleanup_retries` are bounds kbf already has; `health_url` and `observe_units` are for observation (5.8) and `activate`. None of them lets a release or the coordinator do more than the signer's code can already do.

## 5. How each operation works

### 5.1 Who may do what, and what a compromise gets

| Actor | Credential | May |
|---|---|---|
| Daemon | host client certificate (7 days) | heartbeat, renew, execute the verbs on its own host |
| Coordinator | server certificate chaining to the CA pinned on the host | order any verb for any enrolled host; supply no command, path or URL |
| Operator | personal token | exclude, include, label, clear halt, force, mark and unmark bad, approve enrollment, assign services the host's file names |
| CI | OIDC token, single-use `jti` | set the desired version and read convergence, only for services its allow-list entry names |
| Local root | uid 0 on the host | everything, including editing the services file |
| Release signer of service S | the pinned repository and workflow | publish the releases (binary and hook) that run as root on hosts that run S |

**The daemon checks every order itself,** from its own files, because the coordinator can order anything: the service is in the services file; the version and digest are well formed; the target is at or above the entry's `min_version`; it is not below the running version unless the verb is `force_rollback`; no pin or hold (only `unpin` and `activate` naming the pinned release are accepted); no unfinished record of a different release (`busy`); no standing halt. It does not know the coordinator's bad marks, canary or the single live update. The release is verified against D and the pinned signer regardless (section 5.5).

**Threat model, plainly.**

- **A compromised daemon is root on that host.** It is root's own program. Nothing here contains it.
- **A compromised coordinator, or a stolen operator token, can order any verb on any host.** In detail: `update` and `force_rollback` between signed releases of services the host runs, at or above `min_version`; `install` of services the host's file names; `uninstall` of them, which runs the service's signed `uninstall` hook and deletes its release tree and step records; `activate` of a kept release; `pin` and `unpin`; `abort`, which ends a running step and halts the order (a denial of service); `clear_halt`; update of `hostkeeper` itself to any signed release at or above its `min_version`; and, at the coordinator, exclude, halt and approving a rogue enrollment (which receives fleet information and can hold the single live-update slot). It cannot run code that was not signed by a pinned signer, add a service or a signer, or lower `min_version`: those live in the services file. A downgrade to an older signed release is bounded only by `min_version`, which the operator raises (as local root, or by the path Q3 decides) when a version must never run again. The CA key sits with the coordinator (Q5), so a coordinator compromise includes certificate issuance. If Q3(b) is chosen, the services-release signer, which is the `hostkeeper` signer, can also add a service and its signer on every host, and a compromised coordinator alone still cannot.
- **A compromised release signer** (the repository workflow pinned for service S) is root on every host that runs S, by shipping a hook. A root hook can touch any file, so this is root-equivalent on those hosts for every service. For `hostkeeper` itself that is every host.
- **A stolen CI token** can set the desired version of the services on its allow-list, to a version that has a signed release.
- **A compromised release channel** can only serve bytes; the digest and attestation checks refuse any that the signer did not sign.
- **The daemon is the only process that parses network data, and it is root.** That is the coordinator's messages, the release bytes from the channel, the attestation bundle and the release archive. What limits it: it listens on no port and connects out; the coordinator's certificate must chain to the CA pinned on the host (section 5.7), so a message needs the CA key or the coordinator; messages are protobuf decoded by Rust code with size caps and a small fixed message set; the archive is parsed only after its digest equals the one in the order and the attestation verified. What does not limit it: the release channel and the attestation bundle are not CA-bound (the attestation is parsed before it verifies, the channel's TLS is the web's), nothing sandboxes the daemon, and the TLS, protobuf, attestation and archive parsers read bytes from parties that may be compromised. A memory-safety bug in one of them would be root on the host. We claim no more than that.

### 5.2 One update, end to end

![Update sequence across CI, coordinator, daemon, hook and service, including release verification, each step as a transient unit, and the drain inside prepare](diagrams/update-sequence.svg)

1. CI sets the desired version of service S to V, with the release digest D that CI computed from the artifact it built and attested. The coordinator refuses, as an early check, a V below its copy of the service's `min_version`, a V it cannot parse, a V marked bad, and a V it already holds with a different D (a re-publish with different bytes is a new version).
2. At the next heartbeat of a host that is behind, `decide()` (section 5.4) checks its rules and, if every one passes, the reply carries the order `update S V D try`. `try` is assigned by the coordinator as one more than the highest try it has seen for that order, so a restarted coordinator never re-issues a lower one.
3. The daemon runs its own checks (section 5.1).
4. The daemon fetches the release, verifies it against D and the attestation, stages it as a read-only tree and writes the seal (section 5.5). Fetch and verify have their own deadlines and do not use the dead-man.
5. The step runner writes the step record, then runs `prepare <staged-release>`, `cutover`, `verify`, `cleanup` in order, each as a transient unit running as root (section 5.3), each preceded by a re-check of the release tree against its seal. The hook is the new release's hook.
6. After each step the runner reads the exit code and last stdout line, and `next()` (a pure function, `crates/hostkeeper-hook/src/next.rs`) says what follows. Failure before commit runs `rollback` and reports Failed. Deferral runs `rollback` for what `prepare` did and reports Deferred.
7. The verdict goes in the step record and in `last_update` in the next heartbeat.
8. If the last `verify` said `promotion_pending`, a watcher keeps running `verify` until it passes without the flag, or the promotion ceiling passes (Failed after commit), or a new order replaces it.

A release cannot change the services file: signer and bounds come only from the host's file. The step contract:

| Step | Argument | Meaning |
|---|---|---|
| `prepare` | staged release directory | check preconditions, drain, stage anything that can be undone |
| `cutover` | none | make the new release serve; if this makes the old release unrecoverable, say `reversible:false` here |
| `verify` | none | observe; may be repeated; success means the new release is serving |
| `rollback` | none | return to the state before `prepare`, or say `reversible:false` |
| `cleanup` | none | remove what is no longer needed; if the irreversible act can wait, it belongs here |

Exit codes: 0 success, 1 failure, 75 deferred (nothing irreversible happened), 76 halt (no version can repair this). A signal or timeout is failure. The last non-blank stdout line is one JSON object `{class: release|environment, reason, retry_after_secs, reversible, promotion_pending, detail}`; a missing line is `{}`; a non-boolean `reversible` reads as false. The updater, not the hook, assigns the phase (`before_commit`, `gate_release`, `gate_environment`, `after_commit`, `halt`).

**Commit point.** The update commits at the first step whose line says `reversible:false`. A hook that never says so never commits: a failed `verify`, or a failed `cleanup` that states `reversible:true`, is rolled back. After a commit, a failed `verify` still selects `rollback`, and the verdict is `Failed(after_commit)` when it completes; if `rollback` itself fails the verdict is `Failed(after_commit)` when its line says `reversible:false` and Halt otherwise (`next.rs`; test `a_failed_verify_rolls_back_and_cleanup_never_runs`). A cleanup that fails without stating `reversible` is retried forward, never rolled back.

All five steps use the hook in the **new** release. The **previous** release's hook runs `rollback` only if the new hook cannot start at all. A hook's record must be readable one version back: a contract rule, not yet checked by the conformance suite (**to build**, B3).

Parameters: the daemon writes one TOML file per update and passes it to every step: the entry's opaque `params` plus reserved keys `updater.staged_release`, `updater.previous_release`, `updater.target_version`, `updater.previous_version`, `updater.artifact_digest`, `updater.promotion_ceiling_secs`. An entry may not set an `updater.*` key; a hook ignores unknown `updater.*` keys.

### 5.3 How a hook runs

A hook runs as root. Each step is a transient unit, `systemd-run --system --wait --collect --pipe --unit=updater-<service>-<step>-<generation>`, with `RuntimeMaxSec` set to the step's timeout (`infinity` for an unbounded `prepare`) and nothing else: no user, no capability set, no sandbox property, no `ReadOnlyPaths` or `ReadWritePaths`, no polkit rule. The unit exists for three reasons only: the timeout (the dead-man stops the unit by name), the kill (the whole cgroup goes), and the capture of the exit code, last stdout line and log. On macOS the same three come from a launchd job (section 5.9).

Why there is no filter. A root daemon that launches a root hook cannot meaningfully limit the hook: any allow-list of what a root program may do is only as complete as the list's author's imagination, and the hook already writes the files it will run. What makes the hook safe to run is that it came, byte for byte, from the release the pinned signer attested. The daemon fetches the release, verifies its digest and attestation against the signer (repository and workflow) pinned for that service in the services file, stages and seals it, and only then runs its hook. This is the trust apt gives a package's maintainer scripts. The conformance suite (idempotence, the hook writes only its record directory, exit codes) runs in the service's own CI; at runtime nothing confines a hook.

**Designed (B3).** Built today, `crates/hostkeeper-rights/src/launch.rs` still builds the unit with `User`, `AmbientCapabilities`, `ProtectSystem=strict`, read and write paths, ten fixed hardening properties and `RuntimeMaxSec=<n>`; the loader and the runner refuse a timeout of 0. B2 and B3 reduce that to the unit above and accept `prepare = 0`. An unbounded step is ended by the hook returning or by `abort` (section 8.5).

### 5.4 Which hosts may update now: the coordinator's rules

![One live update across services: three hosts, three services, the single in-flight token, which orders are held and why, and what frees the token](diagrams/one-live-update.svg)

`decide()` is a pure function (`crates/hostkeeper-coordinator/src/decide.rs`, open PR #5). Per host and service, the first failing rule wins and its name is the reason shown to the operator:

`halted`, `no_definition`, `no_desired`, `not_a_member`, `duplicate`, `waiting_for_operator`, `quiet`, `bad`, `absent`, `updating`, `busy_elsewhere`, `up_to_date`, `failed_here`, `deferred`, `canary_wait`, `no_slot`, `baking`, `min_up`.

The four that look least like "desired version, membership, marks" are safety rules, kept: `waiting_for_operator` (a host pending approval gets no order), `duplicate` (a cloned disk gets none), `quiet` (a restarted coordinator orders nothing until it has heard the fleet) and `absent` (a service the host's file names but that is not installed gets `install`, not `update`). The earlier `waiting_for_role`, `waiting_for_service` and the `after` graph are cut in this revision (Q6): the single live update already orders everything.

The slot rule is the one that matters here:

> **One update is live in the whole farm at a time.** A host holds the slot while it is updating, while its last success has promotion pending, after a failed update until it beats healthy, from the moment the order is issued until the host reports it, and while it bakes. A host that holds an order and then goes silent keeps the slot; the order's age (`ORDER_AGE`, 120 seconds) frees it only while the host is still heartbeating without `updating`.

This is deliberate and has no setting. Promotion-pending, failed-until-healthy and bake extend the slot past the update itself, because a release that passes verify and fails shortly after must not reach a second host (Q8 asks whether to keep bake). Consequence: a slow drain on one host holds every other service's updates (section 8.9, Q1).

Other rules: `quiet` (the first 30 seconds after coordinator start order nothing; B7 makes it a flag the coordinator refuses to set below two heartbeat intervals); `failed_here` (this host failed this exact version; a new desired version lifts it); `deferred` (backoff from the rollout file, capped at 1 hour); `canary_wait` (the first N hosts go alone; a canary that reports `Failed` of class `release` meets the rollback threshold on its own and raises an alert); `baking` (the host holds the slot until N further healthy heartbeats at the desired version); `min_up` (never take the last live hosts of a role).

**Replayed verdicts.** The daemon resends its `last_update` until the coordinator acknowledges it by order id and try; a verdict is counted once per (host, order id, try).

**Automatic rollback** (`rollback = "automatic"`): when failures with phase `after_commit` or `gate_release` and class `release` reach the larger of the configured percent of live hosts and `min_hosts` (defaults 25% and 2), the coordinator marks the desired version bad first, then sets the desired version to `last_good`, if `last_good` is at or above `min_version`. Environment failures, deferrals and halts do not count. `last_good` advances only when the desired version has converged and baked with no slot held; a restart forgets the bake count and it is rebuilt from heartbeats.

**Halt.** A host reporting `Failed{phase: halt}` halts the service for every host. A halt raised by the hook's exit 76 is lifted only by the operator's `clear_halt`. A halt the updater itself raised (a rollback that failed before commit, `artifact_changed`) is also lifted by a new desired version from CI, since a fix-forward release is the remedy. Every halt is an alert. **Designed (B4):** the mechanism is a per-service generation sent in the reply and persisted by the daemon beside its hold files; the daemon lifts its hold only on a strictly greater generation.

**Force** (`Releases.Force`, in the protobuf crate `operator.proto`, not in the imported coordinator, B7): the operator names service, version and reason. In one step it records the force, lifts the bad mark of the target, marks the version it leaves bad and held, clears the halt, and sets desired. It passes canary, bake, `min_up`, deferral, bad, halt, failed-here **and the single live update**. It keeps quiet start and one update per host: a host in the middle of a step finishes that step first, and the order is never sent to a host that holds one. **Decision, and its cost:** a broken release of the system that deploys must be recoverable even when the slot is held by a hung `prepare` elsewhere, so a force does not wait for the slot; a forced update can run while another host is mid-update, and the audit log records the force and the slot it passed. A force names a version at or above the host's `min_version`; a version below it is reachable only by local root editing the services file (or the Q3 path), then a normal force. Automatic rollback is suspended while a force stands.

### 5.5 Fetching, verifying and staging a release

![Release fetch, verify and stage: order with version only, fetch by the daemon, size and digest check, attestation, archive shape check, binary self-report, tree seal, with what is root-written at each step](diagrams/release-fetch-verify-stage.svg)

Designed; kbf has the logic inside its own installer, and the seal reader is built (`crates/hostkeeper-steps/src/artifact.rs`, with `Staged::check`). The fetch step does not exist yet (B5).

The order carries a service, a version and the digest D that CI set with the version. Everything else comes from the service's entry in the services file. If the channel serves different bytes for the version, the digest check fails and the outcome is `Failed`, class `release`, reason `digest_mismatch`, before anything is staged.

1. **Order.** A service, a version and D. No URL, no path.
2. **Resolve.** The location is the entry's `release_url` with the version filled in. The coordinator can name a version and a digest; it cannot name a place.
3. **Fetch** by the daemon itself, into a file under the staging directory with a running byte count and SHA-256. (There is no privilege-dropped helper: it would be a rights filter, and the bytes are not parsed until step 6.) Connect timeout 30 seconds; total bound from the declared maximum size at a floor rate of 256 KiB per second. A timeout or network error is `Deferred`, class `environment` (`fetch_failed`, `attestation_unavailable`, `self_report_timeout`); a digest, attestation or shape failure is `Failed`, class `release`.
4. **Size and digest.** Bounded by a declared maximum; the SHA-256 of the bytes read equals D and the byte count.
5. **Attestation.** Verified offline by a pinned verifier against the entry's `signer {repository, workflow}`: right repository, right signing workflow, hosted runner, source commit equal to the release's, allowed ref. The provenance must list the tarball's SHA-256.
6. **Shape.** Only now is the archive parsed: one named regular file for the binary; no links, devices, absolute or `..` paths; nothing after the archive end; on macOS no quarantine attribute and a valid code signature. A release carries no declaration and no extension file: nothing in it can widen anything.
7. **Unpack and hash in one pass** into `releases/<service>/.stage-*`, owned by root; the tree hash covers sorted names, kind, mode, owner, size or link target, and bytes.
8. **Self-report, seal and publish.** The binary, which is already verified, runs as root from the staging tree with an empty environment and a timeout, and names its version, which must equal the order's. Then mode 0555, rename into `releases/<service>/<version>/`, and write `<version>.seal {tree_digest, artifact_digest}` root-owned. Only now does the release exist.

Every later step re-seals and compares before it runs (`Staged::check`). A tree that changed halts the service with reason `artifact_changed`; a tree that merely cannot be read right now is retried and never halts (`a_release_that_cannot_be_read_is_tried_again_and_never_halts`). Orphan `.stage-*` directories are removed at daemon start and before each new fetch of the same service.

Residual trust: the signer repository's main branch is the trust root. Nothing refuses an older attested release at or above `min_version`, so `min_version` is the only downgrade bound (section 5.1).

### 5.6 Forced rollback, with the coordinator down

![Forced rollback with the coordinator unreachable: operator on the host runs activate over the admin socket, the daemon takes the step lock, switches links to a kept release, restarts units, writes the pin](diagrams/forced-rollback-coordinator-down.svg)

A broken release must be recoverable with the coordinator down, with no code from the release being left.

**Kept releases.** A host keeps the release it runs, the previous one, every pinned release, the coordinator's `last_good` when known, and the newest four others. Pruning happens only after a switch is confirmed, takes the step lock, and never removes the `updater.previous_release` of an unfinished record, a pinned release, or the target of a switch on trial. A release that is not kept cannot be activated: recover through an ordinary update, or `ctl fetch` (B13), which stages without switching.

**`activate`.** On the host, as root: `hostkeeper ctl activate --service S --version V --release-digest D [--pin]`. The daemon takes the service lock and the step lock without waiting (a live holder is named and the call is refused; a stuck holder is ended with `abort`), requires the target to be a kept release at or above `min_version`, switches links, restarts the service's `observe_units`, and returns. No fetch, no trial, never reverted. It is a forced update in the record, so the runner's crash safety applies. The binary doing the switch is the installed `hostkeeper`, which a service release cannot write, so the swap runs nothing from the release it leaves. The coordinator can send the same verb when it is up.

**Pin.** `--pin` writes a root-owned hold file first and marks the release pinned second, so a crash between the two leaves a pinned host. `pin` and `unpin` take the service lock; the daemon reads the hold file again under the step lock before the first step of every order. A pin written while an update is in flight lets that update finish and is reported as `pinned_after_inflight`. A pinned service takes no order except `unpin` and `activate` naming exactly the pinned release. Lock order: service lock, then the step lock. The step lock is global to the host; an unbounded `prepare` on service A blocks `activate` for service B, which `abort` escapes.

**From the coordinator,** when it is up: `Releases.Force` (section 5.4). The seed-build path that does not depend on the broken system is the release pipeline's, outside this document. The operator reaches every host's socket over SSH when the coordinator is down. Not covered: the pin file is at kbf's legacy path; the neutral path `/var/lib/updater/pins/<service>` is the decision, both read until kbf retires its agent (B13).

### 5.7 Enrollment and remote install

![Enrollment and remote install: boot image with daemon, coordinator URL and CA fingerprint and no secret; pending request; operator approval; certificate; assignment; install order through the update path](diagrams/enrollment-and-remote-install.svg)

Designed. The image or package carries the daemon, the coordinator URL, the SHA-256 of the CA certificate in `/etc/hostkeeper/bootstrap.toml`, and the host's `services.toml`. No secret.

1. **Request.** On first start the daemon generates a P-256 key, collects claims (machine id, DMI serial and product uuid, network addresses; Mac: serial and platform UUID), persists the key, and only then posts `{csr, claims, image}` to the coordinator, whose certificate is checked against the pinned CA hash. The request id is the SHA-256 of the public key. The host is pending; nothing is installed. A second host presenting the same machine id or key is `duplicate_claim`, pending with an alert.
2. **Claims are not proof.** They help an operator recognise a machine. They authorise nothing alone.
3. **Approval.** The operator approves a request in the UI or CLI, naming host id, role, optional services, and the SHA-256 of the pending CSR's public key. An approval for a different key is refused; host ids are unique (re-enrollment uses `--replace`), checked under the store lock that records the approval. On a Mac the one automatic path is the MDM nonce (section 5.9).
4. **Certificate.** The daemon polls with a signed nonce and receives a 7-day certificate, its host id and the CA bundle. Issuance is idempotent per request id. The daemon renews at half life, signing with the key it holds. A host offline past expiry posts a new request marked `renewal_of <host id>` with the same key; one approval returns it. A cloned disk is seen as two live daemons on one host id with different machine ids: the host is `duplicate`, ordered nothing, with an alert.
5. **Assignment.** The operator assigns services to the host or a role. A service can be assigned only if the host's own services file names it. The coordinator emits `install S V D` under the same rules as an update.
6. **Install.** The same path as an update with no `updater.previous_*` keys. A failed first install rolls back or declines with `no_previous_release`.
7. **Remove.** A retiring host with an update in flight finishes or aborts that step, frees its slot when the record is final (or by `Exclude --abandon` if the host is gone), and is sent no `uninstall`: retiring revokes the host and leaves its services as they are. `uninstall S` is a separate order that runs the additive `uninstall` hook step if the hook has one (otherwise `uninstall_unsupported`), then removes the release tree and `state/<service>`; the daemon deletes no data directory.

### 5.8 Service health: observed, not reported

Designed (B8); replaces kbf's registration socket (`/run/updater/register.sock`), which is not carried; a new daemon unlinks an old one at start. The daemon makes the probes and reports the result in the heartbeat.

- **Liveness** from the service manager: systemd `ActiveState`/`SubState` of the entry's `observe_units`; launchd job state on macOS.
- **Version** from the step record, verified against the main process's executable: the pair (pid, start time) is read before and after hashing `/proc/<pid>/exe`; if they differ the round is discarded. A wrapper binary shows `version_mismatch`. The `/proc` hash check is the first to drop if it proves costly.
- **Health** from the entry's `health_url`: scheme `http`, loopback literal host, no redirects, 4 KiB read cap, only 2xx is healthy.
- States: `absent`, `stopped`, `transitioning`, `unobserved`, `unmonitored` (no `observe_units`), live-with-version-unknown. **An observation failure is `unobserved` and is never read as absent or stopped.**

### 5.9 macOS

![Mac path: MDM enrollment, package install by digest, per-device nonce, enrollment, auto-approval conditions, and the one launchd daemon](diagrams/mac-path.svg)

The flow is the same; these differ:

| Aspect | Linux | macOS |
|---|---|---|
| Daemon | systemd unit | one launchd daemon, root |
| Hook run | transient systemd unit | one launchd job per step, as root, for timeout, kill and log capture only; no sandbox profile, no rights |
| Process identity | `/proc/<pid>/exe` | `proc_pidpath` plus device and inode check (to verify) |
| Enrollment | operator approval | device enrollment through MDM, a package install, then auto-approval |
| Hand-over of ports | nft redirect | none; tierstore on a Mac updates with a gap |
| Live leases | the hook drains in `prepare` | a Mac with live leases defers, reason `leases_live` |
| Release verification | attestation, archive shape | plus no quarantine attribute and a valid code signature |
| Self-update | `OnFailure=` fallback | launchd `KeepAlive` plus a timer job that flips the link back |

Mac enrollment auto-approval, designed: MDM installs the hostkeeper package by allowlisted digest; the gate pushes a per-device one-time nonce as a managed configuration profile; the package's postinstall writes `bootstrap.toml`; the daemon enrolls with the nonce; the coordinator auto-approves only if the nonce is valid, unexpired (one hour), unused, consumed in the same durable transaction as the approval, issued for the claimed serial, the gate reports the device enrolled and supervised, and the serial is on the allowlist. **MDM is part of hostkeeper, decided:** the policy gate and installer kit are components of this repository with their own design document; this document states only the interface the enrollment path needs. The MDM profile identifier is renamed to the hostkeeper reverse-DNS prefix before any Mac enrolls (a build gate on B18).

### 5.10 The UI

Designed, not built. A web UI served by the coordinator, so an operator does not need the CLI for anything routine.

![UI, CLI and API: the generated CLI and the UI both call the coordinator's one API with the operator token; the UI listener is bound to the private network only](diagrams/ui-api-cli.svg)

- **One API.** The CLI is generated from the protobuf API (B10). The UI renders its pages from the same API handlers, and every button is an operator RPC authenticated by the operator's token. The UI has no operation the CLI lacks, and none the API does not define; a test lists the UI's actions and requires each to be an API method **(to build, B25)**. The operator API in PR #3 (`operator.proto`) has `Hosts.{List,Get,Exclude,Include,SetLabels}` and `Releases.{SetDesired,Converge,Force,UnmarkBad,ClearHalt,Status}`. These methods are **Designed** and still to add: `Releases.MarkBad`, `Releases.Halt`, `Hosts.{Pin,Unpin,Abort}` (queued as orders for the host's next heartbeat; verbs that change no release take no slot), `Enrollment.{List,Approve}`, `Audit.List`, and `Hosts.Exclude --abandon` (B1, B7, B17).
- **Private network only.** The UI is served on the coordinator's private listener (section 4.3), bound to the private (tailscale) address, never on the heartbeat listener. Sign-in is the operator token, exchanged by the UI server for an `HttpOnly`, `SameSite=Strict` session cookie that carries that token's name, with a CSRF token on every form; the API handler sees the token, so the audit log names the person.
- **Pages.**
  - *Fleet:* hosts, role, each service's installed version, health state, pinned and excluded marks, and the last update with its verdict, phase, class, reason, `detail`, try number and the last log lines the daemon returned in the heartbeat. A host row opens a drill-down. An *Alerts* filter lists the conditions of 6.7 (halt, duplicate, failed canary, abandoned host).
  - *Rollout, per service:* desired against converged, canary progress, halts, bad versions, and the one-live-update state (who holds the slot, since when, what is held and why).
  - *Actions* (every operator RPC; the test compares the two lists): exclude and include a host, halt a service, clear a halt, force rollback to a version, mark and unmark bad, pin and unpin, abort a step, list and approve pending enrollments. The operator does not set the desired version: CI does, and the operator uses Force (Q6).
  - *Audit log:* the append-only log, filterable by token name, host and service.
- **Stack.** Confirmed from kbf's public repository: the farm UI is `crates/kbf-ui`, server-rendered HTML from a Rust library with one static file (`kbf.css`); its guide `docs/deploy/kbf-ui.md` says "no script and no client toolchain", read-only, with no cookie or login (the "no script" claim is read from that guide, not from the crate source). Whether hostkeeper reuses it is Q4: the hostkeeper UI writes and signs in, which the farm UI does not.

## 6. Background work

### 6.1 The host lifecycle

![Host lifecycle state machine: unenrolled, pending, enrolled, retired, and per service absent, installing, running, updating, rolled back, halted, pinned, removing, with the event on each edge](diagrams/host-lifecycle.svg)

- **Host:** `unenrolled` -> `pending` (request posted) -> `enrolled` (certificate issued) -> `retired` (operator revokes; the host is refused at handshake at once and loses its certificate by expiry within 7 days; an update in flight finishes or is aborted locally; its slot is freed when the record is final, or by `Exclude --abandon` if the host is gone, section 8.4).
- **Service on a host:** `absent` -> `installing` -> `running` <-> `updating` -> `running` | `rolled back` | `halted` (until clear) | `pinned` (until unpin) -> `removing` -> `absent`.

### 6.2 The step runner

`StepRunner::run` (`crates/hostkeeper-steps/src/runner.rs`) drives one order of one service to a `Finished{verdict, reason}`:

- It writes `step-record.json` and `params.toml` in the service's private state directory (owned by root, mode with no group or other bits).
- Before each step it writes the record with state `running` and a deadline; after it, `done` with the exit code and last line. Writes are temp file, `fsync`, rename, directory `fsync`.
- On resume the rule is one table (section 6.5). The unit-name `<generation>` is a count of step runs persisted before the unit is launched, so a resumed run can stop the unit it launched.
- A finished record answers the same order again without running anything. A `Halt` record holds every later order for that service until cleared by `clear_halt` (local, or an order carrying a higher halt generation than the record's). A corrupt record is cleared only by `clear_record` (local, B20). `Deferred` runs again when the order returns.
- Time is injected: a `Clock` supplies monotonic seconds, boot id and sleep. Elapsed time across a different boot id counts as zero.

### 6.3 The dead-man

On its own thread, `Watch` arms with a unit and a monotonic deadline; at the deadline it stops the unit by name. A stop under way blocks `disarm`. A unit that cannot be stopped stops the runner with `Unstopped` and leaves the step `running`; the next run stops it first. The dead-man also bounds verify as a whole (`verify_deadline_secs`). A step declared unbounded (`prepare = 0`, B2) has no deadline. The `Watch` and `RunError::Unstopped` behaviour is Built. **Designed (B4, B20):** after `Unstopped` the daemon keeps serving and holds the service lock; `abort` retries the stop, and if it still fails marks the record `Finished{Halt, unstoppable}` and logs an alert. The farm slot stays held until then, which is intended.

### 6.4 The post-success watcher

After `cleanup` succeeds with `promotion_pending`, the runner keeps running `verify` every `retry_after_secs` (30 seconds when the hook gives none; at most 3600) until it succeeds without the flag, the promotion ceiling passes, or `verify` fails. **The watcher never runs `rollback`** (Built, `StepRunner::watching` in `runner.rs`): a failing `verify` ends it with `Failed(after_commit)` (or Halt on exit 76), and the coordinator's automatic rollback is the remedy. This is narrower than "the updater never rolls back after a commit": a failed `cutover` or `verify` earlier in the same update does run `rollback` even when a step has committed (section 5.2).

### 6.5 Boot

**In kbf** (`kbf-updater/src/root/mod.rs` for the `Locked` error and lock file, `root/boot.rs` for the boot-id pass); hostkeeper `main` has only the runner side. B4 imports the rest.

At daemon start, before any unit: lock the state directory, load the services file (a refused entry is unmanaged), then read every step record against the boot id. One table, the same for a crash of the daemon and for a reboot:

| Record | Daemon died, host did not reboot | Host rebooted (boot id changed) |
|---|---|---|
| `prepare` pending or running | stop the unit by name, run `prepare` again | run `rollback`, then report Deferred `rebooted` |
| `cutover` pending or running, nothing committed | stop the unit, run `cutover` again | run `rollback` before any unit starts, then Failed `before_commit` |
| `cutover` or later, a step has committed | run the step again | restart of the new release; continue with the next step |
| `verify` running | run `verify` again | nothing committed: `rollback`, Failed `gate_release`; committed: continue `verify` with a fresh window |
| `rollback` or `cleanup` running | run it again | run it again |
| `done` | recompute the next step | recompute the next step |

The new boot id is written only after every record was read. Then each unfinished update runs to its verdict, watchers restart, and the entry's `boot` step runs from the current release (tierstore's `restore`). Then `sd_notify` ready. The `current` link flips at the commit point or, when nothing committed, at the success of the update's last step.

### 6.6 Updating hostkeeper itself

Never inside the trial it guards. **Built** (`crates/hostkeeper-steps/src/selfupdate.rs`): the binary directory layout, staging the new binary under `versions/<v>`, repointing `current` (keeping `previous`) by symlink rename and directory `fsync`, `follow_on` returning a restart request, `flip_back`, and the generated `OnFailure=` drop-in (`StartLimitBurst=3` within 300 seconds) and fallback unit. **Designed (B22):** the restart confirmation, i.e. the step record reporting Succeeded for version V only after the new binary has run (an `awaiting_restart` state the new binary confirms on first start); the fallback verb writing `Failed(after_commit)` for V and marking V bad locally so the coordinator stops re-ordering it; the refusal to self-update while another service's step record is `pending` or `running`, with the swap taken under the step lock; and the self-update that clears an `updater_old` deferral (the deferral itself is Built in `crates/hostkeeper-steps/src/runner.rs`). Nothing in `selfupdate.rs` implements these today.

### 6.7 Coordinator background work

Reconcile on every heartbeat: advance `last_good`, mark bad, plan automatic rollback. Reload TLS certificate and key every 60 seconds; a bad pair keeps the one in use. The coordinator has no pager and calls out to nothing: every condition that needs a person (`halt`, `waiting_for_operator`, `duplicate`, a failed canary, a host abandoned while updating) is a log line at error level with an `alert=` field, and also shows in the UI.

## 7. Edge cases handled

| Case | Behaviour | Where |
|---|---|---|
| Hook prints nothing or garbage | `{}`: class release, reversible true; exit 0 with no line is a success | `line.rs` |
| `reversible` is not a boolean | read as false: treated as committed | `line.rs` |
| Unknown exit code | failure | `exit.rs` |
| `cutover` exits 75 after a step reported `reversible:false` | treated as failure; `verify` and `cleanup` retry on 75 | `next.rs` |
| Old hook never states `reversible` | a failed cleanup is retried forward, never rolled back | `reversible_stated` |
| `prepare` returns 75 | everything it placed is undone first; if undo fails it must exit 1 | contract |
| Rollback itself defers | run again up to three times, then treated as failure | `ROLLBACK_ATTEMPTS` |
| Rollback fails before commit | Halt | `next.rs` |
| Rollback fails after commit | Failed after commit if the hook says `reversible:false`, else Halt | `next.rs` |
| Hook-requested wait too long | clamped to 3600 seconds | `MAX_RETRY_SECS` |
| Newer-schema step record | read, reported, never rewritten | `RecordNewer` |
| Corrupt step record | the run fails; the operator runs `clear_record` (B20) | `RecordError::Corrupt` |
| First start of a daemon not yet enrolled | logs once, calls nothing, retries every few seconds, finishes a half-written enrollment under the store lock | designed from kbf `enrolled.rs` |
| Order for a service the services file does not name | `not_in_services_file`, nothing runs | daemon (to build, B4) |
| Order carrying a version that is not a version | refused by grammar | `crates/hostkeeper-wire/src/version.rs` (open PR #5) |
| Coordinator restarted | quiet 30 seconds, fleet rebuilt from heartbeats | `crates/hostkeeper-coordinator/src/fleet.rs` (open PR #5) |
| Heartbeat with a bad optional fact | the fact is dropped, the rest is read | `crates/hostkeeper-wire/src/wire.rs` (open PR #5) |

## 8. Races and failures

Each case has a decision and a test. A test in `code font` with no tag exists on `main` today; others are tagged. Every test is hermetic and asserts no elapsed time: clocks are injected.

### 8.1 Two updates at once

**One host.** The coordinator orders a host only if it is not updating and has no outstanding order. The daemon holds a per-service lock and one step lock; a second order for a different release while one is unfinished is refused `busy`; a repeat of the same order is a resend. Tests: the `decide` table test for `updating` and `busy_elsewhere`; `a_second_order_while_one_runs_is_busy` **(to build, B4)**.

**Different hosts.** One slot farm-wide, read and written under one lock that heartbeat decisions, CI writes and rollback planning all take; the second host's order is `no_slot`. Tests (**open PR #5**): `racing_heartbeats_for_two_services_get_one_order`, `a_host_that_leaves_a_service_mid_update_still_holds_the_slot`.

**Two coordinators or two daemons.** Not supported. A second `serve` on the same state files is refused by their lock (`a_second_coordinator_on_the_same_files_does_not_start`, **to build, B7**); a second daemon exits `Locked` (**in kbf**, B4).

**CI against automatic rollback.** A `set-desired` for a version marked bad is refused; only the operator's unmark or a `Force` lifts a mark (`set_desired_of_a_bad_version_is_refused`, **to build, B7**).

### 8.2 The coordinator dies mid-roll

The daemon does not need the coordinator to finish a step; it writes its verdict and resends it until acknowledged. The coordinator restarts with an empty fleet and a quiet start of two heartbeat intervals, and rebuilds from the first heartbeats: the fleet, each host's `updating` state (so the slot), bake (from zero; `last_good` waits), deferral backoff (reset, with a per-host floor of one retry per `initial`), outstanding orders (forgotten). Durable: desired state (version and digest), ops file, halts file, enrollment store. `order_try` is derived from the highest try heard in heartbeats. **Decision:** a restart costs the quiet start plus one heartbeat round and restarts bake; it never loses a halt or a bad mark. A halt whose write fails is held in memory and retried every heartbeat, and the coordinator refuses to start if a previous halt write was unfinished. Tests **(to build, B7)**: `a_restart_forgets_orders_and_a_beating_host_takes_its_slot_back`, `a_restart_resets_bake_and_last_good_waits`, `an_unwritten_halt_is_retried_and_blocks_start`; the `a_crash_*_reached_the_ops_file_*` family (open PR #5).

### 8.3 The daemon dies mid-step, or the host reboots

The step's transient unit continues in its own cgroup. The restarted daemon reads the record and applies the table of section 6.5: a `running` step's unit is stopped by name and the step run again. Before commit a reboot is an abort (rollback runs before any unit starts); `cleanup` and `rollback` run again; after commit it is a restart of the new release, and a committed update's `verify` window restarts. For `activate`, which has no trial, a reboot after the links moved leaves the target active. The coordinator's resend of the same order (same `try`) is answered from the record; same try with different fields is `try_conflict`; a higher try is a new order once the last has ended; a lower is `stale_try`. The hook's own lock on its record directory is the backstop: a second copy exits 1 and changes nothing. **Decision:** steps are re-run, never skipped, and a committed step is never undone by resume. Tests: `tests/crash.rs` sweeps a crash before and after every record write; `a_launcher_failure_stops_the_runner_and_resume_stops_the_unit_first`; `the_verify_window_restarts_after_a_reboot`; conformance checks 5 and 8; `resend_after_daemon_death_returns_the_recorded_verdict`, `same_try_with_a_different_version_is_try_conflict` and a boot-id sweep over all step positions **(to build, B4)**.

### 8.4 Network partition between coordinator and daemon

The daemon keeps running whatever step it is in. The coordinator declares the host down after 45 seconds without a heartbeat. **Decision:** a host that was updating, or holds an order, **keeps its slot** while down; it cannot be known whether the host is mid-cutover. The operator ends the wait with `Hosts.Exclude --abandon` (**Designed, B7**), which records in the audit log that the host may still be mid-update; plain `Exclude` keeps the slot. Tests **(to build, B7)**: `a_down_host_that_was_updating_keeps_the_slot_until_abandoned`, `abandon_frees_the_slot_and_is_audited`.

### 8.5 A hook hangs or lies

**Hangs.** Every step has `RuntimeMaxSec`, the dead-man stops the unit by name, timeout is failure, and verify has an overall deadline. An unbounded `prepare` has neither: the only endings are the hook returning or `abort`, which stops the unit, retries the stop if needed, and ends the order `Finished{Halt, aborted_by_operator}` (B20). Tests: `a_step_past_its_own_bound_is_stopped`; `an_unbounded_prepare_is_never_stopped_by_the_dead_man` and `the_loader_accepts_zero_for_prepare_only` **(to build, B2, B4)**; `abort_stops_an_unbounded_prepare_and_halts_the_order` **(to build, B20)**.

**Lies.** Class is the hook's call and is not trusted for promotion: `baking` counts healthy heartbeats from the observation of section 5.8. If `verify` exits 0 and the service is dead, the observer reports `stopped` or `unhealthy`, the host holds its slot and bake does not advance (`a_hook_that_says_ok_while_the_service_is_down_never_bakes`, **to build, B8**). A hook that says `reversible:true` and cannot roll back halts the service; conformance check 4 catches it in the service's CI. A hook that writes outside its record directory is caught by conformance check 3 in CI; nothing confines it at runtime (section 5.3).

### 8.6 A bad release that passes health, then fails

`verify` succeeded, `cleanup` committed, `promotion_pending` is true. The watcher runs `verify` until it passes without the flag or the ceiling passes (`Failed(after_commit)`; the watcher never runs `rollback`, section 6.4). The coordinator holds the slot through bake. If the service later fails after commit on enough hosts, automatic rollback marks the version bad and sets `last_good`; with `rollback = "manual"` the operator uses `Force`. Tests: (**open PR #5**) `release_failures_past_the_threshold_roll_desired_back_and_mark_the_version_bad`, `environment_before_commit_deferred_and_halt_outcomes_never_roll_back`; `the_watcher_never_runs_rollback` **(to build, B4)**.

### 8.7 Forced rollback

`Force` passes the single live update but not a host that holds an order or is mid-step. On a host, `activate` takes the locks without waiting; a running step is refused `busy`, naming the step. **Decision:** an update in flight is never torn by a force or an activate; `abort` then `activate` is the exit. With the coordinator unreachable `activate` and `pin` work on the local socket; the daemon notices the hold file and takes no order for that service, and the coordinator, when it returns, sees `pinned` in the heartbeat. Rollback by hand is a pin; unpin returns the host to the desired version. Tests **(to build)**: `activate_while_a_step_runs_is_busy_and_names_the_step` (B13), `force_passes_the_slot_but_not_a_host_that_holds_an_order` (B7), `abort_then_activate_succeeds` (B13, B20), `activate_works_with_no_coordinator_and_the_daemon_stops_taking_orders` (privileged workflow, B15), `a_pinned_host_is_not_ordered` (B7), `a_pinned_service_accepts_only_unpin_and_activate_of_the_pinned_release` (B4), `a_pin_written_before_an_order_is_honoured_under_the_step_lock` (B13).

### 8.8 Enrollment and CI replay

Each approval names the SHA-256 of the pending request's key, so a different key cannot ride it. The pending queue is capped (`pending_full`); host ids are unique under the store lock; the key is durable before the request is posted; issuance is idempotent per request id; an expired host returns through a `renewal_of` request; a cloned disk is detected as `duplicate`; Mac nonces are consumed in the approval's transaction. Tests (**to build, B16, B17**): `approval_for_a_different_key_is_refused`, `a_full_pending_queue_keeps_existing_requests`, `a_nonce_is_consumed_with_the_approval_across_a_restart`, `two_approvals_for_one_host_id_admit_one`, `a_reposted_request_is_the_same_request`, `a_crash_between_approval_and_issue_yields_one_certificate`, `a_host_offline_past_expiry_returns_as_a_renewal_request`, `two_live_daemons_on_one_id_are_duplicate`. CI replay: OIDC `jti` values are spent once and persisted until their expiry, so a restart opens no replay window (`a_spent_jti_stays_spent_across_a_restart`, **to build, B7**).

### 8.9 A service with live work that must drain

The hook drains inside `prepare`; the updater does not cut work at a deadline. The service chooses: (1) **wait without bound** (`prepare = 0`): the order holds the slot as long as work remains; (2) **wait to a bound, then defer:** `prepare` returns 75 `drain_bound` after undoing its drain, the slot is freed, the coordinator backs off; (3) **drain in the service's own stop path** (tierstore): the new release starts beside the old. **Decision:** the default is 1. A bound is an opt-in with no ceiling the updater imposes; the 3600-second clamp is a polling interval, not a drain deadline. Q1 asks whether the farm may move on while an unbounded drain waits. Tests: `an_unbounded_prepare_holds_the_slot_and_a_bounded_one_frees_it` **(to build, B7)**; conformance check 6 (`a_hook_that_ignores_a_fault_or_keeps_state_after_a_deferral_is_refused`).

### 8.10 Further cases

| Case | Decision | Test |
|---|---|---|
| Release changed after verification | re-seal before every step; mismatch halts `artifact_changed` | runner tests with a mutated tree |
| Channel serves other bytes for a version | `digest_mismatch`, nothing staged | `a_changed_artifact_for_a_version_is_refused` **(to build, B5)** |
| Fetch or attestation hangs | per-stage deadlines; `Deferred`, slot freed | `a_stalled_fetch_defers_and_frees_the_slot` **(to build, B5)** |
| Order for a service not in the services file, or below `min_version` | refused, nothing runs | `a_service_not_in_the_services_file_is_refused`, `an_update_below_min_version_is_refused` **(to build, B4)** |
| An order field that could name a command, path or URL | no such field exists in the order messages | descriptor test **(to build, B1)** |
| Clock skew | nothing the updater decides reads the wall clock (monotonic time plus boot id); it is used only for certificate, OIDC and nonce validity, so a host with a wrong clock cannot handshake, is silent and keeps its release | `no_decision_reads_the_wall_clock` (an `xtask` lint, **to build, B19**); `a_token_that_expires_while_its_keys_are_fetched_is_refused` (**open PR #5**) |
| Disk full while staging | `ENOSPC` removes the staging directory and returns `Deferred`, class environment, `no_space`, before any hook runs; if the step record cannot be written no hook runs (`journal_unavailable`); pruning never makes room for the update in flight | `enospc_during_staging_defers_with_no_space_and_leaves_no_partial_tree`, `enospc_writing_the_record_runs_no_hook`, `a_crashed_staging_directory_is_removed_at_start` **(to build, B5, B4)** |
| Hook output unbounded | keep the last 1 MiB in a ring; a last line over 64 KiB reads as `{}` and the exit code decides | `a_hook_that_prints_without_limit_is_bounded_and_its_exit_code_stands` **(to build, B3)** |
| Daemon resends or the fleet resends verdicts | counted once per host, order id and try | `a_resent_verdict_counts_once` **(to build, B7)** |
| Halt raised by the updater, fix released | a CI write of a new version lifts it; a hook's exit-76 halt needs `clear_halt` | `ci_write_lifts_an_updater_halt_but_not_a_hook_halt` **(to build, B7)** |
| `set-desired` below `min_version`, unparseable, or bad | refused | `set_desired_refuses_below_floor_unparseable_and_bad` **(to build, B7)** |
| Retiring host mid-update | the step finishes or is aborted; slot freed when the record is final, or by `Exclude --abandon` if the host is gone | `a_retired_host_mid_update_frees_its_slot_when_final` **(to build, B16)** |
| Self-update during another service's step | refused; the swap takes the step lock | `self_update_waits_for_an_idle_step_lock` **(to build, B22)** |
| Self-update new binary cannot start | fallback flips the link back; V marked bad | `a_fallback_marks_the_version_bad_and_the_coordinator_stops_ordering_it` **(to build, B22)** |
| Old daemon with a new coordinator, and the reverse | features negotiated; new reply fields go only to daemons that announced them; roll the coordinator first | **(to build, B7)** |
| Mac service-manager error, or path replaced while running | `unobserved`; a Mac with live leases defers | fake launchd port in the pull-request set; device run in the privileged workflow (B18) |

## 9. Assumptions

- systemd 250 or newer on Linux for `systemd-run --wait --pipe`.
- The release channel and the CI provider's attestation service are reachable at update time; neither is needed for rollback to a kept release.
- The signer repository's branch is protected enough that a commit on it is trusted. Nothing here defends against a compromised signer (section 5.1).
- Hosts have a monotonic clock and a boot id that changes on every boot.
- A host's root account is the operator's. hostkeeper defends the host against the network and the coordinator only as far as section 5.1 states.
- Hooks are idempotent and decide from their own durable record; the conformance suite checks this for hooks that run it.
- One coordinator per farm, on a host whose disk survives a restart.
- The operator can reach each host as root by some means that does not depend on hostkeeper (SSH, console).

## 10. State: where each thing lives and who writes it

| Thing | Location | Writer | Reader | Notes |
|---|---|---|---|---|
| Services file | `/etc/hostkeeper/services.toml` | package or boot image; local root | daemon | root-owned, not group or other writable; the coordinator cannot write it (updating it fleet-wide is Q3) |
| Release tree and seal | `/var/lib/updater/releases/<service>/<version>/`, `<version>.seal` | daemon fetch step | daemon, hooks (read-only) | mode 0555; `current` link = last release whose update succeeded |
| Step record | `/var/lib/updater/state/<service>/step-record.json`, `params.toml` | daemon | daemon | schema 1; temp, `fsync`, rename, directory `fsync` |
| Boot id | beside the step records | daemon | daemon | written after all records were read |
| Hook record | the entry's `state_dir`, e.g. `/var/lib/updater/hooks/<service>` | hook | hook | created by the daemon |
| Pins and hold files | `/var/lib/updater/pins/<service>` (kbf legacy path read as well) | daemon (local root through the socket, or a coordinator `pin`) | daemon | root-owned regular file |
| Held halt generation | beside the hold files | daemon | daemon | persisted so a restart cannot lift a halt wrongly |
| Kept-release marks | beside the state file | daemon | daemon | unparseable means nothing is pruned |
| Host certificate and key | `/etc/updater/pki/gen-<n>/` with `current` link | daemon (renew) | daemon | key generated on host and never leaves |
| Desired state | coordinator `--state-file` | coordinator (CI writes through it) | coordinator | version and release digest per service |
| Operator tokens and audit | `--tokens-file`, `<ops-file>.audit` | operator (tokens); coordinator (audit, append-only) | coordinator | one named token per person |
| Ops file, halts file | `--ops-file`, `<ops-file>.halts` | coordinator | coordinator | exclusions, labels, bad marks, `last_good`; strict reader refuses unknown keys and newer schema |
| Fleet, outstanding orders, bake, deferral | memory | coordinator | coordinator | rebuilt from heartbeats (section 8.2) |
| OIDC `jti` cache | file of spent ids until expiry, plus memory | coordinator | coordinator | capped at 10,000 |
| Enrollment requests, deny rows | files in the coordinator's state directory | coordinator | coordinator | designed; binary protobuf with a schema number |
| CA key | coordinator host, owner-only | coordinator setup | coordinator | one sealed offline backup |

Naming: `updater` names (`/var/lib/updater`, unit names, reserved keys) are stored formats and stay. `hostkeeper` names (`/run/hostkeeper`, `/etc/hostkeeper`, `/var/lib/hostkeeper`) are new.

## 11. Limits

1. **One update at a time, farm-wide.** A farm of N hosts with a ten-minute update takes at least N times ten minutes for one service. Canary and bake add to it.
2. **A slow drain blocks the farm** unless the service declares a bound (8.9).
3. **Wire.** A heartbeat lists at most 16 services in the imported coordinator; the protobuf schema says 17. They must agree before the protobuf API is served (B1). Heartbeat bodies are capped at 16 KiB, control bodies at 4 KiB, a bearer at 8 KiB.
4. **Single coordinator,** no failover; its state is a handful of files; a restart costs the quiet start and resets bake and deferral backoff.
5. **A global lock around decisions and disk writes:** a slow disk on the coordinator stalls heartbeats. Acceptable at the farm sizes this targets.
6. **One step at a time per host.** No parallel services on one host. Pins are per service per host.
7. **A hook is root and nothing confines it** (section 5.3). The limits on a bad hook are the signer and the conformance suite, not the daemon.
8. **Names.** Service names `[A-Za-z0-9_-]`, versions 1 to 128 characters of `[A-Za-z0-9._+-]`, no leading dot. Roles 1 to 32 bytes, `a-z0-9-`.
9. **macOS** has no hand-over of ports.
10. **Wire compatibility.** Additive only: fields never removed, only reserved; every enum zero is `*_UNSPECIFIED`; breaking changes need a `v2` package. A compatibility test compares each release's descriptor set against a committed baseline. Existing JSON routes stay frozen as adapters over the same handlers until every host and the coordinator speak protobuf; daemons announce features (`deferred-v1`, `halt-v1`, `heartbeat-v2`, `forced-v1`) and the coordinator advertises only to those that announced.
11. **Output.** The last stdout line is parsed (at most 64 KiB); 1 MiB is kept. Until B3, output is unbounded.

## 12. Future work and the build order

### 12.1 Not planned in this document

- Network boot (PXE, UEFI HTTP boot): a vision, no build items. Apple silicon cannot net-boot, so Macs stay on MDM.
- A coordinator high-availability design.
- Automated rollback rehearsal in every release.
- TPM-based hardware identity for enrollment.

### 12.2 Build order, from today's code to done

Today: the hook, rights and steps crates are on `main`; the protobuf crate and the coordinator are open pull requests. "After" lists prerequisites. Size: S under a day, M a few days, L a week or more.

| # | Item | Implements | After | Size |
|---|---|---|---|---|
| B1 | Rebase and merge the protobuf crate **with the root-part service, `ApproveDeclaration` and `declaration_digest` removed**; the order messages carry a verb and typed arguments only; one service-count limit; wire-name pins | 4.1, 11.3, 11.10 | none | S |
| B2 | **Replace the declaration with the services file:** loader keeps the root-owned-file check and the name grammar; accept `prepare = 0` (kbf `main` already launches it with `RuntimeMaxSec=infinity`; test `unbounded_prepare` in kbf's `update-rights`; the import re-checks anything else kbf gained after the imported tag); `min_version`, `release_url`, `health_url`, `observe_units`; **delete** the widening check, the gate, the approval store, the `approve` command, the account and reserved-path checks | 4.7, 5.3 | none | M |
| B3 | Production `StepLauncher`: transient unit with `RuntimeMaxSec` only (no user, capabilities or sandbox), `infinity` for unbounded `prepare`; bounded output capture | 5.3, 6.2, 8.10 | B2 | M |
| B4 | **The daemon program:** one root unit that merges kbf's agent and root part; admin socket; the compiled verbs; per-service and step locks; try semantics; boot pass; watchers | 4.1, 5.2, 6.5, 8.1 to 8.3 | B1, B3 | L |
| B5 | Release fetch, verify, stage, seal (one-pass hash); `no_space` | 5.5, 8.10 | B4 | L |
| B6 | Merge the coordinator import (PR #5); rollout file as data; legacy-service flag; named operator tokens file and append-only audit; one private listener; cut `waiting_for_role`, `waiting_for_service`, `after` and `max_updating` | 4.3, 5.4 | none | M |
| B7 | Coordinator: file lock at start; `Hosts.Exclude --abandon`, `Releases.{MarkBad,Halt}`, `Hosts.{Pin,Unpin,Abort}`; `Force` that passes the single live update; desired state carries the release digest; `set-desired` refusals; persisted `jti`; unwritten-halt rule; replayed-verdict counting; canary rule; updater-halt lifted by CI; `quiet` flag floor; bake and slot tests | 5.2, 5.4, 8.1, 8.2, 8.4, 8.7, 8.8, 8.9 | B6 | L |
| B8 | Service-state observer (systemd, launchd ports; health probe; bake from observation) | 5.8, 8.5 | B4 | M |
| B9 | Daemon heartbeat loop: order handling, resend, enrollment client hook-up | 4.1, 5.2, 8.3 | B1, B4, B6 | L |
| B10 | `ctl` generator from descriptors (including the methods named in 5.10); gRPC serving beside the JSON adapters | 3, 11.10 | B1 | M |
| B11 | Linux packaging: deb, rpm, unit, default `services.toml` | 4.6 | B4, B9 | M |
| B12 | Privileged workflow on `main`: real systemd, root rig, workload fixture | 8.3, 8.7 | B4 | M |
| B13 | Local admin verbs: kept releases, `activate`, `ctl fetch`, pin under the step lock at the neutral path, `activate` of the pinned release accepted on a pinned host | 4.1, 5.6, 8.7 | B4 | M |
| B14 | Release automation: version rule, artifacts, attestation, release job | 4.5, 4.6 | B11 | M |
| B15 | Privileged proof: end-to-end update of a tierstore-shaped hook; forced rollback with no coordinator | 8.7 | B12, B13 | M |
| B16 | Certificate module in the coordinator, server certificate, daemon enrollment and renewal, deny rows | 4.4, 5.7, 8.8 | B9 | L |
| B17 | Enrollment approval: `Enrollment.{List,Approve}`, operator, Mac nonce; pending cap; security review | 5.7, 8.8 | B16 | M |
| B18 | macOS: launchd daemon, per-step job, device run | 5.9 | B4, B8 | L |
| B19 | Lint: no wall-clock reads outside TLS and OIDC | 8.10 | none | S |
| B20 | `abort`, `clear_halt`, `clear_record` | 6.2, 6.3, 8.5, 8.7 | B4, B13 | S |
| B21 | Adopt hosts that run kbf's agent: one-window move with back-out (Q2) | 12.3 | B9, B13 | L |
| B22 | Self-update of hostkeeper: restart confirmation, fallback verdict, refusal during other steps, protocol check | 6.6 | B4 | M |
| B23 | `install` and `uninstall` orders and assignments | 5.7 | B16, B5 | M |
| B24 | Consumers: services' CI uses `ctl releases set-desired` after provenance check; conformance harness as a dev-dependency | 4.5, 3.1 | B10, B14 | M |
| B25 | **The UI** (blocked on Q4) | 5.10 | B6, B7, B10, B17 | L |
| B26 | Remove the `hostkeeper-rights` crate or fold what is left into the services-file crate; drop `/etc/updater.d/approved` from tests and docs; update `README.md` and `MIGRATION.md` | 3.1 | B2, B3, B4 | S |
| B27 | Services-file update path, as Q3 decides (touches 5.5: a distinct artifact kind) | 4.7, 5.5 | B5, B14 | M |

**Deviation from the generate-the-CLI-from-the-API order:** the ruling is to define the protobuf first and generate the clients and CLI from it. Here the coordinator being imported is HTTP/JSON, so the JSON routes land first (B6) as adapters, and the generated CLI (B10) follows the protobuf crate (B1). Until B10 the `ctl` verbs named here are the allowed hand-written exception for daemon entry points and offline recovery, replaced by generated verbs without changing their names.

Critical path: B2, B3, B4, B5, B8, B9, B13, B11, B14. The UI (B25) is off the critical path and waits on Q4. First end-to-end proof (a CLI-set version reaches a hook through coordinator, daemon and staged release): B9, with B5. First release: B14.

### 12.3 Moving hosts that run kbf's agent

Decision: **a one-window move with a back-out.** For a host: place the hostkeeper program, stop kbf's agent and root part, start hostkeeper's with the shared state directory and certificates, confirm by the host's first heartbeat reporting the new daemon; back-out is stopping hostkeeper's unit and starting kbf's. The coordinator moves first on kbf's own state files so either family is served by the same bytes (`--legacy-service`). Downtime of the updater (not of the services) is minutes. Rules: the two never run together (`Conflicts=` and a lock on the shared state directory); a forced-rollback pin is read from both the legacy and the neutral path; certificates move to hostkeeper's CA only after an operator closes the back-out window, and the re-key is a point of no return. **Accepted cost:** after it a host cannot be served by kbf's frozen tools, so a rollback of that host is an older hostkeeper release. The alternative is Q2.

## 13. Open questions

Each has options and a recommendation. Until decided, the design behaves as the "now" column. Numbers are this document's own.

**Q1. May the farm move on while an unbounded drain waits?** Now: no; the slot is held. Options: (a) keep, and a service that cannot wait declares a bound and defers; (b) a host in `prepare` with only a drain pending releases the slot; (c) per-service lanes (rejected: it brought back the interactions the single rule removed). Recommendation: (a) until the maintainers rule, because (b) changes "one live update".

**Q2. How are hosts moved from kbf's agent?** Now: 12.3, the one-window move. Option: a journaled adopt with a reclaim timer and a frozen kbf-updater; or two CAs indefinitely. Recommendation: the one-window move with the one-way re-key; add the journal only if a host move is ever needed unattended.

**Q3. How is a new service added to a fleet?** The services file is root-owned and the coordinator cannot change it, so a new service needs the file to change on each host that will run it. Now: local root or the boot image only. Options: (a) **local root or boot image only:** edit or reimage; nothing remote can add code; cost: touching every host, which does not scale past a small farm. (b) **The services file shipped as a signed release** from a repository pinned in the hostkeeper package, fetched, verified and installed through the ordinary update path under a special service name. It needs a distinct artifact kind (one TOML file, not a binary), so it is an exception to the 5.5 shape rule and skips the self-report step; the file is validated whole and applied by atomic rename, and each entry's `min_version` may only rise, so a services release cannot roll the floor back. It adds a compiled-in rule to `update`, and it widens the threat model: the services-release signer can add a service and its signer on every host (5.1). (c) The coordinator carries the list, with operator signatures: rejected, it brings the approval keys back. Recommendation: (b), with the services-release signer being the `hostkeeper` signer, which already has root on every host, so no new trust root exists. This is the maintainers' call.

**Q4. What is the UI stack?** The facts about the farm UI are in section 5.10. Options: (a) the same approach: server-rendered Rust pages and forms that POST; (b) reuse `kbf-ui` as a library (couples two repositories' release cycles); (c) a client-side app (adds a toolchain and a second build). Recommendation: (a), sharing no code until a page toolkit is extracted to its own crate.

**Q5. Keep the CA signer a separate process?** Now: a coordinator module. The signer as a separate process protected the CA key from a coordinator compromise; under the threat model of section 5.1 a compromised coordinator can already order any verb on any host, and a host trusts the CA only to authenticate the coordinator and itself. Options: (a) module in the coordinator, key file owner-only, one sealed backup; (b) separate process on its own socket. Recommendation: (a).

**Q6. What else is cut or kept in the coordinator?** The coordinator's duties under this revision: desired version per service, fleet membership, operator marks (exclude, halt, bad, force), the per-heartbeat `decide()` rules including the one live update, CI by OIDC, operator tokens, the API, the CLI endpoint and the UI. Cut in this revision (reversible; adding one back is additive): (a) service identity certificates; (b) batch-token enrollment and the `operator`/`auto` assurance levels; (c) `waiting_for_role`, `waiting_for_service`, the `after` graph and `max_updating`; (d) the earlier read-only page; (e) per-token set-desired grants and a third listener: tokens operate or do not, CI sets desired, the operator uses Force, and the API and UI share one private listener. Kept, with a recommendation each: canary (keep: it is the only protection before bake); the safety rules `waiting_for_operator`, `duplicate`, `quiet`, `absent` (keep, section 5.4); the pending-enrollment queue with `duplicate_claim` and `renewal_of` (keep: they are the enrollment safety rules); Force passing the live-update slot (keep, section 5.4); **automatic rollback with `last_good`** (recommend cut before the first release: the operator has Force and one click in the UI, and it is the largest piece of coordinator logic; it exists in PR #5); `deferred` backoff settings per service (recommend one fixed constant); the JSON adapters (keep until kbf's agent retires, then remove). The separate CA signer process is Q5.

**Q7. Add `min_free_bytes` to the services file?** Now: no; staging fails with `no_space`. Recommendation: no; size the volume.

**Q8. Keep bake?** Bake is N further healthy heartbeats at the desired version before the next host, counted by the coordinator. Recommendation: keep; it stops a bad release reaching the second host. It restarts on a coordinator restart.

**Q9. Do privileged and macOS tests count toward the 99% gate?** Now: no. Recommendation: not counted; keep real-OS code in thin adapter modules and fake the ports.

### Decided

| Was open | Decision | Where |
|---|---|---|
| Rights model for hooks, on Linux and macOS | None: hooks run as root from a verified release; the transient unit or launchd job is for bounds and capture only (the maintainers' ruling on this design) | 5.3, 5.9 |
| Remote approval of a declaration | Gone with the declaration's rights; nothing to approve | 5.1 |
| Agent and root part as two processes | One root daemon; the CLI on a root-only socket | 3, 4.1 |
| Does a privilege-dropped helper fetch or self-report? | No: the daemon fetches and runs the verified binary itself; a helper is a filter | 5.5 |
| May an order go below `min_version`? | No, never remotely | 5.4, 5.1 |
| Is MDM part of hostkeeper? | Yes; own document; profile identifier renamed before any Mac enrolls | 5.9 |
| Is hostkeeper built in kbf first or here? | Here; kbf is frozen against hostkeeper-like work | 3.1, 12.3 |
| Abort, force, `jti`, halt-write failure, digest source, version order | `abort` exists; `Force` passes the slot; `jti` persisted; refuse to start on an unfinished halt write; CI supplies the digest; `version_scheme` | 4.3, 5.2, 5.4, 6.3, 8.2 |

## 14. Glossary

- **Artifact:** a release tarball with an attestation naming its SHA-256.
- **Bake:** the number of healthy heartbeats at the desired version a host keeps its slot for.
- **Canary:** the first hosts of a rollout, updated alone.
- **Commit point:** the first step that reports `reversible:false`.
- **Coordinator:** the farm's single process that decides which host updates next, and serves the API, the CLI endpoint and the UI.
- **Hold file:** the root-owned file that records a pin or a standing halt on a host.
- **Daemon:** the one root process on each host; speaks to the coordinator, executes the compiled verbs, runs hooks.
- **Deferred:** exit 75: nothing irreversible happened; try later; holds no slot.
- **Dead-man:** the thread that stops a step at its deadline.
- **Halt:** exit 76: stop the service everywhere until an operator clears it. A halt the updater raised itself is also lifted by a CI write of a new version.
- **Hook:** the executable in a release that implements the five steps; runs as root.
- **Kept release:** a release a host retains for rollback.
- **Live update:** an update from the order to the end of its bake.
- **`min_version`:** the lowest version of a service a host will ever run, set in the services file; the only downgrade bound.
- **Order:** a verb plus typed arguments, sent by the coordinator in a heartbeat reply.
- **Pin:** a root-owned hold that stops a host taking orders for a service.
- **Promotion pending:** the update succeeded but the hook cannot yet call it final; the coordinator holds the slot.
- **Release channel:** where a service's release archives are served (the entry's `release_url`).
- **Role:** a label on a host (1 to 32 bytes, `a-z0-9-`) used by `min_up` and assignment.
- **Quiet start:** the 30 seconds after coordinator start in which nothing is ordered.
- **Seal:** the digest of a staged release tree, written when it was verified.
- **Services file:** the root-owned file on a host naming the services it runs and the signer of each. Not the coordinator's rollout file.
- **Signer:** the repository and workflow pinned for a service; the release it attests is the only code that runs as root for that service.
- **Slot:** the single farm-wide permission to have an update live.
- **Step record:** the durable file that lets the runner resume.
- **Unmanaged:** a service whose services-file entry was refused at load.
- **Verb:** one of the fixed operations compiled into the daemon; an order is a verb plus typed arguments.
