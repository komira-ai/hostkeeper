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

**hostkeeper is pre-release.** This repository holds only the build, lint and coverage tooling so
far. There is no code to run and no release yet. The parts listed above are being moved here from
[kbf](https://github.com/komira-ai/kbf), where they are developed today.

hostkeeper is licensed under [Apache-2.0](LICENSE). How to contribute is in
[CONTRIBUTING.md](CONTRIBUTING.md), and how to report a vulnerability is in
[SECURITY.md](SECURITY.md). The rules every change to this repository is written and reviewed
against are in [AGENTS.md](AGENTS.md).
