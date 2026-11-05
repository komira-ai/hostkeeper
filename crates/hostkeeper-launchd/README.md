# hostkeeper-launchd

The macOS side of the host daemon: the same daemon, flow, config and package paths as on Linux, with
the ports that differ implemented over launchd. How it is meant to run:
[docs/macos.md](../../docs/macos.md). This page states what the code holds. It is tested against
fakes on Linux and has not been run on a Mac.

| module | what it holds |
|---|---|
| `label` | `REVERSE_DNS_PREFIX`, `Label` (1 to 255 bytes of letters, digits, `.`, `_`, `-`): the daemon's label, a step job's label, and the label of the job for a step unit name as the step record names it |
| `plist` | the XML property lists of the daemon job, of a step job and of a service package's job (`daemon_plist`, `step_plist`, `service_plist`: kept alive while a path exists, not started at load); control characters are refused. `daemon_plist` is the daemon's one plist, the self-update's included |
| `launchctl` | the `Launchctl` port (one command), its real adapter `LaunchctlProcess` with a deadline and bounded output, the reader of `launchctl print system/<label>`, and print and bootout by label |
| `fake` | `FakeLaunchctl`: scripted answers per exact arguments, and the calls made |
| `manager` | `LaunchdServiceManager`: the observer's `ServiceManager` over `launchctl print` |
| `procid` | `LaunchdProcessProbe`: the observer's `ProcessProbe` over `proc_pidpath` and a device and inode check; the `PidInfo` and `FileIdentity` ports |
| `launcher` | `LaunchdLauncher`: the step runner's `StepLauncher`, each step a transient launchd job read until it ends, booted out at its bound (none for an unbounded `prepare`), its output kept in the same capture ring as on Linux; the `Pace` port for its clock. `LaunchdLauncher::system` is compiled on macOS only |
| `unit_control` | `LaunchdUnitControl`: the linking side's `UnitControl` over `launchctl`, reloading the plists that were linked, changed or removed |
| `platform` | `Platform`: the unit directory, unit suffix, admin socket and label prefix of Linux and of macOS |
