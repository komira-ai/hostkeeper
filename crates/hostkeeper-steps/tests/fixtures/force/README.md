# The strings of a force record

`strings.json` holds the on-disk spelling of every phase (`ForcePhase`) and every verdict
(`ForceVerdict`) of the `force` field of a step record, as this daemon writes them. It is written
by hand and never regenerated: `tests/force_record.rs` compares what the code writes with it, so
that renaming a phase or a reason, which a record already on a host would no longer read as,
fails a test (AGENTS.md B8).
