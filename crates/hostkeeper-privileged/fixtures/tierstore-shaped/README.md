# The tierstore-shaped fixture

The package the privileged end-to-end test installs, updates and rolls back through the real
daemon, coordinator and systemd. It has the shape of the production service `tierstore`: one
binary that is both the workload (`serve <port>`, answering `GET /healthz` with
`version=<version>`) and its own update hook (`update-hook prepare|cutover|verify|rollback|cleanup`),
two run units that are instances of one template unit, a loopback health URL with `{port}`, and a
`prepare` that waits on a drain gate.

Nothing binary is committed. The binary is the `tierstore-like` program of
`tests/bin/tierstore_like.rs`, built by cargo with the crate's tests. The test renders the two
templates here (`manifest.toml.tmpl`, `units/hostkeeper-ci-tierstore@.service.tmpl`) with the
placeholders `{{service}}`, `{{version}}`, `{{platform}}`, `{{port_a}}`, `{{port_b}}`,
`{{record_dir}}`, `{{fail_verify}}` and `{{packages}}`, and `tests/e2e/package.rs` packs them with
the binary into `<service>-<version>-<platform>.tar.zst` in memory, at test time.

To add a version, add a `FixtureVersion` (version text, two ports, whether its `verify` fails) to
the scenario in `tests/e2e/scenario.rs`; the builder does the rest. A change to the manifest or
the unit is a change to the templates here, and the lint rules of `hostkeeper-config` and
`hostkeeper-placed` judge it when the test installs the package.
