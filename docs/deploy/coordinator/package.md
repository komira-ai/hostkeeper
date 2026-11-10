# The coordinator from its deb or rpm

The coordinator as a systemd service on one Linux machine, installed from the release's
`hostkeeper-coordinator_<version>_<deb-arch>.deb` or `hostkeeper-coordinator-<version>-1.<rpm-arch>.rpm`
(verify them as [verify-a-release.md](../verify-a-release.md) says). Use this page, or the image of
[README.md](README.md) with compose, not both on one machine. The package's own files are in
`packaging/coordinator/`.

## What the package installs

| Path | What it is |
|---|---|
| `/usr/bin/hostkeeper-coordinator` | The coordinator, the static (musl) build, so one package runs on every release of its family |
| `/usr/lib/systemd/system/hostkeeper-coordinator.service` | The unit: `hostkeeper-coordinator serve --config /etc/hostkeeper-coordinator/serve.toml`, as the user `hostkeeper-coordinator` |
| `/usr/lib/sysusers.d/hostkeeper-coordinator.conf` | The user and group `hostkeeper-coordinator`, home `/var/lib/hostkeeper-coordinator`, no shell |
| `/usr/lib/tmpfiles.d/hostkeeper-coordinator.conf` | `/etc/hostkeeper-coordinator` (root:hostkeeper-coordinator 0750) and `/var/lib/hostkeeper-coordinator` (hostkeeper-coordinator 0700) |
| `/usr/share/hostkeeper-coordinator/serve.toml.template` | The template of the unit's flags |
| `/usr/share/hostkeeper-coordinator/ui/` | The operator web app, which `serve` serves by default |

The package depends on `systemd`. Its post-install script creates the user and the two directories
and has a running systemd read the unit again. It never enables or starts the unit, on an install
or an upgrade. Removing the package stops and disables the unit and keeps the user and both
directories, which hold the keys and the state.

## The flags: serve.toml

The unit takes every flag of `serve` from `/etc/hostkeeper-coordinator/serve.toml`. The
coordinator reads no environment variable. The file is TOML, and each key is a flag of `serve`
without its `--`; `hostkeeper-coordinator serve --help` lists them all.

| Value | Stands for | Example |
|---|---|---|
| a string or an integer | the flag and its value | `live-rollouts = 2` is `--live-rollouts 2` |
| `true` | a flag that takes no value | `ca-init = true` is `--ca-init` |
| `false` | nothing, the flag is left out | |
| an array of strings or integers | the flag once per element, in order | `server-name = ["a.example", "b.example"]` |

Any other value is refused at start, naming its key, with exit status 2: a float, a date, a table,
an empty array, or an array holding another kind. So is a key that is not a flag's name, and the
key `config`. The flags are put where `--config` stands and parsed with the command line, so every
other rule of the flags holds too:

- an unknown key is an unknown flag;
- each value is checked as the flag's own value;
- a flag in the file cannot also be given on the command line.

```sh
install --mode=0640 --owner=root --group=hostkeeper-coordinator \
  /usr/share/hostkeeper-coordinator/serve.toml.template /etc/hostkeeper-coordinator/serve.toml
```

Edit the listen addresses (one interface's address each, never a wildcard) and the store, then
place the files the flags name:

- **Owned by `hostkeeper-coordinator`, mode 0600:** the tokens file, the TLS key, and, for
  Postgres, the connection string of `database-dsn-file` (mode 0400 is also accepted). The
  coordinator refuses these files when they are readable by others.
- **Root's, readable by the group:** the certificate, `client-ca.pem`, and `oidc.toml`. There is no rollout file: `serve` refuses `--rollout-file`.
- **Writable by the coordinator:** the state files and the CA's key, under
  `/var/lib/hostkeeper-coordinator`.

[coordinator-state.md](../../operations/coordinator-state.md) lists each flag's refusals.

## Starting it

```sh
systemctl enable --now hostkeeper-coordinator.service
journalctl --unit hostkeeper-coordinator.service
```

A new release is installed with the package manager. The coordinator keeps running the old program
until it is restarted (`systemctl restart hostkeeper-coordinator.service`).

## From the image to the package

The image runs as uid 65532 and the package's user is assigned by `systemd-sysusers`. To move a
coordinator from compose to the package:

1. Stop the compose service.
2. Copy its volume's files into `/var/lib/hostkeeper-coordinator`.
3. Copy `etc/` into `/etc/hostkeeper-coordinator`.
4. Give every file that was 65532's to `hostkeeper-coordinator`, keeping its mode.
5. Write the compose `command:`'s flags into `serve.toml`.
