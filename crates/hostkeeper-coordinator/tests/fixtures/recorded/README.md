# Recorded fixtures

Data recorded from kbf's update coordinator at its tag `v0.0.64` (commit
`8f90d0604794f3fee057e71647c7e1c1e034fa2e`). The tests in `crates/hostkeeper-coordinator/tests`
read them as they are; none is generated at test time. A file is changed only to record kbf
again, never to make a test pass.

## Services and flags

- `kbf.services.toml`: kbf's embedded `kbf.services.toml` as an entry of a services file, plus
  `require_min_up = ["storage", "frontend"]` (the `min_up` keys kbf hard-coded for its own
  service).
- `tierstore.services.toml`: kbf's `tierstore.services.toml`, unchanged.
- `services.toml`: the two in one file, kbf first. This is the `--services-file` of the
  replays unless a sequence names another.
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

Where kbf's test asserted a literal, `expect_body` is the literal. Where it asserted a value in a
Rust type (a field of a parsed answer, the marks of a service), `expect_contains` holds the
fragment of the answer's bytes that value is, in kbf's serialiser order. A heartbeat with no
`expect_body` only keeps a host in the fleet: kbf's test made no assertion about its answer.

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

Two things differ from kbf's bytes on purpose, both additions to the answer: `{COMMIT}` stands
for the commit this build was made from (kbf's tests appended their own the same way), and
`"agent":{"name":"kbf-updater"},` in each host of a status, the agent this coordinator shows for
a host that sent none, is taken out of the status before it is compared.

Not transcribed: `tests/container_unit.rs` (packaging of the container is not part of this
repository) and the kbf tests that break a file system or a clock on purpose (the sequences are
calls on a working coordinator).
