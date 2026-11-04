# Step records from before `order_try`

`finished.json` is a step record of schema 1 as hostkeeper wrote it before the record carried the
coordinator's try of the order (`order_try`). It is committed as it was written and never
regenerated: `tests/record.rs` reads it with the current binary to prove that the field is
additive (AGENTS.md B8).
