# HostGroups: every deployment targets a HostGroup

Status: design, for the maintainers' ruling. Build items HG1 to HG3 of section 12 are built, and `docs/operations/hostgroups.md` says what the coordinator does with groups now; the other items are not. It replaces every path by which the coordinator places a service on a host today: the roles of the rollout file and its `selector` (`hostkeeper-design.md` sections 4.3 and 5.4, `rollout-update-plans.md` section 1.1), the operator's assignments per host and per role (`Services.Assign`, `Services.Unassign`), and the services an enrolment approval names (`hostkeeper-design.md` section 5.7, `network-install.md`, `hostkeeper-mdm.md`). It moves the rollout policy of the rollout file and the per-role limits of the desired state (`rollout-update-plans.md` sections 1.1 to 1.6) onto the HostGroup. It adds config as an ordinary package, delivered from a private repository of the coordinator's repository registry. Where those documents disagree with this one, this one wins; each of them points here at the place it is superseded. Section numbers like "rollout-update-plans.md section 1.3" are that file's; a bare "section 3" is this file's.

It builds on two other changes. The manifest's hook command (komira-ai/hostkeeper#264, built): a package names its own hook program, run from its sealed tree, and hostkeeper puts no rule on a package's shape. The coordinator's repository registry (designed in its own change, not yet merged): repositories are configured on the coordinator (`Repositories.List`, `Get`, `Put`, `Remove`), each service names its repository, and the coordinator pushes to each host the repository entries and keys its services need. Section 6.6 says what this design adds to that registry, and nothing else of it is restated here.

## What changes for the operator

**Before.** A host runs a service when the rollout file lists the host's role for it (or its `selector` picks the host), unless an operator entry for the host or its role says otherwise (`docs/operations/assignments.md`); an approval can also name services, which are written as host entries. The role comes from the host's certificate. A desired version is set per service, for every host the service is placed on, and its `max_updating` and `min_up` are per role. The plan, the canary, the bake and the rollback policy are per service, in the rollout file. A service's configuration is the host's own business: host setup, or files a service's package ships under `/etc`.

**After.** The coordinator holds a set of **HostGroups**. A HostGroup has a name, the ordered list of the packages its hosts run, and one rollout policy. Every enrolled host is in exactly one HostGroup: the approval or the inventory record names it, and `HostGroups.Move` moves a host, in one audited call, which installs what the new group lists and removes what only the old one listed. A host runs exactly its group's packages; there is no other way to place a package on a host. **Every deployment targets a HostGroup**: a desired version is (host group, package, version), set by CI or an operator with `Releases.SetDesired`, and it rolls over that group's hosts by that group's policy. `hostkeeper` itself is listed in every group. A service's configuration for a group is an ordinary package (`tierstore-config-web`, say), built by the operator's own CI from a private repository, published to a private repository in the coordinator's registry that only hosts can read, and deployed to its group like any package, with the same stages, canary, rollback and audit; its hook reloads or restarts the service. A host's own values (its id, its group, its labels) are in a facts file hostkeeper writes; hostkeeper templates nothing. No package carries a secret.

**What does not change.** The decision rules and their order (`hostkeeper-design.md` section 5.4, `rollout-update-plans.md` section 1), the plan engine, the bake, rollback and its threshold, deferral, the one update at a time on a host, the installers and the hook contract, the package trust (the repository signature and the digest pin), the enrolment protocol and the certificate, the operator and CI tokens, and the frozen JSON routes' bytes. Hold, exclude, abort and force stay per host, the tools of an incident. Nothing on a host changes but the facts file and the private repository's entries.

## 1. The HostGroup

### 1.1 What a group holds

| Field | Meaning |
|---|---|
| `name` | 1 to 32 bytes of `[a-z0-9-]`, a letter first, no `--`, not ending in `-`: the grammar of a role name today (`hostkeeper-wire` `RoleName`), so that every role name is a valid group name (section 10). |
| `packages` | 1 to 64 entries, in order, each a package (a service name as today, `A-Za-z0-9._-`, 1 to 255 bytes). No package twice. `hostkeeper` is one of them (section 1.3). The order is the order in which a host is offered its packages (it replaces the rollout file's `priority`) and the order of installs; uninstalls run in the reverse order. |
| `policy` | The rollout policy of every package of the group (section 3). |
| `generation` | Raised by every change of the group. A `Put` that replaces a group names the generation it read (`if_generation`) and is refused `generation_mismatch` when it is absent or the group moved on; a `Put` that creates one names none. |

A package's minimum version in a group is not part of the group's body: it is per (group, package) state written only by `Services.SetMinVersion` (section 4), which a `Put` neither reads nor resets, so an incident's minimum is never undone by an operator applying their file.

A group lists packages, not versions. The versions are the deployments of section 2. A package can be listed in many groups; a host runs it at its own group's desired version.

Nothing in a group names a host. Membership is the host's (section 1.2). Nothing in a group names an installer or a platform either: a group of Macs lists the packages a Mac installs, and a host whose installers cannot install a listed package is told `no_target` for it, as today.

### 1.2 Membership: each host in exactly one group

Every enrolled host has one group, kept by the coordinator with the host's enrolment record (section 8). Nothing else decides it: not the certificate, not a label, not a role.

- **At approval.** `Enrollment.Approve` names the group (`host_group`); it must exist (`unknown_host_group`). An approval names no services: an approval with any `services` is refused `services_retired`, since the group decides what the host runs. The approval's audit reason names the group (`group web`).
- **From the inventory record.** `Enrollment.AddRecord` names the group (`host_group`), which must exist (`unknown_host_group`). A record that names a group is the approval of the host it matches, with the window of today (`docs/operations/enrollment.md`); the coordinator's automatic approval puts the host in the record's group. A record that names no group has no window, as a record with no role has none today. A group cannot be removed while a record, a pending approval or the Mac allowlist names it (section 7.1), and the existence check of `Approve` and `AddRecord` and the emptiness check of `Remove` run under the one store lock, in one transaction on Postgres, so no approval lands in a group being removed.
- **The Mac allowlist.** Each `[[mac]]` entry of `--mac-serial-allowlist` names `host_group`. An entry that names `services`, or a group the store does not hold, is refused when the file is read at start, naming the key or the group.
- **A renewal** keeps the host's group. Moving a host is `HostGroups.Move` (section 5), never a renewal or a re-approval. A re-approval with `replace` keeps the group of the host it replaces; one that names another group is refused `host_group_differs`.
- **Retire** (`Hosts.Retire`) ends the membership with the host.

The certificate is the host's identity only. The coordinator never reads a placement from it. The shipped certificate readers require one host-role URI (`hostkeeper-wire` `identity.rs`), so the issuer writes the fixed host-role `host` into every certificate it issues from this release on; it means nothing, and a certificate issued before, with a role in it, keeps working and is read for its host id alone.

### 1.3 hostkeeper is in every group

`HostGroups.Put` refuses a group whose packages do not include `hostkeeper` (`hostkeeper_not_listed`). hostkeeper's desired version is therefore set per group, like any package's, and rolls by the group's policy: a release can go to a canary group first and to the others after it, with the waits of section 2.1. The rules that are hostkeeper's own stay as they are: force refuses it (`self_service`), its list per platform is one native entry, and it updates itself through its package after the new binary confirms (`packages-native.md` section 10.4).

### 1.4 What a host runs

A host runs exactly the packages its group lists, each at the group's desired version for it (section 2). This replaces membership by assignment (`membership.rs`, `assignment.rs`) with one lookup: the host's group, then the package's place in its list. Everything after membership in the decision is as before: halt, hold, quiet start, bad version, minimum version, one update at a time on a host, up to date, failed here, deferral, the plan gate, the slot and `min_up`.

- A package the group lists that the host does not run is installed (an `install` order, as an assignment gives today; agent features `install-v1` and `heartbeat-v2`). **Installs follow the list:** a host is offered the install of a package only when every package listed before it is installed on the host (at any version) or cannot be (`no_target`, `no_desired`); until then the later one waits, told `install_order`, and takes no place in its plan. So a config package is never ordered to a host before the service its `Depends` names, whichever stage each plan is in. Updates are not ordered by the list: the `Depends` ranges and expand-then-contract (section 6.3) order them.
- A package leaves a host only by an uninstall intent, written by a `Put` that takes the package out of the group's list (for each host of the group that listed it at its last heartbeat) or by a move (section 5). It is ordered and paced exactly as an uninstall intent is today (`docs/operations/assignments.md`, Uninstall): under the halt, the hold, the quiet start, one update at a time on the host and the group's other live rollouts, taking a slot of the group's `max_updating`, not a stage. A `Put` that takes out a service and its config package together orders the config package's uninstall first, by the reverse of the list. A package the host runs that its group does not list and no intent covers (installed by hand, or left by an older release) is never ordered; `Hosts.Get` shows it among what the host runs, as today.
- A group with no desired version yet for a package it lists orders nothing for it; `Releases.Status` shows the pair as `no_desired`.

## 2. Deployments: (host group, package, version)

### 2.1 Setting a desired version

`Releases.SetDesired` gains `host_group`. A call that names a group sets the desired version of that package in that group, and nothing else: one deployment, one audit entry `set_desired` naming the group, one plan over that group's hosts. The package must be listed in the group (`not_in_host_group`): CI deploys versions, operators decide what a group runs.

A call that names no group is the call every client made before this design, and the frozen route `POST /v1/desired` makes. It is read as one deployment to each group that lists the package, written in one change: one desired version per group, one `set_desired` audit entry per group, each its own plan. A package no group lists is refused `not_in_host_group`. `if_generation` with no group is accepted only when exactly one group lists the package, and is refused `ambiguous_host_group` otherwise, since there is no one generation to compare. This keeps every shipped client working (section 9); `hkctl`, `set-desired.yml` and every document name the group.

Promotion across groups is a caller's sequence of deployments, never a coordinator rule: set the canary group, `Releases.WaitConverged` on it (which gains `host_group`), then set the next. `set-desired.yml` gains the input `host_group`, and its callers chain jobs for the order they want.

`Releases.Converge`, `WaitConverged` and `GetRelease` gain `host_group`; with none, `Converge` and `WaitConverged` answer for every group that lists the package and has a desired version for it (converged when all are), as a call without a group sets them all. A group with no hosts is converged. A pair with no desired version is left out of the answer's verdict and shown `no_desired`; when no group has a desired version for the package, `WaitConverged` answers `no_desired` at once, without waiting.

The desired state's `max_updating` and `min_up` maps move to the group's policy (section 3). A `SetDesired` whose `desired` carries either map is refused `limits_in_host_group`; no shipped workflow sends them (`set-desired.yml` sets the version and the targets only).

### 2.2 CI tokens

An entry of the OIDC allow-list keeps `services` and gains an optional `host_groups`: the groups the token may deploy to. An entry without it may deploy its services to any group that lists them, as today it may deploy them to every host. A call without a group from a token limited to some groups is read as one deployment to each group the token may deploy to that lists the package.

### 2.3 What is per (group, package) and what is per (package, version)

| Kept per (host group, package) | Kept per (package, version), for every group |
|---|---|
| The desired version and its generation; `last_good`; the halt and its generation; the plan's position, its pause and its done hosts; a standing force; the rollback (it sets the group's desired version back to the group's `last_good`) | The release (its targets and digests, the registry of versions CI set, at most 4096 a package); a bad mark |

A version marked bad is bad in every group: a build that fails its canary group must not roll on to the next one. A config package's name is its group's (section 6.1), so its bad marks are its group's alone.

## 3. Rollout policy per group

The policy is the rollout file's per-service keys and the desired state's per-role limits, made one table per group:

| Key | Default | From |
|---|---|---|
| `stages` | `[1, 10, 50, 100]` | the rollout file (`rollout-update-plans.md` section 1.1) |
| `canary = { count }` | none | the rollout file |
| `bake_beats` | 0 | the rollout file |
| `rollback` | `manual` | the rollout file |
| `rollback_after_failed = { percent, min_hosts }` | 25, 2 | the rollout file |
| `defer_backoff = { initial_secs, factor, max_secs }` | 300, 2, 3600 | the rollout file |
| `max_updating` | 1 | the desired state's per-role map; 0 holds the group |
| `min_up` | 0 | the desired state's per-role map |
| `live_rollouts` | the coordinator's `--live-rollouts` (default 1) | the coordinator flag, now counted within the group |

The ranges and refusals are the rollout file's and the desired state's of today, checked by `Put` (`invalid_policy`, naming the key in the audit and the log). Each package of the group rolls by this one policy: stages are computed over the group's eligible hosts for that package, `max_updating` caps the hosts of the group updating that package at once, `min_up` counts the group's live hosts up for it, and `live_rollouts` is how many of the group's packages may roll at once. Groups do not wait on each other: a host is in one group, so two groups' rollouts never touch the same host. `rollout_elsewhere` names the other packages of the same group that hold the place.

A group's policy changed by `Put` applies at the next decision, as a restart with a new rollout file does today: a plan keeps its position, and its stages are recomputed.

## 4. Incidents: hold, exclude, abort, force stay per host

These are the tools of an incident, and they stay what they are, per host:

- **Hold** (`Services.Hold`, `Unhold`) of a host or of the whole fleet, as today. Its role scope names a group: a hold of role `web` holds the group `web`'s hosts.
- **Exclude** (`Hosts.Exclude`, `Include`) per (package, host); **abort** (`Hosts.Abort`) per (host, package); unchanged.
- **Force** (`Releases.Force`) gains `host_group`: a force sets the group's desired version, so it is a deployment to that group. Its targets are `hosts` (each must be in the group, else `unknown_target`) or `fleet`, the whole group (with `confirm_service` as today). Its `role` target names the group; a `role` and a `host_group` that differ are refused `unknown_target`. A force without `host_group` is read as one force per group: with `hosts`, one for each distinct group of the named hosts (an unknown host refuses the whole call); with `fleet` or `role`, one for each group that lists the package (or the named one). Every form is written in one change, with one audit entry per group.
- **Minimum version** (`Services.SetMinVersion`) of a host stays; of a role, it sets the minimum version of the package in the group of that name (refused `unknown_host_group` when no group has the name, `not_in_host_group` when the group does not list the package). It is the only writer of a group's minimum (section 1.1), audited `set_min_version`, and does not raise the group's generation.
- **Pause and Advance** (`Rollouts.Pause`, `Resume`, `Advance`) gain `host_group`. A pause without a group pauses the package's plan in every group that lists it, in one change (the incident's stop); an advance or resume without one is accepted only when one group lists the package, and refused `ambiguous_host_group` otherwise.

## 5. Moving a host

`HostGroups.Move { node_id, host_group, reason }` moves an enrolled host into another group in one call, with one audit entry `host_move` (by the operator; the entry's `node_id` is the host, its new `host_group` the group, its reason "from web to web-canary: " followed by the operator's reason). The host is in the new group from that write: `Hosts.Get` and the groups' counts say so, and the old group's plans stop counting it. It is refused `unknown_host`, `unknown_host_group`, `same_host_group`, or `audit_unavailable` (nothing changes).

A moved host is **moving** until its packages match its new group. The move changes what the host runs as one sequence, so the host never waits, half moved, without a config its service needs:

1. **Admission.** The sequence starts only when the new group's plans admit the host for every package it must install or change: each such plan has finished (every eligible host rolled) or has a free place in its current stage, and none is paused, halted or at a version marked bad. Until then the host runs what it ran, is ordered nothing, and `Releases.Status` shows it `moving` with the plan it waits for. When it is admitted it takes those places, as a host that joins takes one (`rollout-update-plans.md` section 1.7).
2. **Facts.** The coordinator sends the host its new facts (section 6.4) and orders nothing more until the host acknowledges them.
3. **Uninstalls.** Each package the host runs that the new group does not list is uninstalled, in the reverse of the old group's list: a config package before the service it configures. The local mirror of the old group's private suite is deleted with the last of them (section 6.6).
4. **Installs and updates.** Each package of the new group, in its list order (section 1.4): one the host does not run is installed at the new group's desired version; one it runs at another version is updated to it. A downgrade is an ordinary update (the `apt` backend installs with `--allow-downgrades`).

Steps 3 and 4 run back to back, one order at a time as every host runs them, under the gates of today (one update at a time on the host, the quiet start); they take no further place. A pair the operator holds on the host is not touched: its package stays as it is, removed or not, until the hold is released, and the move goes on around it. A move does not cut an update running on the host: it finishes and reports, and the sequence waits for it. A host's exclusions, holds and minimum version are its own, and stay with it. A standing force of the old group no longer covers it. A move of a host that is silent is recorded, and the sequence runs when it is heard. A second move while one is still running replaces the target: the sequence starts again from admission into the newest group.

The cost of uninstalling first is a gap of one or two orders, between a config package's uninstall and its successor's install, in which `/etc/<service>/` is absent and the service runs on what it read; a restart of the service inside that gap would start it on its defaults. The admission rule bounds the gap to the orders themselves, never a stage's wait. A config package's `uninstall` step does nothing to the service (section 6.2), so the gap restarts nothing.

## 6. Config is a package

hostkeeper stays packages-only. A service's configuration for a group is a package like any other: built by the operator's CI, published to a repository, named in the group's list, deployed to the group by `SetDesired` with targets pinned by digest, staged, activated, verified, rolled back and audited by the same code. hostkeeper adds no config kind, no templating and no per-host package.

### 6.1 Names and file ownership

- **One config package per (group, service)**, named `<service>-config-<group>` by convention (`tierstore-config-web`). hostkeeper puts no rule on the name (komira-ai/hostkeeper#264); the convention keeps every config package's versions, releases, bad marks and audit its group's own.
- **The service package ships its defaults outside `/etc`** (`/usr/share/<service>/`, say) and nothing under `/etc/<service>/`.
- **The config package owns `/etc/<service>/`.** It ships its files there as ordinary files, not conffiles, so that each version replaces them whole and `dpkg --purge` removes them (the `apt` backend installs with `--force-confold`, which would keep a hand-edited conffile). A hand edit is overwritten at the next deployment.
- **Two config packages of one service never install together.** Each declares `Provides: <service>-config` and `Conflicts: <service>-config`. The `apt` backend's check at `stage` refuses an install that would remove another package (`dependency_changes`), and a move uninstalls first (section 5), so the conflict is never resolved by apt behind hostkeeper's back.
- **Its manifest** has `package`, the hook command, and no `run` units of its own (the service's package names those). Its `params` are scalars as for any package and are not secret (`packages.md` section 4).

- **On a Mac** a config `.pkg` installs under `/etc/<service>/` too (on macOS, `/private/etc`), its identifier `<service>-config-<group>`. The `macos-pkg` installer has no `Provides`, `Conflicts` or `Depends`: that two config packages of a service never coexist holds by the move's order (section 5) and by one config package per group, and compatibility with the service (section 6.3) holds by the order of deployments alone, with the hook's `verify` as the check.

An operator's repository keeps one directory per (group, service), `config/<group>/<service>/`, and its CI builds each into its package. That layout is the operator's; hostkeeper reads only the packages.

### 6.2 The hook: reload or restart

A config package's manifest names its hook program (`hook.command`, komira-ai/hostkeeper#264), run from the package's sealed tree with the unchanged contract. A config hook is small: `cutover` reloads the service (`systemctl reload tierstore`) or restarts it, `verify` checks the service took the new config (its health URL, or its own check of the files), `rollback` reloads again after the previous version's files are back, and `uninstall` does nothing to the service: the package's files go with the purge, and the service keeps running on what it read until a successor's `cutover` (section 5) or its own restart. A config package that names no hook gets the built-in steps, which restart the package's own `run` units: it has none, so it would change files and restart nothing. A config package therefore names a hook.

A failed verify rolls the host back to the previous config, and enough of them mark the config version bad and halt its plan, as for any package. A config change is canaried like a code change.

### 6.3 Version compatibility between a config package and its service

- **The config package declares the service versions it works with**, as Debian dependencies: `Depends: tierstore (>= 2.4), tierstore (<< 3)`. hostkeeper reads no `Depends` and adds no compatibility field: the `apt` backend's check at `stage` refuses an install whose dependency is not already on the host (`dependency_missing`), and an update of the service that would break an installed config package's dependency (apt would remove the config package: `dependency_changes`). Both are `Failed`, class `environment`, reported in `Releases.Status`, and roll nothing on.
- **The group lists the service before its config package**, so a host that installs both (an enrolment, a move) installs the service first.
- **When both change, expand then contract.** A service release that needs a new config shape lands in three deployments to each group: (1) a config version that both service versions accept, its `Depends` range covering both; (2) the service release, which its `Depends` now admits; (3) a config version that drops the old shape, its `Depends` raised to the new service. A step that is out of order fails at `stage` on the first host of its plan, with the reason above, and goes no further.

### 6.4 Per-host values: the facts file

A host's own values are in a file hostkeeper writes: the daemon's new `--facts-file`, by default `/var/lib/hostkeeper/facts.json` on Linux and on a Mac, owned by root, mode 0644 (a service runs as its own user and reads it), written whole (a new file, then a rename) when its content changes. Tests point the flag into the repository's scratch directory.

```json
{"schema": 1, "host_id": "web-04", "host_group": "web", "platform": "linux-x86_64", "labels": {"rack": "r12"}}
```

- `host_id` and `host_group` from the coordinator's enrolment record; `platform` from the daemon; `labels` from `Hosts.SetLabels` (at most 8, as today).
- The coordinator sends them in the heartbeat's answer (`HeartbeatResponse.facts`, new) to a daemon that announces `facts-v1`, with a generation that a move, `Hosts.SetLabels` and an approval raise. The daemon writes the file before it acts on any order of that answer and reports the generation it wrote in each heartbeat (`HeartbeatRequest.facts_generation`, new). The coordinator sends the facts in every answer until the host reports their generation, and orders a moving host nothing until then (section 5).
- A service, or its config hook, reads the file. A config package that needs values that differ by host carries them for every host of its group (a table keyed by host id) and the service picks its own by `host_id`. hostkeeper renders nothing.
- A change of a fact runs nothing: hostkeeper restarts and reloads nothing for it. A service reads its facts at its start and at its config package's cutover; a label that must take effect at once ships with a config deployment.
- `schema` is raised only for a change a reader of schema 1 would misread; a field added is not one, and readers ignore fields they do not know.

### 6.5 No secrets in any package

No package, config or not, carries a secret: a package is read by every host of its group, kept on every host in its sealed tree (mode 0444), mirrored, and its manifest is unattested and audited by digest. A config file names a secret by reference (`db_password = { secret = "tierstore/db" }`) and the service reads it at run time from the site's secret store, with a credential that the secret store issues to the host or the service. hostkeeper delivers no secret material and holds no credential for a secret store; the facts file gives the service the host id and group the store's own policy may key on. The operator's config CI refuses to build a package from files that hold a secret (its own check, before publishing); a private repository is not a place for secrets either, only for configuration that is not public.

### 6.6 The private repository

Config packages are never published to the public repository. They go to a **private repository**: an entry of the coordinator's repository registry with `access = "hosts"` (new; the default `public` is every entry of today).

- **Origin.** The operator's CI publishes the private repository to an origin the coordinator can read and hosts cannot: an `https` origin with a credential the coordinator holds (`credential_file`, a root-owned 0600 file on the coordinator's machine, named in the entry, never sent to a host). It is signed with its own key, never the public repository's; the entry pins its fingerprint as any entry does.
- **Layout.** One suite per group, `dists/<group>/`, and one pool per group, `pool/<group>/`: a group's packages are listed in its suite only, so a host learns nothing of another group's configuration, not even its package names.
- **Authenticated host reads.** A host reads the private repository only through the coordinator, over its mutual-TLS connection with its enrolled certificate: `GET /repositories/<name>/dists/<group>/...` and `/repositories/<name>/pool/<group>/...` on the agent listener. The coordinator serves a host the suite and pool of the host's own group and refuses any other path (404, the same as a path that does not exist), and reads the bytes from the origin with its credential. It does not parse or sign anything: the suite's `InRelease` is the origin's, and the host verifies it with the pushed keyring, as for a public entry.
- **On the host.** The coordinator pushes the private entry like any other, with the host's group as its suite. The daemon fetches the suite's index files and the pinned package over its coordinator connection into a local mirror, `/var/lib/hostkeeper/mirror/<repository>/` (apart from the per-service apt work directories under `/var/lib/hostkeeper/apt/`), and apt reads it with a `file:` URI (one of the two schemes the repository loader accepts today). The mirror holds the suite's `InRelease`, the index files it lists for the host's architecture, and the pool files of the orders staged from it; it is fetched into a new directory and renamed over the old one whole, its directories 0755 and files 0644 so apt's unprivileged fetch user can read them. A fetch whose files do not match the `InRelease` it fetched first (the origin published between two reads) is fetched again once and then defers the order (`fetch_failed`). The mirror is deleted when the host leaves the group (section 5). apt's trust chain is unchanged: `InRelease` signed by the pinned key, the `Packages` digest, the `.deb` digest, and CI's digest pin on top. apt is given no client certificate and the host's key never leaves the daemon.
- **A moved host** gets its new group's suite with its next push; the old suite is no longer served to it.
- **A Mac.** A config `.pkg` is an artifact entry fetched by digest (`packages-repositories.md` section 4.7) from the store `coordinator` (new kind): its locator is a path `/repositories/<name>/pool/<group>/<file>.pkg`, which the daemon fetches from the coordinator over its connection, served by the same route under the same rule of the host's own group's pool; the daemon checks the entry's digest before it reads a byte, as for any artifact. The `.pkg` is signed by a signer the host's `macos-pkg.toml` lists, as for any `.pkg`.

### 6.7 Where config is checked

A config mistake is fixed in the config repository and found by its own CI, before publishing: the operator's CI builds the package and runs the service's own config check on it (`tierstore check-config`, say). At deployment, the canary of the group's plan and the hook's `verify` are the check; a failure rolls back and halts as for any package. hostkeeper adds no config gate, dry run or counter.

## 7. The operator API and the generated CLI

### 7.1 The `HostGroups` service

A new file, `crates/hostkeeper-proto/proto/hostkeeper/operator/v1/hostgroups.proto` (`operator.proto` is over 1000 lines), with the standard header, in `build.rs`'s `FILES`. Every call takes the operator token only (a CI token is refused `forbidden`, no token `unauthorized`), is unary, and has a request of at most 4 KiB but `Put`, at most 64 KiB. Each mutating call needs a reason (1 to 256 bytes, no control character) and answers the audit entry it wrote. A call that cannot write its audit line changes nothing (`audit_unavailable`).

| Call | Does | Refused (beyond `unauthorized`, `forbidden`, `invalid_request`, `store_failed`, `audit_unavailable`) |
|---|---|---|
| `List` | Lists the groups, with each one's package list, policy, generation, host count, and per package its desired version (paged: `limit`, `page_token`, `next_page_token`). | none |
| `Get` | One group, with, per package, its desired version, minimum version and convergence, and a page of its hosts (node ids; `limit`, `page_token`, `next_page_token`). | `unknown_host_group` |
| `Put` | Creates a group or replaces one whole (`if_generation` optional). A package taken out of the list is uninstalled from the group's hosts (section 1.4); one added is installed when its group has a desired version for it. Audited `host_group_put`, the reason naming what changed ("packages +tierstore-config-web; policy stages"). | `invalid_host_group` (the name), `invalid_package` (a bad name or a package twice), `invalid_policy`, `hostkeeper_not_listed`, `too_many_packages`, `generation_mismatch` |
| `Remove` | Removes an empty group, with its desired versions and plan positions. Audited `host_group_remove`. | `unknown_host_group`, `not_empty` (a host is still in it or moving into it, or an inventory record, a pending approval or the Mac allowlist names it) |
| `Move` | Moves a host (section 5). Audited `host_move`. | `unknown_host`, `unknown_host_group`, `same_host_group` |

The refusal enum is `HostGroupsRefusal`, values `HOST_GROUPS_REFUSAL_<NAME>`, each with its gRPC code (`unknown_*` `NOT_FOUND`; `generation_mismatch`, `not_empty`, `same_host_group` and `hostkeeper_not_listed` `FAILED_PRECONDITION`; the `invalid_*` and `too_many_packages` `INVALID_ARGUMENT`; `store_failed` `INTERNAL`; `audit_unavailable` `UNAVAILABLE`), as `tests/layout.rs` requires of every service. There is no rename: a group is renamed by putting the new one, moving its hosts and removing the old.

The gateway serves each call at `POST /hostkeeper.operator.v1.HostGroups/<Method>`, from the same handlers, as it does every operator call (`docs/gateway.md`). The CLI is generated from the proto: `hkctl host-groups list|get|put|remove|move`, with `--host-group`, `--node-id`, `--reason` and `--if-generation` as flags and the group's body of `put` as `--request <file|->` (a message), proto JSON as for every request file. An operator who keeps the groups in a file of their own repository (`hostgroups.toml`, one table per group, its keys the message's fields) converts it to the request and applies it with `hkctl host-groups put`; the UI's Rollout page gets a HostGroups view from the same generated client.

### 7.2 Changes to the shipped messages, old -> new

The wire stays additive: no field, message, enum value, call or service is renumbered, renamed, retyped or removed, and the compatibility check against `crates/hostkeeper-proto/proto/compat/baseline.binpb` keeps passing. A field or call this design retires stays in the proto, marked deprecated in its comment, and answers as the table says. What the wire's additivity does not cover, the calls now refused and the fields now read differently, is listed in section 9.1 for the maintainers' ruling. Every refusal named below is a new value of the call's existing refusal enum (`ServicesRefusal`, `EnrollmentRefusal`, `ReleasesRefusal`, `RolloutsRefusal`), never a new enum.

| Item | Old | New |
|---|---|---|
| `Services.Assign`, `Services.Unassign` | Write a host or role entry | Refused `placed_by_host_group` (`FAILED_PRECONDITION`); `Put` and `Move` are the placement |
| `Services.List` `ServiceEntry.roles`, `.assignments` | The service's roles and entries | Empty; new `host_groups` lists the groups that list the package |
| `Enrollment.ApproveRequest.role`, `.services` | The host's role; services assigned to it | New `host_group`. `role` is read as the group's name when `host_group` is absent; any `services` is refused `services_retired`; a `replace` naming another group than the replaced host's is refused `host_group_differs` |
| `Enrollment.AddRecordRequest.role`, `InventoryRecord.role` | The record's role | New `host_group`; `role` is read as the group when `host_group` is absent, and is written as the group's name in answers, for old readers |
| `PendingEnrollment.record_role` | The record's role | New `record_host_group`; `record_role` carries the same name |
| `Host.role` | The certificate's role | New `host_group`; `role` carries the group's name, for old readers |
| `Releases.SetDesiredRequest` | `service`, `desired`, `targets` | New `host_group`; without it, section 2.1 |
| `SetDesiredResponse.dropped_roles` | Roles whose limits were dropped | Always empty |
| `DesiredState.max_updating`, `.min_up` | Per-role limits | Refused when set (`limits_in_host_group`); the group's policy holds them |
| `ConvergeRequest`, `WaitConvergedRequest`, `GetReleaseRequest` | By service | New `host_group`, optional (section 2.1) |
| `ServiceStatus` (in `Releases.Status`), `RoleCounts` | Per service, hosts grouped by role | One per (group, package), new `host_group`; `RoleCounts` keyed by the group's name |
| `ForceRequest` | Targets `fleet`, `role`, `hosts` | New `host_group`; `fleet` is the whole group, `role` names a group (section 4) |
| `HoldScope.role`, `MinVersion.role` | A role | Read as a group's name (section 4) |
| `Rollouts.AdvanceRequest`, `PauseRequest`, `ResumeRequest`; `RolloutPlan` | By service | New `host_group` (section 4) |
| `PlanHost.role` | The host's role | The host's group's name |
| `AuditEntry` | `service`, `node_id`, `reason` | New `optional string host_group`; `Audit.List` gains the filter `host_group` |
| Audit actions | | New `host_group_put`, `host_group_remove`, `host_move` |
| `agent.v1.HeartbeatResponse`, `HeartbeatRequest` | | New `HostFacts facts` in the answer and `facts_generation` in the request; the daemon feature `facts-v1` |
| `HostGroups` service | | New (section 7.1) |

Coordinator configuration, old -> new: `--rollout-file` is refused by `serve` once this is built, naming the migration verb (section 10); it is read only by `hostkeeper-coordinator host-groups migrate`. `--live-rollouts` stays, as the default of a group's `live_rollouts`. The OIDC allow-list gains `host_groups` (section 2.2). The Mac allowlist's `role` is read as `host_group`, which it gains; its `services` is refused at start. The repository registry's entry gains `access` and `credential_file` (section 6.6).

## 8. Storage

On Postgres, one migration, `0008_host_groups.sql` (`0007` is the repository registry's), `-- older-releases: refuse` (a coordinator that does not know groups would place by roles over a fleet that has none):

- `host_groups(name PK, generation, policy jsonb, by_name, at_unix)`, `host_group_packages(host_group, service, position, PK (host_group, service))`; `group_min_versions(host_group, service, version)`, written by `SetMinVersion` alone (section 1.1).
- `group_desired(host_group, service, version, generation, write_id, replaced_version, PK (host_group, service))`; `group_services(host_group, service, last_good, halt_generation, force_generation, force jsonb, halt jsonb, halt_acked jsonb, halt_seen jsonb)`; `group_rollouts(host_group, service, version, entered, paused_by, paused_reason)` and `group_rollout_done(host_group, service, node_id)`, the plan's position keyed by (host group, service). The release registry (`desired_digests`, `releases`) and `bad_versions` stay keyed by (service, version).
- New nullable columns: `enrollment_hosts.host_group`, `enrollment_hosts.moving_from` (the old group while a move's sequence runs, section 5), `enrollment_hosts.facts_generation`, `enrollment_requests.approved_host_group`, `inventory.host_group`, `audit.host_group`. Uninstall intents keep today's table, `uninstall_intents`.
- The tables of today's placement and per-service state (`desired`, `assignments`, `min_versions` rows of role scope, `rollouts`, `rollout_done`) stay, are not read or written after the migration of section 10, and are not dropped (the migration lint refuses `DROP`; taking them away is a later release's own change).

On the file store: the state file gains a top-level `host_groups` key (the groups, their packages and their desired versions), written while a group is held, which a release before refuses at start, as its strict reader refuses any key it does not know (the state file has no `schema`); the ops file gains a top-level `host_groups` key, each (group, package)'s marks (its minimum version, last good version, halt with its reports and generation, force, plan position), under the same rule; an inventory record and an audit entry gain `host_group`; the enrolment store's `Approval` and `Host` (`store.proto`, private to the crate) gain `host_group = 9` and `host_group = 11`, and `Host` also gains `moving_from = 12` and `facts_generation = 13`, the fields of the columns of the same names.

## 9. Compatibility and failure modes

- **Old operator clients and CI.** Every shipped call keeps its fields: a call that names no group is read as sections 2.1 and 4 say; a role names the group of that name, and the migration names groups after roles (section 10), so a script that names a role keeps working. Calls whose purpose is gone are refused by name, never ignored: a client is told, never given a silent other answer (section 9.1).
- **The frozen JSON routes.** `POST /v1/desired` is a `SetDesired` with no group (section 2.1); `GET /v1/convergence` answers for every group that lists the legacy service. Their bytes are unchanged; a group appears in no field of them.
- **Old daemons.** A daemon without `facts-v1` gets no facts and writes no file; everything else it is sent is as today, since placement is the coordinator's. A host whose group lists a config package from a private repository needs a daemon that mirrors private entries; an older one is told `no_target` for that package and runs the rest.
- **An older coordinator on the new store** is refused at start (migration 0008 says `refuse`; the state file's and the ops file's `host_groups` keys). Going back across this change is restoring the backup taken before the migration (section 10), losing what changed since.
- **The coordinator down.** Hosts keep running what they run; nothing is ordered. A host cannot fetch from the private repository while the coordinator is down, so an install or update of a config package defers (`Deferred`, `environment`, `fetch_failed`, as an unreachable repository does today) until it is back. A public package is fetched from its origin as today.
- **The private origin down or refusing the coordinator's credential.** The coordinator answers the host 502 and logs the origin's status; the host's stage defers the order (`fetch_failed`), as for an unreachable repository. No secret or credential is in the log line.
- **A group without hosts** rolls nothing; its deployments are recorded and roll when a host joins.
- **A package listed in a group whose hosts' installers cannot install it** (no target for the platform) is `no_target` on those hosts and blocks nothing else of the group.
- **A host whose group's private suite does not exist yet** gets `not_in_repository` (`Deferred`) for its config packages, as for any package not yet published.

### 9.1 What is not additive, for the maintainers' ruling

The wire stays additive (section 7.2), and the coordinator's own store is internal; but the ruling that removes every other placement path cannot be met by additions alone. These are the departures from "formats change additively only" (B8), each with why no additive form exists:

| Departure | Why it cannot be additive |
|---|---|
| `Services.Assign` and `Unassign`, an approval's `services`, the Mac allowlist's `services` are refused | Accepting them would keep a second placement path, which the ruling removes; ignoring them would answer a client with a silent other result. |
| `DesiredState.max_updating` and `min_up` are refused when set | The limits are the group's policy; honouring them per call would be a second policy path. No shipped workflow sends them. |
| Role fields (`Host.role`, `HoldScope.role`, `MinVersion.role`, `ForceRequest.role`, `PlanHost.role`, `RoleCounts`, `InventoryRecord.role`, `ApproveRequest.role`) are read and written as group names; `ServiceStatus` comes once per (group, package) | A role decides nothing after this change; reading the shipped field as the group of the same name is the reading under which old clients keep working, since the migration gives groups the roles' names. |
| `serve` refuses `--rollout-file` | A coordinator that read it would place by roles beside the groups. Refusing it at start makes a stale deployment fail loudly once, at deploy. |
| A release before refuses the migrated store (migration `0008` `refuse`; the state file's and the ops file's `host_groups` keys) | A release before would place by roles over hosts that have none. Going back is restoring the backup of section 10. The test that the previous release reads the new data is therefore replaced by the test that it refuses it, naming the migration. |

## 10. Migration in one update window

Today's roles, rollout file and assignments become groups in one window, with the hosts running throughout. The new coordinator's `serve` refuses `--rollout-file`; the conversion is one offline verb over the store:

`hostkeeper-coordinator host-groups migrate --rollout-file FILE <the store's flags> [--apply]`

1. **Before the window**, on a copy (`export --into-empty` of the store, or the backup of the database), run it without `--apply`. It prints what it would write, and changes nothing:
   - For each enrolled host, the set of services placed on it today (the rollout file's roles and selector, the role and host entries, exactly as `assignment.rs` decides now), plus `hostkeeper`.
   - The groups: the hosts of one role with the same set form one group named after the role; when the hosts of a role run different sets, the most common set takes the role's name and each other set is `<role>-2`, `<role>-3`, in the order of the node id of its first host. A derived name longer than 32 bytes, or one that is already a role's name, is an error naming the role. Each group's packages are the set, ordered by the rollout file's `priority` and then by name. Its policy is the rollout file's per-service keys when every service of the group agrees, and otherwise the most careful value of each key, each such choice printed: `stages` and `canary` of the service with the smallest first stage; the highest `bake_beats`; `manual` over `automatic`; the lowest `rollback_after_failed` percent and `min_hosts`; the longest `defer_backoff` of each field; and, from the desired state's per-role values for its role, the lowest `max_updating` and the highest `min_up`. `live_rollouts` is the coordinator's `--live-rollouts`, now counted within each group, so N groups may roll N packages at once where the fleet rolled one; the dry run prints both.
   - Each group's desired version of each package: the service's desired version of today, with its release, `last_good`, halt and force. The plan's position (the stage entered, the pause) is copied to each group of the service, and its done hosts go to the group each host is in; a group whose hosts did not reach that stage enters it as a plan does whose stages its hosts already fill (`rollout-update-plans.md` section 1.4).
   - Holds and minimum versions of a role scope, attached to the role's groups; host entries' exclusions, holds and minimum versions kept per host; uninstall intents kept. A service left on a host by an `Unassign` without `uninstall` is not in the host's set, has no intent, and stays as it is (section 1.4).
   - Inventory records and pending approvals naming a role, given that role's group; Mac allowlist entries, with the `host_group` and the removal of `services` each needs in the file.
   - Every host whose placement would change (none, by construction; a host whose role no group takes is an error naming it), and every entry it cannot carry over, as errors. An error makes `--apply` refuse.
2. **In the window**: stop the coordinator; take the backup (`docs/deploy/coordinator/README.md`); run the verb with `--apply`, which writes it all in one transaction (Postgres) or one commit of the files, with one audit entry per group, by `migration`; edit the coordinator's configuration (drop `--rollout-file`, add `host_group` to the Mac allowlist and drop `services`); start the new release. Its quiet start rebuilds the fleet from heartbeats, and no host is ordered anything it was not already to be ordered.
3. **After the window**: point CI at the groups (`set-desired.yml`'s `host_group`), put the config packages into their groups, and stop naming roles in scripts.

The window is the coordinator's stop, the verb and its start: minutes, with no host restarted and no service touched.

## 11. The existing documents

The design documents this supersedes point here now: `hostkeeper-design.md`, `rollout-update-plans.md`, `host-updates.md`, `coordinator-storage.md`, `network-install.md` and `hostkeeper-mdm.md`. The documents that describe what the code does today (`docs/operations/assignments.md`, `rollout-plans.md`, `enrollment.md`, `holds-and-force.md`, `network-install.md`, `mdm-package.md`, `coordinator-state.md`; `docs/cli.md`, generated; `docs/hello.md`, `docs/enrollment-coordinator.md`, `docs/ui-serving.md`, `docs/deploy/ci-client.md`, `github-actions.md`, `service-caller.md`; `README.md`; `examples/hello/rollout.toml`) keep saying it until the build item that changes the code changes them in the same pull request (section 12). `MIGRATION.md` gains an entry for the move from roles to groups with HG8. `docs/operations/assignments.md` is replaced by `docs/operations/hostgroups.md`.

## 12. Build items

| Item | What | Documents it changes |
|---|---|---|
| HG1 | The proto: `hostgroups.proto`, the fields and refusals of section 7.2, `additions_host_groups.rs`; `docs/cli.md` regenerated | `docs/cli.md`, `docs/gateway.md` |
| HG2 | Storage: migration 0008, the file store's schema, `store.proto`'s fields; the per-(group, package) state of section 2.3 | `docs/operations/coordinator-state.md` |
| HG3 | The coordinator: groups, membership, the decision by group (section 1.4), `HostGroups` calls, `SetDesired` and the other calls by group, policy per group, `Move` | `docs/operations/hostgroups.md` (new), `rollout-plans.md`, `holds-and-force.md` |
| HG4 | Enrolment: approval and records by group, the Mac allowlist, the fixed host-role in issued certificates | `docs/operations/enrollment.md`, `network-install.md`, `docs/enrollment-coordinator.md` |
| HG5 | The facts file: `HeartbeatResponse.facts`, `facts-v1`, the daemon's write | `docs/design/packages-native.md` (the host's files) |
| HG6 | The private repository: the registry's `access` and `credential_file`, the coordinator's serving of a group's suite and pool, the daemon's mirror, the `coordinator` store kind | `docs/deploy/package-repositories.md` |
| HG7 | CI: `set-desired.yml`'s `host_group`, the allow-list's `host_groups`, hostkeeper's and hello's releases per group | `docs/deploy/ci-client.md`, `github-actions.md`, `service-caller.md`, `docs/hello.md`, `examples/hello/` |
| HG8 | The migration verb and `serve`'s refusal of `--rollout-file`; the UI's HostGroups view | `MIGRATION.md`, `README.md`, `docs/ui-serving.md`, `docs/operations/assignments.md` (removed) |

HG1 lands first (interfaces before code); HG2 before HG3; HG8 last, with the release that operators migrate with. HG5 and HG6 are independent of HG3 and of each other.

## 13. Tests

The tests drive the coordinator over injected heartbeats and a clock the test sets, and the daemon over the repository's scratch helpers and fake package managers; none sleeps or asserts timing, none reaches a network but a loopback listener on port 0. Each build item names its tests in its pull request. The behaviours they must prove:

- A host runs exactly its group's packages: a package added to a group is installed on each of its hosts in list order, a later package waits `install_order` (taking no plan place) while an earlier one is not installed even when the earlier one's plan is at a later stage, one taken out is uninstalled paced by `max_updating` (a config package before its service when both go), a package installed by hand outside every group is never ordered, and no other call places one (`Assign` and `Unassign` refused `placed_by_host_group`).
- A deployment names a group and rolls only that group's hosts, by that group's policy; two groups at different versions of one package roll independently; a call without a group sets each group that lists the package in one change, one audit entry each; `if_generation` without a group is refused `ambiguous_host_group` when two groups list the package; `WaitConverged` without a group counts a group with no hosts as converged, leaves out a group with no desired version, and answers `no_desired` at once when no group has one.
- `SetDesired` of a package the group does not list is refused `not_in_host_group`; a CI token limited to groups cannot deploy to another.
- A version marked bad in one group orders nobody in another.
- `Put` refuses a group without `hostkeeper`, a bad name, a package twice, a bad policy key (each key's bound), 65 packages, and a replacement with a stale or missing `if_generation`; it leaves the group's minimum versions as `SetMinVersion` set them; `Remove` refuses a group with a host, a moving host, a record, a pending approval or an allowlist entry naming it.
- `Move` writes one audit entry; the host is ordered nothing while a plan of the new group it needs is paused, halted, at a bad version or has no free place (shown `moving`); once admitted, its facts are acknowledged before any order, the old group's packages are uninstalled in reverse order and then the new group's installed in order, back to back, with no stage wait between a config package's uninstall and its successor's install; a shared package is updated (a downgrade included) to the new group's version; a held pair is left as it is; a running update is not cut; a silent host's move runs when it is heard; a second move restarts from admission into the newest group; `same_host_group`, `unknown_host` and `unknown_host_group` change nothing.
- Approval and records by group: `services` refused `services_retired`, `role` read as the group, an unknown group refused by `Approve` and `AddRecord`, a `replace` naming another group refused `host_group_differs`, the record's group used by automatic approval, a Mac allowlist naming `services` or an unknown group refused at start.
- Policy per group: `max_updating` 0 holds the group, `min_up` counts the group's live hosts, `live_rollouts` counts within the group and never across groups.
- The facts file: written whole at `--facts-file` before the orders of the same answer, rewritten only on change, resent until its generation is reported, its generation raised by a move, `SetLabels` and an approval; an old daemon gets none.
- The private repository: a host gets its own group's suite and pool, a path of another group is 404 and logged, a Mac fetching another group's `.pkg` path gets 404, a moved host loses the old suite and its local mirror, a mirror fetch that does not match its `InRelease` is retried once and then defers, the origin's credential is never in a log line or an answer (asserted by exact equality of the lines), the host's apt verifies the origin's signature and the pin from the local mirror, and an origin that is down defers the order.
- Config compatibility: a config package whose `Depends` is not met fails `dependency_missing` on the plan's first host and goes no further; a service update that would remove its config package fails `dependency_changes`; an expand-then-contract sequence of three deployments succeeds.
- The migration verb: over a fleet of roles, host entries, role entries, selectors and per-role limits, it prints groups under which every host's placement is unchanged, merges each policy key as section 10 says, carries desired versions, plan positions, holds, minimum versions and uninstall intents, refuses `--apply` on any error (a host whose role no group takes, a derived name over 32 bytes or taken), and `--apply` writes it in one commit; `serve` refuses `--rollout-file`.
- Compatibility: a reader built from the baseline reads every new message and keeps the new fields (`additions_host_groups.rs`); the previous release's coordinator refuses the migrated store, naming the migration (section 9.1); the frozen routes' bytes are unchanged; a `Force` with `hosts` in two groups makes one force per group in one change, and one with an unknown host changes nothing.

## 14. Decisions

| Question | Decision | Why |
|---|---|---|
| A `SetDesired` with no group | One deployment to each group that lists the package, one audit entry each. | B8: shipped clients and the frozen `/v1/desired` route send no group, and refusing them would be a new required field. Each group still gets its own deployment and plan; every new client and document names the group. |
| The role fields of shipped messages | Read as the name of the group; written back as the group's name. | One rule for every shipped role field keeps old scripts working after a migration that names groups after roles, and adds nothing to remove later. |
| Calls whose purpose is gone (`Assign`, `Unassign`, approval `services`, desired-state limits) | Kept in the proto, refused by a named refusal; listed as departures from B8 for the maintainers' ruling (section 9.1). | Removing them would break the wire; ignoring them would give a silent other answer; accepting them would keep a second placement path. |
| Where placement lives | On the coordinator, by `HostGroups.Put` and `Move`; the rollout file is retired. | One way to place a package, audited, through the API every operator call uses. |
| A group's list holds packages, not versions | Versions are deployments by `SetDesired`. | Operators decide what a group runs; CI decides at what version, under its own token. |
| Policy per group, not per (group, package) | One policy per group. | The ruling's "rollout policy per group"; a package that needs another policy goes to a group of its own hosts. A second, per-package table would be a second way to say the same thing. |
| Bad marks | Per (package, version), across groups. | A build that failed its canary group must not reach the next. |
| `live_rollouts` | Counted within a group; groups never wait on each other. | A host is in one group, so two groups' rollouts never share a host. |
| Order of a move | Admission into every plan of the new group it needs, then facts, then uninstalls (reverse order), then installs and updates (order), back to back. | Two config packages of one service never coexist, a config package never outlives its service, and the gap without config is bounded by two orders, never a stage's wait. |
| A moved host and the new group's plans | It takes a place in each, as a host that joins, before it changes anything. | A move must not skip the canary and stages of the group it joins, and must not half-move a host while a plan waits. |
| Install order across packages | An install waits (`install_order`) until every earlier package of the list is installed; updates are not ordered by the list. | Each package has its own plan, so the list alone would not keep a config package from reaching a host before its service; updates are ordered by `Depends` and expand-then-contract. |
| Minimum version per group | Written by `SetMinVersion` alone, not part of `Put`'s body. | An incident's minimum must not be undone by an operator re-applying their file. |
| A `replace` re-approval naming another group | Refused `host_group_differs`. | `Move` is the one way to move a host, with its order and its audit entry. |
| Config package names | `<service>-config-<group>`, `Provides`/`Conflicts: <service>-config`. | Each group's config has its own versions, releases and bad marks; the conflict keeps two from ever being installed together. |
| Config files | Ordinary files under `/etc/<service>/`, not conffiles; the service's defaults outside `/etc`. | `--force-confold` would keep a hand-edited conffile; one owner per path. |
| Compatibility between config and service | Debian `Depends` ranges, checked by the `apt` backend's existing simulation; expand then contract. | No new field or gate: the check exists and refuses before anything is installed. |
| Per-host values | The facts file; no templating, no per-host package, nothing run on a fact change. | The ruling; a change of facts restarting services would be a hidden deployment. |
| Facts file place | The daemon's `--facts-file`, by default `/var/lib/hostkeeper/facts.json`, 0644, on both platforms. | Beside the daemon's package directory on both; readable by a service that runs as its own user; a flag, so tests and other layouts point it elsewhere. |
| How hosts read the private repository | Through the coordinator over mutual TLS, into a local mirror apt reads as `file:`. | Uses the host's existing identity; apt gets no client key and its sandbox is unchanged; the coordinator can limit a host to its group's suite. |
| One suite and pool per group | `dists/<group>/`, `pool/<group>/`. | The coordinator authorizes by path prefix, with no index to parse; a host sees nothing of other groups. |
| Secrets | Referenced by name, read by the service from the site's secret store; never in a package or a fact. | The ruling; a package is copied to every host of its group and kept on disk. |
| The groups' file in an operator's repository | The operator's own; `hkctl host-groups put --request` takes proto JSON. | The CLI is generated from the API; a second, TOML reader in hostkeeper would be a second way to call `Put`. |
| Migration | One offline verb, a dry run first, `--apply` in one commit; `serve` refuses `--rollout-file`. | A stale rollout file is found at start, not half-applied; the dry run proves no host's placement changes before the window. |
| The certificate | Identity only; the issuer writes the fixed host-role `host`. | Shipped readers require one host-role URI; a role in it would go stale at the first move. |
| Operational documents | Unchanged until the build item that changes the code. | They describe what the code does (P10); this design names, per build item, what each changes. |
