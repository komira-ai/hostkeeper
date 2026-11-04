# hostkeeper-placed

Placed-path linking: the daemon, not the hook, puts a package into the system. Why it is shaped
this way: `docs/design/hostkeeper-design.md`, section 4.7 ("Units, enable and boot", "Paths",
"Placed paths", "Uninstall"), section 6.5 (boot) and section 8.10. This page states what the code
holds. It is a library with its fakes; nothing calls it yet.

Each file of a package's `units/` becomes a symlink at `<unit dir>/<name>`, and each `[[files]]`
entry a symlink at its `path`, each pointing into
`/var/lib/hostkeeper/packages/<service>/current/`, so one switch of `current` re-points all of
them. `placed.json` in the state directory records which service owns each placed path.

| module | what it holds |
|---|---|
| `roots` | `Roots` (state directory, unit directory, package directories) and `PackageDirs` (`<packages>/<service>/{current,previous,<version>}`). Paths are the host's spelling. |
| `fs` | The `Fs` port every read and write goes through, and `RealFs`, which reads and writes the host's paths under a root (`/` on a host, a scratch directory in a test). |
| `store` | `PlacedRecord` and `Store`: `placed.json`, schema 1, written by temporary file, `fsync`, rename and directory `fsync`. Keys it does not know are kept on rewrite; a file that does not parse, or has a newer schema, is an error naming the file and is never rewritten. The `uninstalling` tombstones live here. |
| `claim` | `Claims::of(roots, manifest)`: the paths a version claims, with the rules on them (a unit file's name is the service's; a `[[files]] path` is absolute, has no `.` or `..`, is under none of the refused prefixes, compared on whole components; no path twice). |
| `lint` | The load rules of linking: `[units] run` entries are linked units or instances of linked templates; command lines (`ExecStart=` and the other `Exec*=` keys, a plist's first `ProgramArguments` element) name package files only through `.../packages/<service>/current/`; each `run` unit has an `[Install] WantedBy=`. |
| `check` | `check(claims, record, fs)`: the `path_conflict` refusal, which reads only. |
| `placer` | `Placer`: `begin`, `link`, `switch_current`, `reload`, `commit` (the write-ahead order of design 4.7), `relink_to`, `unlink_unlisted`, `remove_all`, `enable_run_units`, `disable` and `reconcile` (the start-up pass of design 6.5, given the `SwitchIntent`s the caller read from its records). |
| `unit_control` | `UnitControl` (`daemon_reload`, `stop_unit`), the write side of the service manager, depending on `std` and `thiserror` only; `Systemctl`, its systemd form, one `systemctl` command per call through a `CommandLauncher`. |
| `testing` | `FaultFs` (records every write, fails the n-th and every later one), `FakeUnitControl` and `FakeLauncher`, which record their calls. |

`placed.json` is read and written, and `daemon-reload` run, under the lock file
`<state dir>/placed.lock`, taken by each call for its duration. Every error is a `PlacedError`
whose `class()` and `reason()` say how a step or force record reports it: a conflict is class
release, `path_conflict`.
