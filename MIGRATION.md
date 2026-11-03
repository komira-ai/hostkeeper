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
