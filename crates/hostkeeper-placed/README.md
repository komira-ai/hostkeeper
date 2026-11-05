# hostkeeper-placed

Placed-path linking: the daemon, not the hook, puts a package into the system. Why it is shaped
this way: `docs/design/hostkeeper-design.md`, section 4.7 ("Units, enable and boot", "Paths",
"Placed paths", "Uninstall"), section 6.5 (boot) and section 8.10. This page states what the code
holds. It is a library with its fakes; the daemon calls it for install, update, force_rollback,
force and uninstall, and to reconcile at start.

Each file of a package's `units/` becomes a symlink at `<unit dir>/<name>`, and each `[[files]]`
entry a symlink at its `path`, each pointing into
`/var/lib/hostkeeper/packages/<service>/current/`, so one switch of `current` re-points all of
them. `placed.json` in the state directory records which service owns each placed path.

| module | what it holds |
|---|---|
| `roots` | `Roots` (state directory, unit directory, package directories) and `PackageDirs` (`<packages>/<service>/{current,previous,<version>}`). Paths are the host's spelling. |
| `fs` | The `Fs` port every read and write goes through, and `RealFs`, which reads and writes the host's paths under a root (`/` on a host, a scratch directory in a test). |
| `store` | `PlacedRecord` and `Store`: `placed.json`, schema 1, written by temporary file, `fsync`, rename and directory `fsync`. Keys it does not know are kept on rewrite; a file that does not parse, has a newer schema, or whose known fields break their grammars (absolute paths, service names, versions) is an error naming the file and is never rewritten. `reload_pending` says a `daemon-reload` is owed until one succeeds. `save` takes the lock (`PlacedLock`). The `uninstalling` tombstones live here, each with how far its uninstall got (`hook_done`, `halted`, written only when set). |
| `claim` | `Claims::of(roots, manifest)`: the paths a version claims, with the rules on them (a unit file's name is the service's; a `[[files]] path` is absolute, has no `.` or `..`, is under none of the refused prefixes, compared on whole components; a `source` is below `files/` with no `.` or `..`; no path twice, and none below another). |
| `lint` | The load rules of linking: `[units] run` entries are linked units or instances of linked templates; command lines (`ExecStart=` and the other `Exec*=` keys, a plist's first `ProgramArguments` element) name package files only through `<packages>/<service>/current/`, read as systemd reads them (prefixes, quotes, backslash escapes, `.` and `..` resolved; a plist's comments skipped and references decoded); each `run` unit has an `[Install] WantedBy=` of plain unit names of a known type (no quotes, escapes or `%` specifiers). |
| `check` | `check(claims, record, fs)`: the `path_conflict` refusal, which reads only: a path another service owns, or one above or below it, or one occupied by anything but a link the record lists for the claimant. |
| `placer` | `Placer`: `claims` (the claims with the command lines checked), `begin` (which checks them again), `link` (a missing link made in one step that fails if anything is there), `switch_current`, `reload`, `commit` (the write-ahead order of design 4.7), `relink_to`, `unlink_unlisted`, `remove_all`, `enable_run_units`, `disable` and `reconcile` (the start-up pass of design 6.5, given the `SwitchIntent`s the caller read from its records). |
| `unit_control` | `UnitControl` (`daemon_reload`, `stop_unit`, and for a `force` `start_unit` and `is_active`), the write side of the service manager, depending on `std` and `thiserror` only; `Systemctl`, its systemd form, one `systemctl` command per call through a `CommandLauncher` (`is-active --quiet`: exit 0 is active, anything else is not). |
| `testing` | `FaultFs` (records every write, fails the n-th and every later one, and fails a chosen read once or from then on), `FakeUnitControl` (keeps which units are active, and fails a start, a stop of one unit, a reload or every call when told) and `FakeLauncher`, which record their calls; a test puts its own events among them with `Event::Mark`. |

`placed.json` is read and written, and `daemon-reload` run, under the lock file
`<state dir>/placed.lock`, taken by each call for its duration. Temporary names are `.hk~<owner>~tmp`: `~` is outside the path grammar, so no placed path can have one. Every error is a `PlacedError`
whose `class()` and `reason()` say how a step or force record reports it: a conflict is class
release, `path_conflict`.
