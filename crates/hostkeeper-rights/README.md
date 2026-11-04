# hostkeeper-rights

The transient systemd unit that runs one step of a hook, and the part of the old per-service
declaration it is still built from. The declaration loader (`/etc/updater.d`), the widening check,
the approval store and its `approve` command, the gate, and the account and reserved-path checks
of the loader are deleted: the host's daemon config and the manifest of each package replace the
declaration (`hostkeeper-config`, design `docs/design/hostkeeper-design.md` sections 4.1 and 4.7).
B3 rewrites `launch` to the unit of design 5.3 and deletes what is left here.

The crate names no product and depends on no crate of this workspace but the hook contract
(`hostkeeper-hook`) and the config crate (`hostkeeper-config`, for the service name and path
grammars); `crates/xtask/tests/layering` keeps both true.

| module | what it holds | why it is kept |
|---|---|---|
| `launch` | The `systemd-run` command of one step as a pure function (`command`, `properties`, `unit_name`), `run_step` over a `Spawner`, and `Systemd`, the real one. | Unchanged; B3 rewrites it. |
| `declaration` | `Declaration`, `Hook`, `Timeouts`, `StepCapabilities`: the fields `launch` reads (name, binary, hook arguments, record directory, user, group, capabilities, read and write paths, address families, environment, step bounds). `Declaration::parse` refuses an unknown key and checks the grammar of each value that reaches a unit property, and a step bound from 1 s to a day. `Refusal`: the refusals of `parse` and of `recheck_targets`. | `launch` builds the unit from these. |
| `load` | `Declared`: a declaration with the uid of its hook account and the host's declared names. `recheck_targets` (and `Declared::recheck`): the reserved-path rules on what each path points to now, and the rules of a hook that runs as root, which `run_step` applies before every launch; with the deny list they use (`RESERVED`, `WRITE_ROOTS`, `HOOKS_ROOT`, `check`). | `run_step` calls `recheck_targets`; its behaviour is unchanged until B3. |
| `names` | The account, environment, capability and address-family grammars; the service name and path grammars are re-exported from `hostkeeper-config`. | `Declaration::parse` and `Hook` use them. |

## Choices kept from the declaration

- **`ReadOnlyPaths=` does not hide anything.** `ProtectSystem=strict` leaves the whole tree
  readable, so `read_paths` says what the hook is declared to read and confines nothing.
- **An empty address family list is `RestrictAddressFamilies=` with an empty value.** Through
  `systemd-run -p` that is an empty allow list (every family refused). `none` is refused by
  systemd as an unknown family.
- **The targets are checked again.** `recheck_targets` repeats the reserved-path rules on what each
  entry points to now; `run_step` calls it before every launch and refuses the launch
  (`RunError::Recheck`) when a target changed, because a link can be re-pointed after the launch
  was built.
- **A root hook's paths cannot be swapped.** For a hook that runs as root every existing directory
  above a read or write path (and above what it resolves to) must be owned by root and closed to
  the group and others, each write path is at or below `<write root>/<service>` or a name
  beginning with the service's and a `-`, and none is in the directory of another declared service.
  "Root" is the hook account's uid 0 (`Launch::root_hook`), whatever its name.
- **Environment names.** `LD_*`, `MALLOC_*`, `PYTHON*`, `DYLD_*`, `GIT_*` and a list of loader,
  resolver, shell and interpreter names (`names::HIJACKING_ENV`) are refused for every hook, and
  `PATH` only as a list of absolute directories.

## What polkit can and cannot scope

A rule for the `manage-units` action limits an account to the units it lists. But `systemctl mask`
needs `manage-unit-files`, whose policy implies `manage-units` for every unit, so an account that
holds it can manage any unit. Hooks therefore never mask, and the launcher never grants
`manage-unit-files` (a test holds that).

## Tests

`tests/` runs without root, and is what a pull request runs. The tests that need root and the real
systemd run the probe hook `tests/bin/probe_hook.rs` (`hostkeeper-rights-probe`); they are not in
this repository yet and come with the privileged workflow (MIGRATION.md).
