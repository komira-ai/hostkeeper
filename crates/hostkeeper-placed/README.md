# hostkeeper-placed

The host's record of what the daemon installed, and the links it switches. Why it is shaped this
way: `docs/design/hostkeeper-design.md`, section 4.7, and `docs/design/packages-native.md`, section
10.7. This page states what the code holds. It is a library with its fakes; the daemon calls it
to record an installed service, to switch `current` and `previous`, to tombstone a service being
uninstalled and to reload the service manager.

A package manager puts a package's files where they belong; nothing here links a package into
the system. `placed.json` in the state directory records, per service, the installer and (for a
repository package) the package, version, repository and the enablement links its installer
wrote, and the tombstone of a service being uninstalled.

| module | what it holds |
|---|---|
| `roots` | `Roots` (state directory, unit directory, package directories) and `PackageDirs` (`<packages>/<service>/{current,previous,<version>}`). Paths are the host's spelling. |
| `fs` | The `Fs` port every read and write goes through, and `RealFs`, which reads and writes the host's paths under a root (`/` on a host, a scratch directory in a test). |
| `store` | `PlacedRecord` and `Store`: `placed.json`, schema 1, written by temporary file, `fsync`, rename and directory `fsync`. Keys it does not know are kept on rewrite, the `paths` and `reload_pending` of an earlier release's placed-path linking among them, so a release that reads them finds them; a file that does not parse, has a newer schema, or whose known fields break their grammars (service names) is an error naming the file and is never rewritten. `save` takes the lock (`PlacedLock`). The `uninstalling` tombstones live here, each with how far its uninstall got (`hook_done`, `halted`, written only when set). |
| `placer` | `Placer`: `current` and `previous` (the version a link names, which must be a version beside it), `switch_current` (`previous` first, `current` second, each link made under a temporary name and renamed over the old one), `lock` (the lock of `placed.json`) and `reload` (`daemon-reload` under it). |
| `unit_control` | `UnitControl` (`daemon_reload`, `stop_unit`, and for a `force` `start_unit` and `is_active`), the write side of the service manager, depending on `std` and `thiserror` only; `Systemctl`, its systemd form, one `systemctl` command per call through a `CommandLauncher` (`is-active --quiet`: exit 0 is active, anything else is not). |
| `testing` | `FaultFs` (records every write, fails the n-th and every later one, and fails a chosen read once or from then on), `FakeUnitControl` (keeps which units are active, and fails a start, a stop of one unit, a reload or every call when told) and `FakeLauncher`, which record their calls; a test puts its own events among them with `Event::Mark`. |

`placed.json` is read and written, and `daemon-reload` run, under the lock file
`<state dir>/placed.lock`, taken by each call for its duration. Temporary names are `.hk~<owner>~tmp`: `~` is outside the path grammar, so no placed path can have one. Every error is a `PlacedError`
whose `class()` and `reason()` say how a step or force record reports it: a `current` or
`previous` that is not a link to a version beside it is class release, `path_conflict`.
