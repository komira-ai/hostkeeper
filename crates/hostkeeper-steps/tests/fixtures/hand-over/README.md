# A hand-over of hostkeeper itself, waiting for its restart

`record.json` is the step record of an update of the service `hostkeeper` as a release that hands
over writes it (docs/design/two-programs.md, section 4.3): `done` at `cleanup`, waiting for the
restart into `1.5.0`, with the two additive fields of the wait, `next` (the version the link
`next` of the binary directory was pointed at) and `starts` (the hand-over's start count, 0 when
it is written). No release writes it yet. `tests/handover.rs` has a reader of today load it and
settle it, as the binary `current` names (which execs the next one) and as the binary it waits
for (which confirms it), to prove that the reader comes before the writer (AGENTS.md B8).

It is committed as written and never regenerated.
