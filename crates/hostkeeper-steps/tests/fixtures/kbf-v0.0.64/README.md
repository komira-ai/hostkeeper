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
| `newer-schema.json` | schema 2, a field this binary does not know and a state it does not know |
| `README.md` | this file |

The reader contract, `SCHEMA` 1: a record of schema 1 is read as the record it is
(`Loaded::Current`); a record of a newer schema is read as `Loaded::Newer` with its schema, and is
never rewritten. Both are held by `reads_step_record_written_by_kbf_tag` and
`a_newer_kbf_record_is_read_only`.
