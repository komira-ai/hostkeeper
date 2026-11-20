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

Edit the listen addresses (one interface's address each, never a wildcard: the package runs on
the host's network, where `private-listen-wildcard` does not belong,
[A wildcard private listener](README.md#the-configuration-and-the-local-volume)) and the store, then
place the files the flags name:

- **Owned by `hostkeeper-coordinator`, mode 0600:** the tokens file, the TLS key, and, for
  Postgres, the connection string of `database-dsn-file` (mode 0400 is also accepted). The
  coordinator refuses these files when they are readable by others.
- **Root's, readable by the group:** the certificate, `client-ca.pem`, and `oidc.toml`. There is no rollout file: `serve` refuses `--rollout-file`.
- **Root's, readable by the group, public keys only:** the optional `operator-keys-file`, the
  operator's SSH public keys for each host group. Each host's daemon writes the keys of its group
  into the managed block of its login account's `authorized_keys`, at its first heartbeat after
  approval and after every change of the file. It holds no secret and no user name or password, so
  any regular file of at most 256 KiB is read. The coordinator reads it again on the reload tick
  (`tls-reload-secs`), so a change reaches the hosts with no restart; a file that is refused on that
  look leaves the keys in use. A host in a group the file does not list is sent no key, which takes
  the keys away from a group that is removed from it. Its shape is one table for each group that
  has keys, `keys = []` for a group that has none:

  ```toml
  [host-groups."build"]
  keys = ["ssh-ed25519 AAAA... ops laptop", "ssh-ed25519 AAAA... ci"]
  [host-groups."mac-build"]
  keys = []
  ```

  Each name is a host group name; a group holds at most 64 keys, each one line `<type> <base64>
  [comment]` of at most 4096 bytes, none twice. A key that breaks a rule refuses the start, naming
  the group and the key's place, never its text.
- **Root's, readable by the group, no secret:** the optional `host-prerequisites-file`, the host
  prerequisites of each host group (today only `xcode-command-line-tools`, which a Mac's daemon
  installs as root). It holds no secret, so any regular file of at most 64 KiB is read. The
  coordinator reads it again on the reload tick (`tls-reload-secs`), so a change reaches the hosts
  with no restart; a file that is refused on that look leaves the sets in use. A host in a group the
  file does not list is sent the empty set, which removes nothing from it. Its shape is one table
  for each group:

  ```toml
  [host-groups."mac-build"]
  prerequisites = ["xcode-command-line-tools"]
  ```

  Each table name is a host group name; a group holds at most 16 names, none twice, each one this release
  knows. A name that breaks a rule refuses the start, naming the group and the name's place.
- **Owned by `hostkeeper-coordinator`, mode 0600 or 0400, a secret:** the optional
  `console-bmc-credential-file`, the one user and password every BMC takes (the coordinator's
  console, [console-broker.md](../../design/console-broker.md)). The coordinator refuses it when
  others can read it. It is passed to `ipmitool` in the child's environment and is in no log or
  answer. Its shape:

  ```toml
  user = "ADMIN"
  password = "a long random password"
  ```

  The user is 1 to 16 bytes of printable ASCII with no space, and the password 1 to 20 bytes of
  printable ASCII, a space allowed; these are the most IPMI 2.0 takes.
- **Root's, readable by the group, no secret:** the optional `console-operators-file`, which token
  names may use the console, and the optional `console-targets-file`, an address that overrides
  for a BMC by its MAC (every other BMC is reached at the address its host's daemon reports, then
  at the one the coordinator's neighbour table holds: [console.md](../../operations/console.md#each-bmcs-address)).
  The operators file needs the credential file beside it (`serve` refuses it without); with no
  operators file nobody has console access and the other two are not read.
  The coordinator reads each again on the reload tick (`tls-reload-secs`); a file that is
  refused on that look leaves the one in use, and a file refused at the start stops the
  coordinator, naming its flag and never its path. The operators file names tokens of the tokens
  file; `power = false` allows opening a console but not power or boot-device calls:

  ```toml
  [[operator]]
  name = "alice"

  [[operator]]
  name = "bob"
  power = false
  ```

  The targets file, when given, gives the address of a BMC by the BMC MAC of its inventory record,
  an IP address or a name, with no port (the BMC's is 623), and of a Mac by one of its data MACs:

  ```toml
  [[target]]
  mac = "00:00:5e:00:53:02"
  address = "192.0.2.12"

  [[target]]
  mac = "00:00:5e:00:53:03"
  address = "bmc-03.lab.example"
  ```

  The coordinator runs `ipmitool`, which must be installed at `console-ipmitool` (by default
  `/usr/bin/ipmitool`); the package does not carry it. `console-ipmi-cipher` (3),
  `console-ipmi-privilege` (`administrator`), `console-ipmi-timeout-s` (1), `console-ipmi-retries`
  (2), `console-attach-s` (30), `console-max-sessions` (8), `console-idle-s` (1800, a stream that
  carries no byte either way for that long closes), `console-total-s` (28800, the longest a stream
  stays attached), `console-connect-timeout-s` (5, the dial of a Mac's Screen Sharing on TCP
  5900) and `console-quiet-s` (10, how long the Console page waits for a serial console's first
  byte before it says that no output came yet) have defaults and need no key. A console stream is the WebSocket
  `GET /console/v1/stream/<session>` on the private listener, which `Console/Open` hands out.
- **Writable by the coordinator:** the state files and the CA's key, under
  `/var/lib/hostkeeper-coordinator`.

[coordinator-state.md](../../operations/coordinator-state.md) lists each flag's refusals.

Two keys bound how the coordinator reads a repository when an operator adds one
([package-repositories.md](../package-repositories.md#hostkeeper-repository-format-version-1)):
`repository-discover-secs` (20, at least 1) is the time each of the two reads, the descriptor and
the key, may take, and `repository-descriptor-max-bytes` (65536, 1 to 65536) is the most bytes of a descriptor.
No repository is built into the program: the organisation's `komira` is added by an operator like any other.

`xcode-feed-url` (empty: off) is the `https://` address of a list of Xcode releases, the Xcode Releases
site's `https://xcodereleases.com/data.json`, which the coordinator reads once a day to tell that a release
newer than any its Xcode store holds exists ([xcode-releases.md](../../operations/xcode-releases.md)); a URL
that is not `https://`, or names no file, is refused at start. The coordinator reads no third party unless the key is set.

`xcode-dir` (absent: no store) is a directory, which must exist, where the coordinator keeps the Xcode
archives an operator pushes to it, exactly as Apple ships them, for its own Macs to read; the coordinator
does not make it and does not start when it is missing or is not a directory. It must be writable by the
unit: under `/var/lib/hostkeeper-coordinator`, which the unit may write, as in the template's example. With
several coordinators, every instance mounts the same directory. `xcode-max-bytes` (21474836480, 20 GiB; at
least 1) is the most bytes of one archive ([xcode-releases.md](../../operations/xcode-releases.md)).

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
