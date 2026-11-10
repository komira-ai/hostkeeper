# The tierstore-shaped fixture

The package the privileged end-to-end test installs, updates and rolls back through the real
daemon, coordinator, apt and systemd. It has the shape of the production service `tierstore`: one
binary that is both the workload (`serve <port>`, answering `GET /healthz` with
`version=<version>`) and its own update hook (`update-hook prepare|cutover|verify|rollback|cleanup`),
two run units that are instances of one template unit, a loopback health URL with `{port}`, and a
`prepare` that waits on a drain gate.

It is a `.deb` of the service `hostkeeper-ci-tierstore`, with no maintainer script, in the layout
of a native package (docs/design/packages-native.md, section 10.2):

- `usr/bin/tierstore-like`: the program, which the run units' `ExecStart=` runs;
- `usr/lib/hostkeeper-ci-tierstore/hostkeeper-hook`: the program again, as the hook at the fixed
  path the daemon runs from the sealed tree;
- `usr/share/hostkeeper-ci-tierstore/release.toml`: the version and the run units of the release,
  which the program reads;
- `usr/lib/systemd/system/hostkeeper-ci-tierstore@.service` and one plain unit file
  `hostkeeper-ci-tierstore@<port>.service` for each of the version's two ports (a `run` unit is a
  file of the package), each the unit template.

Nothing binary is committed. The binary is the `tierstore-like` program of
`tests/bin/tierstore_like.rs`, built by cargo with the crate's tests. `tests/e2e/package.rs` renders
`release.toml.tmpl` with the placeholders `{{service}}`, `{{version}}`, `{{port_a}}` and
`{{port_b}}`, puts the unit template (`units/hostkeeper-ci-tierstore@.service.tmpl`, which has no
placeholder) in the package as it is, and builds the `.deb` and its entry (the hook's bounds and
params, the `run` units, the health URL) at test time.

To add a version, add a `FixtureVersion` (version text, two ports, whether its `verify` fails) to
the scenario in `tests/e2e/scenario.rs`; the builder does the rest. A change to the release file
or the unit is a change to the templates here, and `hostkeeper-install`'s tree rules and its
reading of an entry judge it in the root-free `tests/tierstore_rig.rs`, and the daemon when the
test installs the package.
