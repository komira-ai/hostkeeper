# hostkeeper-steps

The step runner of the generic host updater: the daemon's engine for one update of one service.
Why it is shaped this way:
[docs/design/hostkeeper-design.md](../../docs/design/hostkeeper-design.md) (5.3 and 6.2 to 6.6). This
page states what the code holds.

The crate names no service and reads no manifest or daemon config; the loaders of both are in
`hostkeeper-config`. It runs every step through a `StepLauncher`; `SystemdLauncher` (`systemd.rs`)
is in this crate and is the launcher of the host, one transient systemd unit per step (design
section 5.3). The contract types (steps, exit codes, the JSON last line, the
params table, the next-step table) are those of `hostkeeper-hook`.

| module | what it holds |
|---|---|
| `runner` | `StepRunner::run`: one update from `prepare` to its verdict. Writes the step record before and after each step, asks `hostkeeper_hook::next` what follows, resumes from the record, runs the post-success watcher for `promotion_pending` |
| `record` | the step record (`schema`, service, target version, artifact digest, generation, step, attempt, state `pending`/`running`/`done`/`finished`, what the runner remembers between steps), written by temporary file, fsync, rename, directory fsync; a record of a newer schema is never changed. Two optional fields, written only when set: `forced`, set by a `force` that stops the update (the runner then closes it `Failed`, `forced_over`, and launches nothing more), and `force`, the journal of a `force` |
| `force` | `ForceRecord`: the journal of a hookless `force` (target, digest, the version left, the phase reached, the order's try and halt generation, the verdict), whose phase and verdict strings are fixed |
| `artifact` | `Staged`, `tree_digest` and `read_tree`: the staged tree and its digest at verification (the public format of docs/tree-digest.md: kind, name, mode, size and bytes of every entry; no owner, no times, no links), made only by `Staged::seal`; checked before every step, resumed ones included |
| `seal` | `Seal`: the seal file `<version>.seal` of a staged package version (`schema`, `tree_digest`, `package_digest`), read additively |
| `launcher` | `StepLauncher`, `StepCall` and `Bound`: what the runner needs to run one step and to stop its unit by name, a step's bound (seconds, or `Unbounded` for a `prepare` of 0), and what the launcher must do that the runner cannot check |
| `systemd` | `SystemdLauncher`: each step as `systemd-run --system --quiet --wait --collect --pipe --unit=updater-<service>-<step>-<generation> --property=RuntimeMaxSec=<secs\|infinity> -- <hook> <args>`, run as root with no other property (no user, capability, sandbox or path property); `stop` is `systemctl stop --system <unit>`, a unit that is not loaded being stopped. The command lines are pure functions (`command`, `stop_command`, `unit_name`); `SystemdRunProcess`, the one place that starts a process, runs the programs with an empty environment and no input and reads both output streams at once |
| `capture` | `Ring`: the last `RING_BYTES` (1 MiB) of each output stream of a step; the last line of standard output is the report only when it is at most `LAST_LINE_MAX` (64 KiB) and whole, otherwise the report is `{}` and the exit code decides |
| `deadman` | `Watch` and `DeadMan`: the bound of the running step, enforced on a thread of its own; a stop that did not work is reported to the runner; an unbounded `prepare` is not armed |
| `selfupdate` | the updater's own update: stage beside (never over what `current` or `previous` names), switch the `current` link, restart; the `previous` link. `follow_on` takes the binary from a `Staged` through `Staged::check`. `Layout` is the directory of versions with `current` and `previous`: the updater's binaries (`Layout::new`, `versions/<v>`) or a package's sealed trees (`Layout::package`, `<v>/` with `<v>.seal`, checked by `check_tree`) |
| `clock` | `Clock`, `Stamp`, `SystemClock`: time and the boot id as inputs |

## Rules the code keeps

- Nothing from a release runs unless its tree still has the digest the updater verified. A mismatch
  at any step, a tree in a place others can write, or a tree that holds what is neither a regular file nor
  a directory (a link included) is a `halt` with the reason `artifact_changed`, and nothing from the tree
  runs. A tree that cannot be read for a moment (an I/O error, out of descriptors) is no halt:
  the runner stops with `RunError::Unreadable`, the step stays `pending`, and the next run checks
  again.
- A step found `running` after a crash has its unit stopped by name (`updater-<service>-<step>-<generation>`)
  before it is run again; a step found `done` is not run again.
- **A step can run more than once.** The record says `running` before the step starts and `done`
  only after it ended; an updater that dies between the end of the step and that write finds the
  step `running`, stops its unit and runs it again. The same holds for a step the dead-man could
  not stop at its bound (`RunError::Unstopped`). Every step of a hook must therefore be safe to
  run again after it has run to its end, which the hook contract asks of it.
- `cleanup` runs only after `verify` succeeded and is never rolled back; the watcher never rolls back.
- `rollback` runs the new release's hook, and the previous release's only when the new hook cannot
  start at all.
- The updater writes one params file per update, in its private state directory (refused unless owned by the running user and closed to others), names it in the step record, passes the same path to every step including those after a resume, and removes it when the update ends.
- Tests never wait for a time: a fake clock moves when a `sleep` or a test moves it, the dead-man thread
  is ended by a handshake, and crash tests stop the updater at every point of every record write.

What the launcher must do (the `StepLauncher` docs): start a unit so that its name can be used
again once it ended (`SystemdLauncher` uses `--collect`, so no `systemctl reset-failed`), since a
step that is tried again keeps its unit name; bound its `stop` and escalate to `SIGKILL` (systemd's
default stop timeout, then the kill of the control group); and set `RuntimeMaxSec=` from the
step's bound (`infinity` for an unbounded `prepare`), since a killed updater takes the dead-man
with it.

- **An unbounded `prepare` (`prepare = 0`)** is recorded `running` with `unbounded: true` (written
  only when true, so a bounded step writes the bytes it always did), is not armed with the
  dead-man, and runs with `RuntimeMaxSec=infinity`: only the hook's own end ends it. A `0` for any
  other step, or a `verify` whose window is used up, is a step not started and failed.

What is not here: the heartbeat link to the coordinator (`hostkeeper-link`), and wiring a
binary (`hostkeeper-daemon`). The registration socket of kbf's step runner was not carried over (MIGRATION.md): the state of a
service is read from the service manager instead. The root-only checks (the real systemd restart
limit) need root and are not part of this crate's fast tests.

## Limits the caller should know

- A host that reboots in a loop restarts the verify window at every boot (the monotonic clock
  restarts, so the time spent before the reboot is unknown): such a host is not rolled back by the
  window, and the dead-man bounds each boot's `verify` only.
- `Layout` (the updater's own binary directory) must be given as a root-owned directory closed to
  group and others, in root-owned parents; the type does not check it.
- `UpdateOrder.artifact_digest` is passed through. The step that stages the package must see that
  the digest is of what it staged; this crate does not bind the digest to the tree.
- The staged tree is checked by path before every step; the guarantee that it cannot be swapped
  between the check and the exec rests on the tree being root-owned and read-only in root-owned
  parents.
