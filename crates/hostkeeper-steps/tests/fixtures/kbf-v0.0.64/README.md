# Step records written by kbf v0.0.64

Each `.json` file here is a step record in the bytes that kbf's step runner writes at the tag
`v0.0.64` (commit `8f90d0604794f3fee057e71647c7e1c1e034fa2e`): `RecordStore::save` writes
`serde_json::to_vec_pretty` of the `Record`, two-space indent, no trailing newline. The bytes were
derived from that serializer (field order, `snake_case` names, the `state` tag, the externally
tagged `Verdict`) and are checked by `writes_the_bytes_kbf_tag_wrote` in `tests/kbf_tag.rs`:
hostkeeper's writer produces the same bytes from the same record.

| file | what it is |
|---|---|
| `pending.json` | a step chosen and not started: state `pending`, no previous digest, hook `new`, default progress |
| `running.json` | state `running` with its start stamp and deadline, hook `new`, `verify_started` set |
| `done.json` | state `done` with exit code 75 and the hook's last line, hook `previous`, a non-default progress (`then`, `retry`, `reason`) |
| `finished.json` | state `finished` with the verdict `Succeeded`, no reason, `promotion_pending` and `watching` set |
| `reason-absent.json` | `pending.json` with `progress.reason` left out (the field is `#[serde(default)]`): loads with `reason` `None` |
| `unknown-field.json` | `pending.json` with an unknown top-level field and an unknown field in `progress`, schema 1 (the record has no `deny_unknown_fields`): loads as `pending`, and a save drops them |
| `newer-schema.json` | schema 2, a field this binary does not know and a state it does not know |
| `README.md` | this file |

The reader contract, `SCHEMA` 1: a record of schema 1 is read as the record it is
(`Loaded::Current`); a record of a newer schema is read as `Loaded::Newer` with its schema, and is
never rewritten. Both are held by `reads_step_record_written_by_kbf_tag` and
`a_newer_kbf_record_is_read_only`.

## How these bytes were produced

The committed bytes were derived from the tag's serializer, `serde_json::to_vec_pretty` of
`Record`, and are checked against this repository's writer. This repository's record module is
byte-identical in code to the tag's (see the import diff), so the tag's writer and reader are the
same code as the writer and reader tested here.

To regenerate them with kbf's own writer:

1. Check out `komira-ai/kbf` at `v0.0.64` (commit `8f90d0604794f3fee057e71647c7e1c1e034fa2e`).
2. Add a test to `crates/kbf-step-runner` that builds the same records as `expected()` in this
   crate's `tests/kbf_tag.rs` and calls `RecordStore::save` for each into a scratch directory.
3. Compare the bytes of each `step-record.json` it writes with the matching `.json` file here
   (`pending`, `running`, `done`, `finished`). `reason-absent.json` and `unknown-field.json` are
   hand-edited copies of `pending.json` (one field removed, two fields added) and `newer-schema.json`
   is hand-written; none of them is written by a writer.

## Reading in both directions

- kbf's records are read here: `reads_step_record_written_by_kbf_tag` (the four states),
  `reads_a_kbf_record_without_the_defaulted_reason` and `reads_a_kbf_record_with_an_unknown_field`
  (a save of the loaded record writes the canonical bytes, without the unknown fields), and
  `a_newer_kbf_record_is_read_only` (schema 2).
- What this repository writes is the tag's bytes: `writes_the_bytes_kbf_tag_wrote`, so the tag's
  reader reads it.
