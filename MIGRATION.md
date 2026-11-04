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
| `update-rights` | `hostkeeper-rights` | the declaration, its loader, the widening check, approvals, the gate and the transient unit of one step |
| `kbf-step-runner` | `hostkeeper-steps` | the step runner: the step record and its resume, the dead-man, the staged-artifact check, the updater's own update |
| (the `Scratch` helpers of the three crates' tests) | `hostkeeper-testkit` | the scratch directory the tests share; a dev-dependency only |

| kbf name | hostkeeper name |
|---|---|
| Rust path `update_hook` | `hostkeeper_hook` |
| Rust path `update_rights` | `hostkeeper_rights` |
| Rust path `kbf_step_runner` | `hostkeeper_steps` |
| binary `update-hook-example` (test-only) | `hostkeeper-hook-example` (test-only) |
| binary `update-rights-probe` (test-only) | `hostkeeper-rights-probe` (test-only) |
| `docs/design/host-updates.md` | `docs/design/host-updates.md`, section numbers unchanged |

## What does not change

The names a host, a hook or a file on disk sees are the same bytes as in kbf at the tag:

- the exit codes `0`, `1`, `75` and `76`;
- the reserved `updater.*` params keys;
- `update-hook`, the default `hook_args`;
- the conformance harness's environment variables and the line a parked step writes;
- `/etc/updater.d`, `/etc/updater.d/approved` and `/var/lib/updater/{state,hooks,releases}`;
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
- **The root tests of `update-rights`**: `tests/rights_root.rs` and `tests/root_rig/`, which need
  root and the real systemd. They come with the privileged workflow, which a pull request does not
  run; the probe hook they run (`hostkeeper-rights-probe`) is imported now.
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
- **Test programs are built only for tests.** `hostkeeper-rights-probe` needs the feature
  `test-support`, which only the crate's own tests turn on, as `hostkeeper-hook-example` needs
  `conformance`.
- **Test fixtures name an example signer.** Declarations in the tests that named `komira-ai/kbf` as
  their signer name `example-org/example`.
- **Scratch directories.** The tests' scratch directories come from `hostkeeper-testkit`, with a
  name of their own per process and per directory, so two runs never share one.
- Crate names, paths in comments and READMEs, and the doc references to kbf's layering test.

## Tests that hold this file

- `constants_match_kbf_at_the_tag` (in each of `hostkeeper-hook`, `hostkeeper-rights` and
  `hostkeeper-steps`): the names under "What does not change" are the literals of the tag.
- `reads_step_record_written_by_kbf_tag` and `kbf_tag_reader_contract_documented`
  (`hostkeeper-steps`): the records under "Step records written by kbf".

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
  `[service.roles.<name>]` tables), `priority`, `selector`, `install_when_absent`, `bake_beats`,
  `canary`, `rollback`, `rollback_after_failed`, `min_version` and `defer_backoff`. A table refuses
  a key it does not know, naming it.
- **Removed from the rollout file.** Each of these is refused by name, none is ignored: the
  service's `after` and `require_min_up`, and the `max_updating`, `min_up`, `never_last` and
  `after` of a role. kbf's definition becomes `name = "kbf"` and
  `roles = ["frontend", "storage", "daemon", "mac-daemon"]`.
- **Behaviour changes of the cut rules.**
  - A `max_updating` of 0 no longer holds a role. One update is live at a time across every
    service and role; a later change adds an operator hold.
  - `min_up` no longer delays an order, and the convergence does not read it.
  - `never_last` no longer keeps the last host of a role up.
  - `after`, of a role or of a service, and the order of roles no longer delay an order.
  - A service converges when every live member runs the desired version and none is updating,
    failed, baking or waiting for a promotion.
  - A `POST /v1/desired` body needs no `min_up` for any service (kbf required `storage` and
    `frontend` for its own).
- **What does not change in the state files.** `max_updating` and `min_up` of a desired state are
  still read, kept, written in kbf's order (`min_up` always naming `storage` and `frontend`) and
  printed in the status; no decision reads them. `desired.json`, the operations file and the
  halts file are the same bytes, and the status and convergence JSON keep every field.
- **The decision names a gate or a skip.** `decide::Hold` is split into `Gate` (`halted`, `bad`,
  `updating`, `busy_elsewhere`, `no_slot`, `quiet`, `canary_wait`, `baking`) and `Skip`
  (`no_definition`, `no_desired`, `not_a_member`, `absent`, `up_to_date`, `failed_here`,
  `deferred`), and a decision is `Update`, `Gate` or `Skip`. The status's `hold` keeps its
  strings; `waiting_for_role`, `waiting_for_service` and `min_up` are no longer produced.
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
