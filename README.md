# hostkeeper

hostkeeper keeps a fleet of Linux and macOS hosts on the software versions an operator wants.
It is planned as:

- a **coordinator** that holds the desired version of each service and moves hosts to it a group
  at a time;
- an **agent** on each host, with a separate root part, that updates each service through the
  service's own update hook (`prepare`, `cutover`, `verify`, `cleanup`, `rollback`), within the
  rights the service declares in `/etc/updater.d`;
- **enrollment**: a new host asks to join, and an operator approves it;
- **device management** of Macs through a self-hosted MDM server.

Every API is a protobuf service, and the command line is generated from those definitions.

**hostkeeper is pre-release.** There is no release yet, and the one program, `hostkeeper daemon`,
serves its admin socket's `Status` and runs no order yet (see [Running the daemon](#running-the-daemon)).
The parts listed above are being moved here from [kbf](https://github.com/komira-ai/kbf), where
they are developed today. So far this repository holds the generic half of the host updater,
moved from kbf ([MIGRATION.md](MIGRATION.md)), the first part of the daemon, and the build, lint
and coverage tooling:

| crate | what it holds |
|---|---|
| [`hostkeeper-hook`](crates/hostkeeper-hook) | the contract between the updater and a service's update hook, and the conformance harness a service tests its hook with (feature `conformance`) |
| [`hostkeeper-config`](crates/hostkeeper-config) | the strict loaders of the host's daemon config (`/etc/hostkeeper/hostkeeper.toml`) and of the `manifest.toml` a package carries, the owner and mode check of a trusted file, and the name and path grammars |
| [`hostkeeper-rights`](crates/hostkeeper-rights) | the transient unit that runs one step of a hook, and the part of the old declaration it is still built from |
| [`hostkeeper-steps`](crates/hostkeeper-steps) | the step runner: the step record and its resume, the dead-man, the staged-artifact check, the updater's own update, and the layout of versions with `current` and `previous`, the tree digest and the seal of a package version |
| [`hostkeeper-unpack`](crates/hostkeeper-unpack) | the unpack of a verified `.tar.zst` package into a sealed version directory, in one streaming pass under the archive bounds ([package format](docs/package-format.md)) |
| [`hostkeeper-daemon`](crates/hostkeeper-daemon) | the daemon, the one root process of a host: start-up and its lock, the executor of orders with the service and step locks, the daemon's own checks, the try rules and the held halts, and the root-only admin socket |
| [`hostkeeper`](crates/hostkeeper) | the binary, `hostkeeper daemon`, which only forwards to `hostkeeper-daemon` |
| [`hostkeeper-proto`](crates/hostkeeper-proto) | the API definitions: the protobuf services of the coordinator (`hostkeeper.agent.v1`, `hostkeeper.operator.v1`) and of a host's root part (`hostkeeper.local.v1`), and the test that keeps them compatible |
| `hostkeeper-testkit` | helpers the tests share |
| `coverage-gate`, `xtask` | the 99% coverage gate and the CI steps |

The design is [docs/design/host-updates.md](docs/design/host-updates.md).

## Running the daemon

`hostkeeper daemon` is the one root process of a host. It takes its configuration from flags
only, never from the environment:

| flag | default | what it is |
|---|---|---|
| `--config` | `/etc/hostkeeper/hostkeeper.toml` | the daemon config, owned by root and not writable by group or others |
| `--state-dir` | `/var/lib/updater/state` | a directory per service with its step record, params file and held halt generation, and the daemon's lock file `lock` |
| `--packages-dir` | `/var/lib/hostkeeper/packages` | a directory per service with its version directories and the links `current` and `previous` |
| `--admin-socket` | `/run/hostkeeper/admin.sock` | the admin socket, mode 0600 in a directory of mode 0700, served to uid 0 only |
| `--notify-systemd` | off | once the boot work is over, send `READY=1` to the notify socket systemd sets for a `Type=notify` unit; without that socket the daemon exits 1 |

At start it loads the config first: a config that does not load stops the daemon with an error
naming the key, and nothing on the host changes. It then takes the lock on the state directory
(one daemon per state directory) and does its boot work: it reads every step record against the
host's boot id (kept in `boot-id` in the state directory), runs each update left unfinished by a
crash or a reboot to its verdict, restarts the post-success watchers, runs each service's
`[hook] boot` step from `current`, and only then tells systemd it is ready. It then serves the
admin socket. This build has no step launcher or package store wired in and no order source yet,
so it serves the admin socket's `Status` and runs no order.

| exit status | meaning |
|---|---|
| 1 | the daemon could not start or serve for another reason (the state directory, the admin socket) |
| 2 | a usage error: an unknown flag or subcommand |
| 3 | the daemon config did not load |
| 4 | another daemon holds the state directory's lock |

hostkeeper is licensed under [Apache-2.0](LICENSE). How to contribute is in
[CONTRIBUTING.md](CONTRIBUTING.md), and how to report a vulnerability is in
[SECURITY.md](SECURITY.md). The rules every change to this repository is written and reviewed
against are in [AGENTS.md](AGENTS.md).
