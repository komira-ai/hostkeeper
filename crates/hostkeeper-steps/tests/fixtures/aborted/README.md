# A step record of an aborted update

`finished.json` is a step record of schema 1 as hostkeeper writes it for an update an operator
aborted: `Finished{Halt}` with the reason `aborted_by_operator`. The abort adds no field and no
schema: its reason is a value of the record's existing `reason`. The file is committed as it was
written and never regenerated: `tests/abort.rs` reads it with the record reader, and checks that
writing it back gives the same JSON, so a reader of schema 1 from before the abort reads it too
(AGENTS.md B8).
