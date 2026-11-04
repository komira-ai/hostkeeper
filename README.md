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

**hostkeeper is pre-release.** There is no program to run and no release yet. The parts listed
above are being moved here from [kbf](https://github.com/komira-ai/kbf), where they are developed
today. So far this repository holds the generic half of the host updater, moved from kbf
([MIGRATION.md](MIGRATION.md)), and the build, lint and coverage tooling:

| crate | what it holds |
|---|---|
| [`hostkeeper-hook`](crates/hostkeeper-hook) | the contract between the updater and a service's update hook, and the conformance harness a service tests its hook with (feature `conformance`) |
| [`hostkeeper-config`](crates/hostkeeper-config) | the strict loaders of the host's daemon config (`/etc/hostkeeper/hostkeeper.toml`) and of the `manifest.toml` a package carries, the owner and mode check of a trusted file, and the name and path grammars |
| [`hostkeeper-rights`](crates/hostkeeper-rights) | the transient unit that runs one step of a hook, and the part of the old declaration it is still built from |
| [`hostkeeper-steps`](crates/hostkeeper-steps) | the step runner: the step record and its resume, the dead-man, the staged-artifact check, the updater's own update |
| [`hostkeeper-proto`](crates/hostkeeper-proto) | the API definitions: the protobuf services of the coordinator (`hostkeeper.agent.v1`, `hostkeeper.operator.v1`) and of a host's root part (`hostkeeper.local.v1`), and the test that keeps them compatible |
| `hostkeeper-testkit` | helpers the tests share |
| `coverage-gate`, `xtask` | the 99% coverage gate and the CI steps |

The design is [docs/design/host-updates.md](docs/design/host-updates.md).

hostkeeper is licensed under [Apache-2.0](LICENSE). How to contribute is in
[CONTRIBUTING.md](CONTRIBUTING.md), and how to report a vulnerability is in
[SECURITY.md](SECURITY.md). The rules every change to this repository is written and reviewed
against are in [AGENTS.md](AGENTS.md).
