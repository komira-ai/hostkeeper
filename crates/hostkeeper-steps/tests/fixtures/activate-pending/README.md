# The update of hostkeeper itself, its own package's `activate` still to be done

`record.json` is the step record of an update of the service `hostkeeper` whose new binary has
run and confirmed itself, and whose own native package (a `.deb` through `apt`, or a `.pkg`
through `installer`) is still to be installed: `done` at `cleanup`, with the additive field
`progress.activate` (docs/design/packages-native.md, section 10.4, step 5). The wait for the
restart is gone from it, and no verdict is in it yet.

`tests/selfupdate_activate.rs` reads it with a struct of the record as the release before the
field had it (with `awaiting_restart` and `report_order_id`, without `activate`), to prove that
such a binary still loads it, reads it as unfinished and reports no verdict of it (AGENTS.md B8).

It is committed as written and never regenerated.
