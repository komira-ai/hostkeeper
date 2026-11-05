# The update of hostkeeper itself, waiting for its restart

`record.json` is the step record of an update of the service `hostkeeper` whose steps ran and
whose new binary is in place but has not run yet: `done` at `cleanup`, with the additive field
`progress.awaiting_restart` (design section 6.6). `tests/selfupdate_settle.rs` reads it with a
struct of the record as it was before the field existed, to prove that an older binary (the one
the fallback runs) still loads it and does not read it as finished (AGENTS.md B8).

`bad-versions-later.json` is a list of bad versions as a later release might write it: a newer
`schema` and a key this release does not know. This release must still read its versions, and
keep the unknown key when it writes the list again.

Both are committed as written and never regenerated.
