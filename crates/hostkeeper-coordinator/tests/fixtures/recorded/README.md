# Recorded fixtures

Data recorded from kbf's update coordinator at its tag `v0.0.64` (commit
`8f90d0604794f3fee057e71647c7e1c1e034fa2e`). The tests in `crates/hostkeeper-coordinator/tests`
read them as they are; none is generated at test time. A file is changed only to record kbf
again, never to make a test pass. Three kinds of change are the exception, and say so where they
are: the services files are written as rollout files of this coordinator (the data they record
is kbf's service and tierstore's); the sequences `24` to `30` are not recordings: each is a
case derived from kbf's rules at the tag, where a rule this coordinator cut decided, with its own
answer and kbf's beside it, or, in `25` and `27`, where a rule this coordinator holds as kbf did
decided, with the answer both give; one step of `17`, whose list of nine feature names kbf's
bound of 8 dropped and this coordinator's bound of 16 reads, keeps kbf's answer beside this
coordinator's; and steps of `10` and `23`, where this coordinator's update plan decided, keep
kbf's answers beside this coordinator's.

## Services and flags

- `kbf.services.toml`: kbf's service as an entry of a rollout file (`--rollout-file`): its name
  and its four roles. What kbf's built-in definition also said (a frontend never the last one up
  and waiting for the other roles, the `min_up` keys a desired state must name) is not a rollout
  setting of this coordinator and is not in the file.
- `tierstore.services.toml`: tierstore's service as an entry of a rollout file: its role,
  selector, bake, canary and rollback. The per-role `max_updating` and `min_up` of kbf's file are
  not in it.
- `services.toml`: the two in one file, kbf first. This is the `--rollout-file` of the replays
  unless a sequence names another.
- `flags.json`: the flags kbf's farm ran with that this coordinator takes as parameters:
  `--legacy-service kbf` and the two certificate namespaces `urn:kbf:role:` and
  `urn:kbf:host-role:`.

## State files

Written by kbf's `DesiredStore::write` (`desired.rs`) and `ops_file::write` (`ops_file.rs`):
pretty JSON, field order of the structs, a trailing newline. They were derived from those
serialisers for the states named here, not copied from a running farm.

- `desired-single.json`: the file after CI set `{"version":"2.0","max_updating":{"daemon":1},
  "min_up":{"storage":1,"frontend":1}}`, with kbf the only service (the single-state form
  earlier builds read). Source: `tests/farm.rs`'s `STATE`.
- `desired-map.json`: the map form (`{"services": {...}}`) after tierstore was also set to 7.1
  with `min_up` storage 4: the keys of the format that the body did not name are filled in and
  written in the format's order.
- `ops.json` and `ops.json.halts`: the ops file and the halts file beside it after an operator
  excluded the host `s2` from kbf (`bad disk`), labelled `s1` `rack=r1`, and `s1` reported a
  failed update in the halt phase (check `disk_tainted`, target 2.0). The halts file names the
  SHA-256 of the ops file it goes with. Source: `http_tests/halt.rs` for the report, `ops.rs`
  for the audit entries.
- `ops-schema2.json`: an ops file of a schema newer than kbf's (`{"schema":2,"services":{}}`),
  as `tests/binary.rs::an_ops_file_this_build_cannot_read_stops_the_start` wrote it.
- `strict-readers.json`: for each struct kbf reads from those files with `deny_unknown_fields`
  (`desired.rs`: `Desired`, `MapFile`; `ops.rs`: `OpsState` and every struct inside it), the
  keys it allows, the keys it requires and the schema it reads, with the line of the struct at
  the tag. `tests/state_files.rs` mirrors these readers over the files this coordinator writes.

## Sequences

`sequences/*.json`: calls at a fake clock and what kbf's coordinator answered, each transcribed
from a kbf test that asserted the answer; `from` names the test and says how the transcription
differs from it. The coordinator starts at second 1000 (kbf's test rig did) and a call is made at
`at_secs`. `tests/replay.rs` replays them through `http::handle`; `tests/support/replay.rs` says
what every key means.

Every step but a `status` (and a `restart`, which has no answer) records the full answer in
`expect_body`; the replay refuses a step that has none, naming it. A `status` step records the
full answer or, where kbf's test asserted a value in a Rust type (a field of a parsed answer, the
marks of a service), `expect_contains` holds the fragment of the answer's bytes that value is, in
kbf's serialiser order; it must have one or the other. `expect_contains` and
`expect_not_contains` may also sit beside an `expect_body` of any step. Sequence
`13-kbf-status-and-convergence-bytes` holds kbf's status and convergence bytes in full.

Where kbf's test asserted a literal, `expect_body` is the literal. For the steps kbf's tests did
not assert in full (a heartbeat made only to keep a host in the fleet, a convergence or an
operator answer asserted by its fields, a refused body), `expect_body` was derived from kbf
v0.0.64's source: the rules that decide the answer and the serialiser of the answer, read at the
tag, and found to be the same in this coordinator's source. These bodies were not recorded from a
running kbf.

| file | kbf source |
|---|---|
| `01-farm-mtls` | `tests/farm.rs`: an enrolled updater, the control credentials |
| `02-farm-restart` | `tests/farm.rs`: after a restart |
| `03-golden-first-pass` .. `07-golden-rollback-order` | `src/decide_golden_tests.rs` |
| `08-halt-raise-hold-clear` .. `10-halt-generations-of-two-services` | `src/http_tests/halt.rs` |
| `11-service-order-only-to-an-updater-that-sent-services` .. `13-kbf-status-and-convergence-bytes` | `src/http_tests/services.rs` |
| `14-v0018-updater-and-deploy-farm` | `src/http_tests.rs` |
| `15-features-an-agent-that-announced-nothing` .. `18-features-every-new-fact-is-accepted` | `src/http_tests/features.rs` |
| `19-bake-holds-the-slot`, `20-canary-of-one` | `src/bake_canary_tests.rs` |
| `21-deferral-frees-the-slot`, `22-deferral-backoff-grows` | `src/defer_tests.rs` |
| `23-automatic-rollback-marks-the-version-bad` | `src/rollback_tests.rs` |
| `24-cut-never-last-does-not-delay-an-order` .. `30-cut-require-min-up-does-not-refuse-a-body` | not a kbf test: the cases where a cut rule decided (below) |

Two things differ from kbf's bytes on purpose, both additions to the answer: `{COMMIT}` stands
for the commit this build was made from (kbf's tests appended their own the same way), and
`"agent":{"name":"kbf-updater"},` in each host of a status, the agent this coordinator shows for
a host that sent none, is taken out of the status before it is compared.

## Where a cut rule decided

Sequences `03` to `07` were transcribed from kbf goldens that asked each host without noting an
order; the rules this coordinator cut (the order of roles, the `never_last` floor) showed there.
A recorded sequence goes through the coordinator, which notes each order, and the host asked
after the first order is held, in kbf by the one live update (`no_slot`) and here by the first
stage of the service's update plan, which holds one host, so every answer of `03` to `07` is the
same in kbf and here; the reason here is the plan's.

| sequence | the cut rule | kbf: answer and reason | here: answer and reason |
|---|---|---|---|
| `24-cut-never-last-does-not-delay-an-order` | the `never_last` floor of one | `nothing`, `min_up` (no other frontend up) | `update 2.0`, no reason (nothing holds it) |
| `26-cut-role-order-does-not-delay-an-order` | a role's `after` (a frontend waits for storage) | `nothing`, `waiting_for_role` (storage host `s1` behind and live) | `update 2.0` |
| `28-cut-convergence-does-not-need-min-up` | the convergence minimum | `in_progress` (0 of the 1 storage host required up) | `converged` |
| `29-cut-service-order-does-not-delay-an-order` | a service's `after` | `nothing`, `waiting_for_service` (a host of `first`, which `second` came after, was behind and live) | `update` of `second` |
| `30-cut-require-min-up-does-not-refuse-a-body` | `require_min_up` | 400 `bad_request` (`min_up lacks storage`) | 200 and the echo of the state, with `min_up` written `{"storage":0,"frontend":0}` |
| `17-features-a-bad-announcement-is-dropped`, step 5 | the bound of 8 feature names, raised to 16 | `nothing` with no `features` (the list of nine is dropped) | `nothing` and `features: []` (nine names read, none supported) |
| `10-halt-generations-of-two-services`, steps 6 and 9 | the update plan, which audits the stage it enters | the clears numbered 5 and 7 | the clears numbered 6 and 8: kbf's plan entered its second stage at step 4 (`rollout_advance`, by the coordinator) |
| `23-automatic-rollback-marks-the-version-bad`, step 10 | the update plan: a failed host of a stage holds the next | `update 2.0`, no reason (the one live update was free once `d1` beat healthy) | `nothing`, `stage_failed` (`d1`, the first stage's host, failed below the threshold) |

`kbf_status` and `kbf_body` of a step keep kbf's answer; the replay never compares them.
`tests/replay.rs` lists these eight in `CHANGED` and `check_changed_list` fails when a sequence
keeps kbf's answer and is not listed, or is listed and keeps none, or keeps an answer equal to
this coordinator's.

Not transcribed: `tests/container_unit.rs` (packaging of the container is not part of this
repository) and the kbf tests that break a file system or a clock on purpose (the sequences are
calls on a working coordinator).
