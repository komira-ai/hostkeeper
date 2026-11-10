# Migration from kbf

The generic half of the host updater was built in [kbf](https://github.com/komira-ai/kbf) and moved
here. This file records what was imported, from where, under which names, what was left behind and
what changed on the way. The tests that hold the import to it are named below.

## Source

- kbf tag: `v0.0.64`
- kbf commit: `8f90d0604794f3fee057e71647c7e1c1e034fa2e`

Every file listed below was copied from that commit. The kbf copies stay in kbf until a hostkeeper
release replaces them there.

## Crates

| kbf | hostkeeper | what it holds |
|---|---|---|
| `update-hook` | `hostkeeper-hook` | the hook contract (steps, exit codes, the JSON last line, the params table, the next-step table) and, behind the feature `conformance`, the conformance harness |
| `update-rights` | split | split: loaders and grammar into `hostkeeper-config`, the launcher into `hostkeeper-steps`, the rest deleted by the redesign |
| `kbf-step-runner` | `hostkeeper-steps` | the step runner: the step record and its resume, the dead-man, the staged-artifact check, the updater's own update; since the import also the transient unit of one step |
| (the `Scratch` helpers of the three crates' tests) | `hostkeeper-testkit` | the scratch directory the tests share; a dev-dependency only |

| kbf name | hostkeeper name |
|---|---|
| Rust path `update_hook` | `hostkeeper_hook` |
| Rust path `kbf_step_runner` | `hostkeeper_steps` |
| binary `update-hook-example` (test-only) | `hostkeeper-hook-example` (test-only) |
| `docs/design/host-updates.md` | `docs/design/host-updates.md`, section numbers unchanged |

## What does not change

The names a host, a hook or a file on disk sees are the same bytes as in kbf at the tag:

- the exit codes `0`, `1`, `75` and `76`;
- the reserved `updater.*` params keys;
- `update-hook`, the default `hook_args`;
- the conformance harness's environment variables and the line a parked step writes;
- `/var/lib/updater/{state,hooks}`; kbf's `releases` tree is not reused, the package tree
  `/var/lib/hostkeeper/packages` replaces it (design `docs/design/hostkeeper-design.md`, 3.1 and
  10);
- the transient unit names `updater-<service>-<step>-<generation>`;
- the step record: `SCHEMA` stays 1, and a record of a newer schema is read, never rewritten.

## Step records written by kbf

`crates/hostkeeper-steps/tests/fixtures/kbf-v0.0.64/` holds step records in the bytes kbf's step
runner writes at the tag, one per state (`pending`, `running`, `done`, `finished`), and one record
of a newer schema. `hostkeeper-steps` reads each of the first four as the record it is, and reads the
newer one as `Newer` and never rewrites it.

## Left behind

- **The registration socket**: `kbf-step-runner/src/register.rs` and its tests
  (`tests/register.rs`, `tests/register_place.rs`, `tests/register_client.rs`), and with them the
  dev-dependency on `update-register`. The state of a service will be read from the service
  manager instead. `update-register` itself is not imported.
- **The declaration and what hung on it**: the per-service declaration, its rights (the user,
  capabilities, sandbox and path properties of the launch), the approval store, the `approve`
  command, the widening check and the account checks were not carried into the redesign (design
  `docs/design/hostkeeper-design.md`, 3.1, 5.3 and 12.2 B2 and B26).
- **The root tests of `update-rights`**: `tests/rights_root.rs` and `tests/root_rig/`, which need
  root and the real systemd, and the probe hook they ran. They were not kept: the redesign deletes
  what they tested.
- In `docs/design/host-updates.md`, the kbf hook (section 4.3) and the kbf-specific steps of the
  migration (section 8) are reduced to a pointer to kbf.

## What changed

Behaviour, wire strings, exit codes and on-disk formats did not change. The code changed only where
this repository's rules require it:

- **No `unsafe`.** The conformance harness stops a parked step with `rustix`'s
  `kill_process_group` instead of `libc::kill`, and the example hook kills itself with
  `kill_process` instead of `libc::raise`: the same signal to the same process (group).
- **Environment reads say why.** The four reads of the environment (the harness's kill-point
  variables, the coverage profile it passes through, and the two test programs) carry an
  `#[expect(clippy::disallowed_methods, reason = "...")]`.
- **The harness is behind a feature.** `hostkeeper_hook::conformance` needs the feature
  `conformance`; the example hook is built only with it.
- **Test programs are built only for tests.** `hostkeeper-hook-example` needs the feature
  `conformance`; `hostkeeper-steps-spawn-fixture` needs the feature `test-support` of
  `hostkeeper-steps`, which only that crate's own tests turn on.
- **Scratch directories.** The tests' scratch directories come from `hostkeeper-testkit`, with a
  name of their own per process and per directory, so two runs never share one.
- Crate names, paths in comments and READMEs, and the doc references to kbf's layering test.

## Since the import

The daemon config (`/etc/hostkeeper/hostkeeper.toml`) and the package manifest (`manifest.toml`)
replace the per-service declaration (design `docs/design/hostkeeper-design.md`, sections 4.1 and
4.7). Their loaders are the new crate `hostkeeper-config`, which also took `trust.rs` and the
service name and path grammars from the crate that held the declaration; a service name is now 1
to 255 bytes of `A-Za-z0-9._-`, not beginning with `-` or `.` (design 11.8), wider than the
lowercase grammar imported from kbf.

The transient unit of one step moved from `update-rights` (`launch.rs`, with its tests and its
spawn test program) to `hostkeeper-steps` (`systemd.rs`, with
`capture.rs`), as the `StepLauncher` of the host (B3). It runs a hook as root with
`RuntimeMaxSec=` as its one property (design 5.3), writes `RuntimeMaxSec=infinity` for
`prepare = 0`, keeps the last 1 MiB of each output stream and reads a last line of at most 64 KiB.
Its test program is `hostkeeper-steps-spawn-fixture`, built only with the feature `test-support`
of `hostkeeper-steps`. A step record of a running unbounded `prepare` carries `"unbounded": true`;
the field is written only when true and read as `false` when absent, so the records of the next
section read and write as before.

The crate that held the declaration was deleted by this change (B26). Its loaders, `trust.rs` and
the service name and path grammars went to `hostkeeper-config` (B2), and the transient unit went
to `hostkeeper-steps` (B3). What was left was deleted with the crate: the declaration parser, the
reserved-path rules, and the account, environment, capability and family grammars. Nothing reads a
declaration any more, because the hook runs as root with `RuntimeMaxSec=` only (design 5.3). The
earlier deletions of the declaration loader (`load_dir`), the widening check, the gate, the approval
store and its `approve` command, the account checks, the per-service signer and the declaration
digest are recorded in the paragraphs above.

The tree digest of `hostkeeper-steps` (`tree_digest`, which `Staged` and the step record hold)
is now the package format of `docs/tree-digest.md`: kind, name, mode, size and bytes, no owner
and no times, and a tree with a link has none. kbf's format, imported at first, hashed owners and
links. No release of hostkeeper wrote a record with the earlier digest; a record that holds one
is another update to the runner (`Busy` while it is unfinished, so no step runs from it; a new
update after its verdict), which `tests/digest_change.rs` holds.

## Tests that hold this file

- `constants_match_kbf_at_the_tag` in `hostkeeper-hook`: the exit codes, the `updater.*` keys, the
  default `hook_args` of `update-hook`, the conformance environment variables and the parked line,
  and the step names.
- `constants_match_kbf_at_the_tag` in `hostkeeper-steps`: the unit names
  `updater-<service>-<step>-<generation>`, the step record's file names and schema, and the
  self-update layout.
- `constants_match_kbf_at_the_tag` in xtask's packaging tests
  (`crates/xtask/tests/packaging/directories.rs`): the hooks root `/var/lib/updater/hooks` in the
  tmpfiles rules, which the deleted declaration crate pinned before.
- `reads_step_record_written_by_kbf_tag` and `kbf_tag_reader_contract_documented`
  (`hostkeeper-steps`): the records under "Step records written by kbf".

## Build order

`docs/design/hostkeeper-design.md` section 12.2 lists the build order. Read from the subjects of
`main` and the crates on disk when this was written, B26 being this change:

- Built: B1, B2, B3, B4, B5, B6, B7, B8, B9, B10, B11, B13, B16, B17, B19, B22, B24, B25, B26,
  B27, B28.
- Partly: B14 (the release workflows and verbs, but no release is published), B18 (the launchd
  crate and the daemon's Mac adapters, run under launchd only in the device run, with no update
  through it on a device), B23 (`install` and `uninstall`, but no assignment of services).
- Not yet: B12, B15, B20, B21.

# The update coordinator

The same tag (`v0.0.64`) and commit (`8f90d0604794f3fee057e71647c7e1c1e034fa2e`) as above. The
update coordinator moved here with the crates it stands on. It is imported with four cuts, listed
under "What changed": no service is built in, roles are open strings, the legacy service is a
flag, and the desired-state file format stays kbf's; and the narrowing described below: the
rollout rules that ordered roles and services or counted hosts up are cut, operator tokens are
named, and every change is audited to a file.

## Crates of the coordinator import

| kbf | hostkeeper | what it holds |
|---|---|---|
| `kbf-coordinator` | `hostkeeper-coordinator` | the coordinator: the heartbeat and control endpoints, the desired state of each service, the fleet, the decision and convergence rules, the operations and halts files, the audit log |
| `kbf-oidc` | `hostkeeper-oidc` | the verifier of OIDC tokens against an allow-list, and the issuer keys it fetches |
| `kbf-tls` (the reload code: `config.rs`, `source.rs`) | `hostkeeper-tls` | the TLS server configuration built from files and reloaded when they change, and its reload metrics |
| `kbf-updater` (`wire`, `wire_report`, `version`, `identity`, `secret`), `kbf-peer` (the certificate name helpers), `kbf-types` (`HostRole`, `NodeId`, `ReleasePlatform`) | `hostkeeper-wire` | the heartbeat and its reply, versions, host ids, role names, platforms, the certificate identity of an updater, the secret-file rule |

| kbf name | hostkeeper name |
|---|---|
| `HostRole` | `RoleName` (an open string, see below) |
| `NodeId` | `HostId` |
| `ReleasePlatform` | `Platform` |
| binary `kbf-coordinator` | `hostkeeper-coordinator serve` |
| `KBF_BUILD_COMMIT` | `HOSTKEEPER_BUILD_COMMIT` |
| log target `kbf_coordinator::audit` | `hostkeeper_coordinator::audit` |
| Rust paths `kbf_coordinator`, `kbf_oidc`, `kbf_tls` | `hostkeeper_coordinator`, `hostkeeper_oidc`, `hostkeeper_tls` |
| certificate role prefix `urn:kbf:role:` | `urn:hostkeeper:role:` (flag `--role-uri-prefix`) |
| certificate host-role prefix `urn:kbf:host-role:` | `urn:hostkeeper:host-role:` (flag `--host-role-uri-prefix`) |

## What does not change

- The routes `/v1/heartbeat`, `/v1/desired`, `/v1/convergence`, `/v1/status` and `/v1/operator`,
  and their JSON, including the `node_id` key of the status and of the operations file.
- The desired-state file, the operations file and the halts file, byte for byte: field order,
  pretty JSON with a final newline, the write through a temporary file, `fsync`, rename and
  directory `fsync`. A file kbf wrote is read here, and a file written here is read by the last
  kbf coordinator (see "The back-out rule").
- The heartbeat bytes an agent sends and the reply it gets: a kbf agent talks to this coordinator
  unchanged.
- The error codes of the endpoints.
- The platform strings `linux-x86_64`, `linux-arm64` and `darwin-arm64`.

## What changed

- **The rollout file is data.** `--rollout-file` names the TOML file that defines every service
  (it was `--services-file`: the old name is still accepted, hidden, and giving both is refused).
  No definition is built in, `Services::default()` is empty, and an empty file defines none; the
  first service no longer has to be `kbf`. A `[[service]]` table has `name`, `version_scheme`
  (new; default `semver`, the only scheme; any other value is refused naming the key), `roles` (a
  plain list of role names, `roles = ["storage", "daemon"]`, which replaces the
  `[service.roles.<name>]` tables), `priority`, `selector`, `install_when_absent` (read only as
  `false`; see the assignment below), `bake_beats`, `canary`, `stages` (the service's update plan:
  1 to 16 percents rising to 100, each a number or a table with `percent` and `advance`, `"auto"`
  or `"manual"`; by default `[1, 10, 50, 100]`; see
  [rollout-update-plans.md](docs/design/rollout-update-plans.md)), `rollback`,
  `rollback_after_failed`, `min_version` and `defer_backoff`. A table refuses a key it does not
  know, naming it.
- **Removed from the rollout file.** Each of these is refused by name, none is ignored: the
  service's `after` and `require_min_up`, and the `max_updating`, `min_up`, `never_last` and
  `after` of a role. kbf's definition becomes `name = "kbf"` and
  `roles = ["frontend", "storage", "daemon", "mac-daemon"]`.
- **Behaviour changes of the cut rules.**
  - This migration ordered one update at a time across every service and role, and a
    `max_updating` of 0 did not hold a role. The update plan release replaces that: a service
    rolls out by stages, the hosts of a stage update in parallel up to the desired state's
    `max_updating` of their role, and a `max_updating` of 0 holds the role again. One service's
    rollout is live at a time by default (`--live-rollouts`).
  - `min_up` delays an order again, per role, when it would leave fewer hosts of the role up; the
    convergence does not read it.
  - `never_last` no longer keeps the last host of a role up.
  - `after`, of a role or of a service, and the order of roles no longer delay an order.
  - A service converges when every live member runs the desired version and none is updating,
    failed, baking or waiting for a promotion.
  - A `POST /v1/desired` body needs no `min_up` for any service (kbf required `storage` and
    `frontend` for its own).
- **What does not change in the state files.** `max_updating` and `min_up` of a desired state are
  still read, kept, written in kbf's order (`min_up` always naming `storage` and `frontend`) and
  printed in the status; the update plan of a service reads them per role. `desired.json` and the
  operations file are the same bytes. The halts file gains a `rollouts` key (each plan's position)
  only once a plan goes past its first stage or is paused, and the release before skips it. The
  status and convergence JSON keep every field.
- **The decision names a gate or a skip.** `decide::Hold` is split into `Gate` (`halted`, `held`,
  `bad`, `below_min_version`, `updating`, `busy_elsewhere`, `no_slot`, `quiet`, `baking`, and,
  with the update plans, `paused`, `advance_wait`, `stage_failed`, `stage_wait`, `below_min_up`
  and `rollout_elsewhere`; `canary_wait` is no longer produced, the canary being the plan's first
  stage) and `Skip` (`no_desired`, `not_a_member`, `up_to_date`, `failed_here`, `deferred`,
  `unsupported`, `no_digest`, `no_package_for_platform`, and, for a version set with targets,
  `no_target`, `no_build_for_host` and `installer_dropped`), and a decision is `Update`, `Install`, `Uninstall`, `Gate` or
  `Skip`. `waiting_for_role`, `waiting_for_service` and `min_up` (`below_min_up` says it) are no
  longer produced, nor are `no_definition` (a service the coordinator does not manage is
  `not_a_member`) and `absent` (see the assignment below).
- **A host runs the services the coordinator assigns it.** The rollout file's `roles` (or its
  selector) are the base; `Services.Assign`, `Unassign` and `SetMinVersion` add entries per role
  and per host on top, kept in the operations file under `assignments`, a key written only once
  an entry stands. A service assigned to a host that does not run it (it does not list it, or
  lists it `absent`) is ordered `install` with its desired version and the package digest of its platform, under
  every rule of an update, but only as a typed order to a daemon that announces `install-v1` and
  `heartbeat-v2`; the JSON reply carries no install, so a kbf agent that reports a service
  `absent` is told nothing (`unsupported`), where `install_when_absent = true` used to order it
  an update: that value is now refused, naming what replaced it. A service not assigned to a
  host is never ordered there, installed or not; only `Unassign` with `uninstall` orders a host
  to uninstall it.
- **Named operator tokens.** `--tokens-file` is a TOML file of `[[token]]` tables, each with a
  `name` (1 to 32 bytes of `a-z`, `0-9` and `-`, unique; `coordinator` and `ci` are reserved) and
  the `sha256` of the token as lowercase hex, so the file holds no secret. It is read under the
  secret-file rule and read again on the `--tls-reload-secs` tick, so a token is revoked or
  replaced without a restart. `--operator-token-file` is kept and is one token named `operator`;
  giving both flags is refused, and so is giving neither. `Coordinator::operator` returns the
  token's name, and every operator change records it as `by`.
- **The audit file.** Every change is also appended as one JSON line,
  `{"seq":..,"at_unix":..,"by":..,"action":..,"detail":{..}}`, to `<ops-file>.audit`: opened
  append-only, created with mode 0600 and fsynced. A torn last line is skipped when the file is
  read, and numbers continue from the larger of the ring's last and the file's. The ring of 64
  stays in the operations file for `/v1/status`. A line that cannot be appended is an error in the
  log, and the change stands.
- **One private listener.** `--private-listen` (it was `--control-listen`: the old name is still
  accepted, hidden, and giving both is refused) is the listener of CI's OIDC calls and of the
  operator's. A wildcard address is refused at start, naming the flag. `--heartbeat-listen` is
  unchanged.
- **Roles are open strings.** A role is any `RoleName`: 1 to 32 bytes of `a-z`, `0-9` and `-`,
  starting with a letter, with no trailing `-` and no `--`. There is no enum, no constant and no
  list of known roles; a service's definition says which roles it has. The layering test
  `layering_refuses_a_closed_role_enum` refuses the closed set coming back.
- **The legacy service is a parameter, `--legacy-service <name>`.** kbf read the top level of a
  heartbeat as the heartbeat of the service `kbf`. Here that service is the one named by the flag,
  and it must be defined in the rollout file. It decides, and with no flag none of these applies:
  - the top-level version, health, last update and updating fields of a heartbeat belong to it;
    with no legacy service they belong to no service and only the `services` list counts;
  - a heartbeat with no `services` list (absent, or dropped as bad) is refused with `400
    bad_request` when no legacy service is set;
  - a reserved name in the strict heartbeat reader is gone: with a legacy service, a `services`
    list naming it is dropped as a bad list is (it is the top level of a heartbeat, not a
    service); with none, no name is reserved;
  - an order for it is a reply with no service name, and an order for any other service names it;
  - a desired-state request with no `service`, a desired-state file in the single-state form, and
    `GET /v1/convergence` with no query are its; with no legacy service the request is refused
    (`service is required`), the file is refused to open, and the query is `400 bad_request`;
  - the status lists its hosts and its desired state (empty and none without one);
  - the top-level halt report is its report;
  - an OIDC allow-list entry with no `services` key stands for it, and such an entry is refused at
    start when no legacy service is set.
- **The two prefix flags.** The certificate namespaces are `--role-uri-prefix` (default
  `urn:hostkeeper:role:`) and `--host-role-uri-prefix` (default `urn:hostkeeper:host-role:`). The
  pair is refused when empty, not visible ASCII, equal, or one a prefix of the other. The role in
  a certificate may be any role name.
- **The oidc default services are gone.** An allow-list entry's `services` key stays optional in
  the file, and an old file without it loads. The verifier returns `None` for an entry that names
  none, and the coordinator reads `None` as its legacy service.
- **The agent field.** A heartbeat may carry `agent`, an object with the agent's `name` and
  `version`. It is optional and additive: a heartbeat without it counts as agent `kbf-updater`,
  and a bad value is dropped like the `step` fact. The status shows it for each host as
  `"agent": {"name": "...", "version": "..."}` (`version` omitted when unknown), and
  `{"name":"kbf-updater"}` for a heartbeat without the field.
- **In-process test transports.** The tests that kbf ran over a real listener run here over
  in-memory connections carrying real TLS, and the OIDC issuer keys are fetched through an
  injectable dial; no test binds a port or resolves a name.
- **No `unsafe`.** The effective user id is read with `rustix` instead of `libc::geteuid`.
- **`StartError`.** The coordinator's start-up errors are a `thiserror` enum, not a boxed error.
  The text of each error is kbf's.
- **Behaviour fixed on the way in.** Each of these differs from kbf at the tag, and each has a
  test:
  - an OIDC allow-list entry may say `services = []`: it names no service and its tokens may set
    none (kbf refused the empty list), so a coordinator with no services can start;
  - the OIDC verifier reads the clock again after it fetched the issuer's keys, so a token that
    expired during the fetch is refused;
  - CI's set of a desired state holds the fleet's lock that a heartbeat plans and writes an
    automatic rollback under, so a rollback planned from an older state cannot overwrite the
    version CI set meanwhile;
  - a desired-state write whose directory sync fails after the rename is reported as an error
    and the coordinator serves the state now on disk, which a restart reads too (kbf kept the
    old state in memory);
  - a connection whose peer stops reading its responses is closed after the same patience as a
    stalled request, so it gives its slot back;
  - typed errors replace text in the issuer-key fetch (`FetchError`) and in `Selector::new`
    (`SelectorError`); their texts are kbf's.

## Left behind

- `kbf-tls`: `incoming.rs`, `sni.rs`, the tonic integration and their tests.
- The container unit test and the packaging of kbf's coordinator image.
- gRPC: the coordinator speaks the HTTP and JSON endpoints above only.
- The embedded service definitions (kbf's and tierstore's): they are data a deployment passes in
  `--rollout-file`, and the copies the tests use are recorded in
  `crates/hostkeeper-coordinator/tests/fixtures/recorded/`.

## The back-out rule

Until kbf retires its agent, this coordinator never writes a key or a schema that the last kbf
coordinator refuses to read, so a deployment can go back to kbf's coordinator on the files this one
wrote. That is why the desired-state serializer and readers are kbf's, and why the role names that
format carries (`frontend`, `storage`, `daemon`, `mac-daemon` for `max_updating`, and `storage`,
`frontend` for `min_up`) are constants of the file format, not roles of any service. The tests hold
it from both sides: the files in `tests/fixtures/recorded/` are kbf's bytes and are read here,
and `strict-readers.json` records, from the tag's source, every key the strict kbf readers allow
and require, against which what this coordinator writes is checked. The audit file
`<ops-file>.audit` is a new file beside the operations file, which kbf's coordinator does not
read; `by` in the ring is the token's name, and kbf reads any text there.

## Starting this coordinator on kbf's files

A kbf deployment starts `hostkeeper-coordinator serve` on its old files with its old certificates
by adding four flags to the flags it already passes:

```
--rollout-file kbf.services.toml --legacy-service kbf \
  --role-uri-prefix urn:kbf:role: --host-role-uri-prefix urn:kbf:host-role:
```

where `kbf.services.toml` holds `name = "kbf"` and its four roles (and the definition of any other
service the deployment served). The old `--services-file` spelling still works.

## Tests that hold the coordinator import

- `decisions_equal_kbf_except_where_a_cut_rule_decided`: the decisions and convergence answers of
  this coordinator on heartbeat sequences recorded from kbf are kbf's bytes, except the seven
  sequences of its `CHANGED` list, where a cut rule decided and kbf's answer is kept beside this
  coordinator's; any other sequence that changes fails it.
- `each_removed_key_is_refused_by_name`: every key the rollout file lost is refused, naming it.
- `version_scheme_defaults_to_semver_and_any_other_value_is_refused`: the new key's default and
  its refusal.
- `a_revoked_token_is_refused_without_restart`: a token taken out of the tokens file is refused
  after the next reload tick.
- `every_action_is_audited_under_the_token_name`: each operator change records the token's name.
- `the_audit_file_is_append_only_across_restart`: a restarted coordinator appends to the audit
  file and keeps numbering.
- `the_legacy_single_token_flag_is_one_token_named_operator` and
  `both_token_flags_together_are_refused`: the old token flag and the new one.
- `a_wildcard_private_listen_is_refused_at_start` and
  `the_old_flag_names_are_hidden_aliases_and_both_together_are_refused`: the listener and the
  renamed flags.
- `state_files_written_by_kbf_are_read_and_the_reverse_holds_by_schema_rules`: the files kbf wrote
  are read here, and what is written here passes kbf's strict readers.
- `legacy_single_service_heartbeat_belongs_to_the_named_service`: with `--legacy-service`, the
  top level of a heartbeat is that service's.
- `without_legacy_service_such_a_heartbeat_is_refused`: with no legacy service a heartbeat with no
  `services` list is `400 bad_request`.
- `role_prefix_flags_accept_old_certificates_and_the_new_default_refuses_them`: a certificate
  under `urn:kbf:` is an identity under the old prefixes and refused under the defaults.
- `no_service_name_is_built_in`: a coordinator with an empty rollout file knows no service.
- `heartbeat_without_agent_counts_as_kbf_updater_and_with_agent_is_shown_in_status`: the agent
  field is optional and shown.
- `layering_refuses_a_kbf_dependency` and `layering_refuses_a_closed_role_enum` (`xtask`): no kbf
  package in the graph, and no closed set of roles in the source.
- `hygiene_lint_passes_the_tree` (`xtask`): the fixtures recorded from kbf name no internal host,
  date or address.

# The MDM kit's core

The pure core of the MDM kit (docs/design/hostkeeper-mdm.md, section 8, task M2) was imported
from kbf's `main` at commit `5d7a2c405f9e23949661ee1e2b34fc029a623cbd`, where it is the crate
`crates/mdm-core` (moved there by komira-ai/kbf#2238). kbf's copy stays until the design's task
M6a deletes it.

| kbf | hostkeeper | what it holds |
|---|---|---|
| `mdm-core` | `hostkeeper-mdm-core` | the kit's settings, the enrollment and ADE profiles, the systemd units and Caddyfile of the MDM services, the pinned upstream tools and the arguments of the SCEP CA and the DEP certificate |

| kbf name | hostkeeper name |
|---|---|
| Rust path `mdm_core` | `hostkeeper_mdm_core` |
| default unit prefix `js-mdm` and the defaults derived from it: the slice, `org_magic`, the SCEP CA's organizational unit, the DEP certificate's name, the signature namespace (`js-mdm-erase`) | `hostkeeper-mdm` (`hostkeeper-mdm.slice`, `hostkeeper-mdm-erase`) |
| example user and directories `js-mdm`, `/var/lib/js-mdm`, `/etc/js-mdm`, `/opt/js-mdm/bin` | `hostkeeper-mdm`, `/var/lib/hostkeeper-mdm`, `/etc/hostkeeper-mdm`, `/opt/hostkeeper-mdm/bin` |
| example profile identifier `com.example.mdm` | `com.example.hostkeeper.mdm` |

## What changed

The fixes of komira-ai/kbf#2248 that the design gives this task, each with a test that fails on
the crate as it was copied (the first commit of the import):

- The three loopback ports (`MDM_NANOHUB_PORT`, `MDM_SCEP_PORT`, `MDM_DEP_PORT`) are checked
  together: two equal ports are refused, naming the later key and the key it repeats
  (`the_three_loopback_ports_are_checked_together`).
- `MdmConfig` and `Cidr`, and the parts of `MdmConfig` (`AdeSettings`, `ScepCa`, `DepCert`), have
  private fields and read-only accessors. `MdmConfig::parse` is the only way to make a config;
  `Cidr::new` and `Cidr`'s `FromStr` are the only ways to make a network, with the checks of
  `MDM_LAN_CIDR` and a typed `CidrError` (`a_network_is_built_only_through_its_checks`). A `Cidr`
  is now 8 to 30 bits, so `contains` lost its guard for a /0 and a /32
  (`a_network_of_eight_or_thirty_bits_holds_exactly_its_addresses`).
- DNS names are within DNS's limits, 63 characters a label and 253 in all: `MDM_DNS_NAME`,
  `MDM_PROFILE_ID` and the host of every `https://` URL
  (`dns_names_are_within_the_label_and_name_length_limits`,
  `a_url_host_is_within_the_dns_length_limits`).
- An `https://` URL's authority ends at the first `/` or `?`; a fragment, whitespace and control
  characters are refused anywhere in it; its host is a host name and its port is digits only
  (`an_authority_ends_at_its_path_or_query_and_holds_only_a_host_name_and_port`).
- The ADE profile sets `auto_advance_setup` (design section 6.2); its parser still refuses a
  profile that is not supervised, not mandatory or removable
  (`the_ade_profile_advances_setup_by_itself_and_the_parser_still_refuses_a_weaker_one`).

The other items of komira-ai/kbf#2248 belong to the ports of `host-step` and `mdm-host` (design
section 12, tasks M3 and M4).

## Left behind

- The ignored test `installed_host`, a manual hook that compared a render with the files of a host
  read by hand. `render::manifest` stays.

## Tests that hold the MDM core import

`crates/hostkeeper-mdm-core/tests/kbf_port.rs`: rendered with kbf's names
(`tests/kbf_f89ed5c/kbf_names.conf`), the units, the Caddyfile, the enrollment profile, the ADE
profile, the pins and the SCEP and DEP arguments equal kbf-deploy's own files, except the three
differences that file names (comment wording, the slice's `Description=`, `auto_advance_setup`).

# The MDM kit's binary

The binary of the MDM kit (docs/design/hostkeeper-mdm.md, section 8, task M4) was imported from
kbf's `main` at the commit the core came from, `5d7a2c405f9e23949661ee1e2b34fc029a623cbd`, where it
is the crate `crates/mdm-host`. kbf's copy stays until the design's task M6a deletes it. The first
commit of the import is the crate as moved, unchanged in behaviour; each fix below is a later commit
with a test that fails on that first one.

| kbf | hostkeeper | what it holds |
|---|---|---|
| `mdm-host` | `hostkeeper-mdm` | the binary and its verbs `fetch`, `install` (and `--uninstall`), `tls` and `verify` |

| kbf name | hostkeeper name |
|---|---|
| Rust path `mdm_host`, binary `mdm-host` | `hostkeeper_mdm`, `hostkeeper-mdm` |
| the flag `--config` of `install`, `tls` and `verify` | `--settings` (design section 3) |
| example user, unit prefix and directories `js-mdm`, `/var/lib/js-mdm`, `/etc/js-mdm`, `/opt/js-mdm/bin` | as the core: `hostkeeper-mdm`, `/var/lib/hostkeeper-mdm`, `/etc/hostkeeper-mdm`, `/opt/hostkeeper-mdm/bin` |

## What changed

What the move itself changed, without a change of behaviour:

- **The roots of the downloads.** kbf's `fetch` trusted the bundled web roots (`webpki-roots`),
  whose licence (CDLA-Permissive-2.0) `deny.toml` does not allow. `fetch` trusts the host's roots
  (`rustls-native-certs`), as the coordinator's HTTPS clients do. Every download is checked against
  its sha256 pin after it arrives, so the roots guard the transfer, not what is installed.
- **The host, its trusted owners and the clock are injected.** `run` takes a `System`: a `Machine`
  that makes the verb's `Host`, the downloads, the random source and the unix second, which `main`
  reads once. The `Machine` grants the owners `hostkeeper-mdm-step`'s `RealHost::trust_below` asks
  for (design task M3): the service user's below `MDM_STATE` for `install`, `tls` and `verify`, and
  `--bin-dir`'s owner below it for `install`.
- The read-only accessors of the core's `MdmConfig` replace kbf's public fields.

The fixes of komira-ai/kbf#2248 that the design gives this task, each with a test that fails on the
crate as it was moved:

- **Restarts are planned from the installed state.** kbf restarted a service only when this run
  changed one of its inputs, so a run that stopped after replacing a binary but before the restart
  left the old process running, and the rerun saw nothing to do. `install` now writes an empty 0600
  root file `/run/<MDM_UNIT_PREFIX>/restart/<unit>` before it changes a service's inputs, restarts
  every service that has one (whether or not anything changed in this run) and removes it right
  after that service's restart; `/run` is a tmpfs, so a reboot clears it with the services it
  restarts. `--uninstall` removes them
  (`a_restart_that_a_failed_run_did_not_reach_is_done_by_the_rerun`,
  `only_the_unit_with_a_marker_is_restarted_when_nothing_else_differs`,
  `a_proxy_marker_waits_for_the_certificate_and_is_removed_by_its_restart`,
  `uninstall_removes_the_pending_restart_markers`).
- **Malformed ACME settings are refused, not skipped.** `verify` read them with
  `Acme::parse(..).ok()`, so a malformed one skipped the certificate job's units. Now, when the
  settings set any `MDM_ACME_*` key, all five must be set and valid or `verify` is refused (exit 2);
  with none set the units are skipped as before (`verify_refuses_a_malformed_acme_setting`,
  `verify_refuses_acme_settings_with_a_key_missing`,
  `verify_skips_the_certificate_units_when_no_acme_key_is_set`).
- **`lego run --help` must exit 0.** kbf looked only at its text
  (`a_lego_whose_help_exits_non_zero_is_refused_even_when_it_lists_every_flag`).
- **Archive limits are exact and safe at `u64::MAX`.** A zip member listed larger than the limit is
  refused even when its output would fit (kbf truncated it to the limit and accepted it); a zip
  member must inflate to exactly its listed size; the tar path reads `limit + 1` with saturating
  arithmetic; every offset read from a zip is added with saturating arithmetic
  (`a_zip_member_listed_larger_than_the_limit_is_refused_even_if_its_output_fits`,
  `a_zip_member_must_come_out_exactly_the_listed_size`,
  `a_member_of_exactly_the_limit_is_read_and_one_byte_more_is_refused`,
  `a_limit_of_u64_max_reads_a_member_on_both_packings`,
  `hostile_zip_offsets_near_the_integer_limits_are_refused_not_panicked_on`).
- **Environment files are read by systemd's rules.** kbf took the first `KEY=` line as it stood.
  The secret files are now read as systemd's `EnvironmentFile=` reads them: the last assignment of
  a key wins (so `KEY=abc` then `KEY=` is empty), quotes and backslashes are taken as systemd takes
  them, and comments and white space likewise. A construct whose systemd reading is not certain (a
  quote left open, text after a closing quote, a backslash in single quotes, a continued line that
  starts with white space) and bytes that are not UTF-8 are refused, naming the file and line, by
  `tls` and by `install`, which never makes such a file again
  (`an_empty_assignment_after_a_value_leaves_no_value`, `quotes_around_a_value_are_dropped`,
  `constructs_with_an_uncertain_reading_are_refused`,
  `a_secret_file_systemd_would_not_read_with_certainty_is_refused_by_its_cause`, and the rest of
  `tests/secret.rs`).
- **A typed error for `Tool::named`.** The core's `Tool::named` returns `UnknownTool`, which
  `fetch --only` reports (`only_with_an_unknown_tool_is_refused_with_the_unknown_tool_message`).
- **Downloads.** A body of exactly the limit is accepted (ureq refused the read after the last
  permitted byte, so kbf refused it), and at most five redirects are followed, a longer chain or a
  cycle being an error (`a_body_of_exactly_the_limit_is_accepted_and_one_byte_more_is_refused`,
  `a_chain_of_exactly_the_redirect_limit_is_followed_and_one_more_is_refused`). Tests of what was
  already right: an HTTPS-to-HTTP redirect is refused, a redirect cycle is an error, and a body cut
  short, with a length or chunked, is an error.
- **TLS.** A directory or link at the certificate path, and a certificate `openssl` cannot read,
  are refused before any change (kbf took the first for a certificate and started the proxy). A
  key that is missing, not a regular file or not the certificate's makes a new issuance, as does a
  certificate that ends within `verify`'s 21 days; the reload path unit is started before the
  certificate unit, so the new certificate restarts the proxy
  (`a_directory_or_a_link_at_the_certificate_path_is_refused_before_any_change`,
  `a_certificate_file_openssl_cannot_read_is_refused_before_any_change`,
  `a_missing_or_mismatched_key_plans_a_new_issuance_and_says_why`,
  `a_certificate_ending_soon_is_renewed_once_after_the_reload_path_is_watching`,
  `the_reload_path_watches_the_certificate_and_its_service_try_restarts_the_proxy`).
- **Install.** A SCEP depot is a CA only when it holds both `ca.pem` and `ca.key` (the files
  `scepserver ca -init` writes) as regular files that `openssl` reads as one key pair; a depot
  holding one of them, or a damaged pair, is refused before any change, never made again over. A
  host whose `uname -m` is not `x86_64` is refused first, since the pins are linux-amd64 builds.
  `--uninstall` stops the certificate and reload jobs before it moves their files, and a second
  uninstall in the same second takes `<seconds>-1` and keeps the first snapshot
  (`a_depot_with_only_the_key_is_refused`, `a_certificate_and_a_key_that_are_not_a_pair_are_refused`,
  `an_unsupported_architecture_is_refused_before_anything_else`,
  `uninstall_stops_the_certificate_and_reload_jobs_before_moving_their_files`,
  `uninstalls_in_the_same_second_keep_every_snapshot`).
- **Whole-output redaction.** The output of `install` and of `tls`, dry run and apply, is compared
  whole with the expected text, and no secret value is in it
  (`the_whole_output_of_an_apply_is_the_plan_and_shows_no_secret`,
  `the_whole_output_of_a_dry_run_is_the_plan_and_shows_no_secret`,
  `the_whole_output_of_a_dry_run_and_an_apply_holds_no_dns_credential`).

## Tests that hold the MDM binary import

The tests kbf's crate had are kept, renamed to hostkeeper's names, in
`crates/hostkeeper-mdm/tests/`; each fix above adds its own.

# The MDM kit's push-cert and dep verbs

The verbs `push-cert` and `dep` (`get-cert`, `upload-token`, `define-profile`, `assign`) of the
MDM kit's binary (docs/design/hostkeeper-mdm.md, section 8, task M5) were ported from the `mdm`
module of kbf's `kbf-deploy` (`crates/kbf-deploy/src/mdm/push_cert.rs`, `dep.rs` and `api.rs`) at
kbf's commit `5d7a2c405f9e23949661ee1e2b34fc029a623cbd`. kbf's copy stays until the design's task
M6a deletes it. Its `erase-request` verb is not ported: the design leaves erase out of v1.

| kbf | hostkeeper |
|---|---|
| `kbf-deploy mdm push-cert --config FILE` | `hostkeeper-mdm push-cert --settings FILE` |
| `kbf-deploy mdm dep <verb> --config FILE` | `hostkeeper-mdm dep <verb> --settings FILE` |
| the API calls on loopback over a raw socket | `crates/hostkeeper-mdm/src/api.rs`: the same calls through the injected `Loopback`, whose real client is ureq to `127.0.0.1` only |

## What changed

- **The enrollment profile is mode 0640, root's and the service group's.** kbf wrote it 0600 and
  the service user's, so the service could rewrite the file it serves. The proxy reads it through
  the group (`the_enrollment_profile_is_served_mode_0640`); `verify` checks the mode, owner and
  group.
- **The time of the expiry check is the injected clock** (`the_certificate_expires_exactly_at_not_after`).
- **The git work tree check resolves the path.** kbf walked the path as given; a relative path
  or a link into a work tree got past it. The path must be absolute and every link on the way is
  resolved first (`a_relative_path_or_a_link_into_a_work_tree_is_refused`).
- **`dep define-profile` defines again when the uuid record is lost.** kbf called nothing when
  the profile's sha256 was recorded, even with no uuid recorded, which left `assign` without a
  profile (`a_matching_digest_without_a_uuid_record_defines_again`).
- **`dep upload-token` refuses an empty token or one over 64 KiB, and fails on an account answer
  that is not a JSON object**, where kbf sent the file whole and printed `?`
  (`upload_token_refuses_an_empty_or_oversized_file`,
  `upload_token_fails_on_an_account_answer_that_is_not_json`).

## New in hostkeeper

`hostkeeper-mdm migrate --from <old settings file>` (design section 8.3) has no kbf original; its
settings half is `crates/hostkeeper-mdm-core/src/migrate.rs` and its host half
`crates/hostkeeper-mdm/src/migrate.rs`. The pkg directory (`MDM_PKG_DIR`) and the Caddyfile's
`/pkg/<sha256>.pkg` route are new too (design section 7.1); `tests/kbf_port.rs` names the route
as the Caddyfile's one difference from kbf's outside its comments.
