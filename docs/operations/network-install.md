# Installing a Linux machine from the network

How a new Linux server gets Ubuntu 24.04 and the hostkeeper daemon from the farm LAN, with one
manual step of its own: enter its record, which is also its approval
([enrollment.md](enrollment.md#the-record-is-the-approval)). A second step, once, is needed on a
board that does not try network boot before its disk. The design, and the
reason for every rule below, is [network-install.md](../design/network-install.md) (cited here as
"the design"). Enrolling the machine once it has posted its request is
[enrollment.md](enrollment.md), and every `hkctl` command and flag is
[cli.md](../cli.md).

The parts:

- `pxe`, the boot server, a `.deb` of its own (`/usr/lib/pxe/pxe` and the unit `pxe.service`; the
  hostkeeper deb and rpm carry no boot server). The daemon installs it with `apt` on the boot host
  when you move the host into a group that lists the service, and removes it when you move the host out of it. `pxe serve`
  (called `serve` below) serves a release's boot files. `pxe update-hook prepare` is run by the daemon when it
  updates the package, from the staged copy and before `apt` installs anything: it downloads and
  verifies the release's boot files into a new directory of `/var/lib/pxe` and seals it. You type neither: the daemon and the unit run them,
  with the settings you write in `/etc/pxe/pxe.toml`.
- The farm's router. Its DHCP gives every address. In router mode it also names the boot server.
- The coordinator, which `serve` reads before every answer to learn which machines may be
  installed, and which the installed daemon enrolls with over its LAN listener.

The commands of `hkctl` below reach the coordinator's private listener, as in
[enrollment.md](enrollment.md), which also says where `hkctl` comes from:
`https://<private-listener>` is its address, `<ca.pem>` the CA its certificate chains to, and
`<token-file>` a file holding an operator token. The examples use the documentation addresses
`192.0.2.0/24` for the farm LAN and `00:00:5e:00:53:xx` for MACs; put your own in their place.

**The record's platform.** Router mode chooses the shim and the boot set by the record's platform,
since the machine's own packets may state none, so every record of a machine to install in router
mode names it: `hkctl enrollment add-record --platform linux_x86_64` or
`--platform linux_arm64` ([enrollment.md](enrollment.md#record-the-server)). A record with no
platform is still read, and `serve` logs `has no platform, which router mode needs` for its MACs:
router mode installs nothing for it, and it gets the menu that boots its local disk. Proxy mode
takes the platform from the firmware's stated architecture and does not need the field. Facts the design marks
**[V]** (not yet checked on the real router or boards) are marked the same way here, with the
check that settles each; record what you find in [What has been checked](#what-has-been-checked).

## Which machines are installed

`serve` installs a machine only when one of its data MACs is in the answer set, which it makes
from two reads of the coordinator taken for that answer: `Enrollment.ListRecords` and
`Enrollment.List`. A data MAC of a record is in the set when:

- no other record holds the same MAC;
- the machine has not fetched its seed earlier in this run of `serve` (a MAC leaves the set when
  its install starts);
- the record is not `enrolled`, no pending request matches it, and its `hostHeld` is
  `HOST_HELD_NONE`, unless `reinstall` names its host id ([Reinstall](#reinstall-a-known-host));
- the record's `state` is `RECORD_STATE_OPEN`: its approval window is open and its one use not
  spent, so a machine whose window is over, or whose record is used, is not installed (open the
  record again with `enrollment open-record`, enrollment.md), unless `reinstall` names its host
  id;
- in router mode, the record has a platform (above);
- in proxy mode, a record that has a platform agrees with the architecture the firmware or GRUB
  states, else the machine is refused; and the sealed directory has a boot set for the platform.

**`hostHeld` is the rule; `Hosts.List` is not.** Each record of `list-records` says what the
coordinator's durable enrollment store holds for its host id, read from the store at each call.
Only `HOST_HELD_NONE` (no host by that id) is installed. `HOST_HELD_APPROVED` (a host is approved
as that id), `HOST_HELD_DENIED` (retired with `hosts retire`, not final yet), `HOST_HELD_RETIRED`
(retired, final) and `HOST_HELD_UNSPECIFIED` (a coordinator of a release before the field, or one
without enrollment) are not. `hosts list` is not what decides: it lists the hosts heard from
since the coordinator started and the enrolled hosts that are not denied, so a retired host is
missing from it after a restart, though its record is held. A retired host's record
can stay in the inventory: its `hostHeld` keeps the machine from being installed again. To see
what `serve` will see:

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list-records --enrolled false
```

A record that will be installed shows `"hostHeld": "HOST_HELD_NONE"` and no `enrolled` (the
gateway's JSON leaves out a field at its default), and no pending request of `enrollment list`
names its host id as `record_host_id` or among `candidate_host_ids`.

**A failed read installs nothing.** When either read fails (the private listener does not answer,
the token is refused, an answer does not parse), the answer set of that answer is empty: no proxy
offer in proxy mode, the local-disk menu in router mode. `serve` logs `the coordinator could not
be read (<why>): this answer is made from the empty answer set`, and the machine's next network
boot reads again.

## Once per farm

### 1. Choose the mode

| | Router mode (`mode = "router"`) | Proxy mode (`mode = "proxy"`) |
|---|---|---|
| For | a farm LAN whose router you set, such as the new lab's UniFi router | a LAN whose router you leave as it is |
| The router | names `pxe`'s address and the file `hostkeeper/boot.efi` for network-boot clients | gives addresses and names no boot file |
| `pxe` | sends no DHCP packet; serves every network-boot client the shim and a menu made for its MAC | sends a second DHCP offer, with no address, only to a MAC of the answer set |
| A machine not to be installed | gets the shim and the menu that boots its local disk | gets no answer from `pxe`, and its firmware goes on to its next boot device |
| DHCP guarding | needs no change | drops the proxy offer unless the `pxe` host is trusted ([step 6](#6-set-the-router)) |
| UEFI HTTP boot | not served | served |
| The record's platform | required: it chooses the shim and the boot set | optional; if set, must agree with the firmware's architecture |

`mode` has no default: the wrong mode answers nothing, or its answers are dropped.

### 2. Give the coordinator a LAN listener

A bare machine has no tailnet, so the installed daemon enrolls and heartbeats over the
coordinator's LAN listener: start the coordinator with `--lan-listen <address:port>`, the
address of its own farm-LAN interface (a wildcard address is refused), beside
`--enrollment-dir` ([enrollment-coordinator.md](../enrollment-coordinator.md)). Its start line
says `lan=<address:port>`, and its server certificate names that address. The listener serves
the host's calls only, never the operator API or the UI.

### 3. Write the daemon config of the installed hosts

Every installed host gets the same `/etc/hostkeeper/hostkeeper.toml`, which `serve` writes into
each seed. Write it as in [enrollment.md](enrollment.md#bootstrap-a-linux-node), from
`/usr/share/hostkeeper/hostkeeper.toml.template`, with `coordinator` set to the LAN listener:
`"https://192.0.2.5:8443"` for `--lan-listen 192.0.2.5:8443`. It holds no secret, and must not
gain one: every machine of the answer set fetches it over plain HTTP. Make it root's and closed to
writes by group and others (`chown root:root`, `chmod 0600`): `prepare` and `serve` load it with the
daemon's own loader and refuse what the daemon would refuse, before a seed is made. The boot host
keeps a copy as `/etc/pxe/hostkeeper.toml` (step 5), a file of its own beside the host's
own `/etc/hostkeeper/hostkeeper.toml`.

### 3a. Write the repository entries of the installed hosts

Optional, and needed only when an installed host must install from the package repository before
it reaches the coordinator: every other host gets its entries from the coordinator's registry at
its first heartbeat (packages-repositories.md section 3.2). With `repositories = "<dir>"` in
`[serve]` (step 5), every seed also writes the files of `<dir>` to `/etc/hostkeeper/repositories/`
on the installed host, before its `hostkeeper.toml`, so the daemon starts with its repository
entries (section 3.1); the registry's set adopts them at the first heartbeat when they are the same
bytes, and replaces them otherwise. `<dir>` holds
one `<name>.sources` file per entry and the keyrings they name, and nothing else. Each stanza is
exactly `Types: deb`, one `URIs:` of `https://` or `file:/`, one `Suites:`, `Components:`, and
`Signed-By: /etc/hostkeeper/repositories/<keyring>`: the path the file will have on the host, not
the path in `<dir>`. For the project's channels, `komira-testing.sources` and
`komira-stable.sources` read:

```
Types: deb
URIs: https://packages.komira.ai/apt
Suites: testing
Components: main
Signed-By: /etc/hostkeeper/repositories/komira.gpg
```

(`Suites: stable` for `komira-stable.sources`, and `Suites: testing-fail` for
`komira-testing-fail.sources`, the entry of hello's failing build on the hosts that run its proof,
[hello.md](../hello.md) step 4), the bytes the registry's repository `komira` is written as, and
the keyring `komira.gpg` is fetched from
`https://packages.komira.ai/apt/komira-archive-keyring.gpg` (see
[package-repositories.md](../deploy/package-repositories.md)). Make the directory and its files
root's, with no write for group or others (`chown -R root:root`, `chmod 0755` the directory,
`chmod 0644` the files); each keyring is at most 1 MiB. `serve` checks the directory at its start
with the daemon's own loader, as if it were at `/etc/hostkeeper/repositories/`, and refuses to
start on what the daemon would refuse (a `Trusted: yes`, a `Signed-By` naming `<dir>`, a file no
entry names, a link, a keyring over 1 MiB, or armored without a `.asc` name), naming the file;
on a directory that is missing or holds no entry; and on a file name a seed cannot carry (anything
but `A-Za-z0-9._+-`, or a name starting with `.`). Check
the loader's decision before the install window, not on the first machine:
`hostkeeper check-repositories --dir <dir>`, run as root, judges `<dir>` the same way.

### 4. Choose the boot host, and its token

The boot host is a host that runs the hostkeeper daemon (the daemon installs and removes `pxe`),
has a NIC on the farm LAN (in both modes `serve` reads the DHCP broadcasts there) and reaches the
coordinator's private listener. It can be the coordinator's own host, given a NIC on the farm LAN:
that host already holds every operator secret, so no credential moves. It may be another
farm-LAN host that reaches the private listener, at the cost of a copy of an operator token there.
The host needs the pinned `gh` and the Sigstore trusted root that `prepare` checks attestations
with; hostkeeper's deb on any daemon host provides both (`/usr/lib/hostkeeper/gh` and
`trusted_root.json`). The daemon installs `pxe` from the package repository, so the host needs hostkeeper's
repository entry for the suite that holds the release (`/etc/hostkeeper/repositories/komira-testing.sources`,
which the daemon writes from the coordinator's registry entry `komira`:
[repository-keys.md](repository-keys.md#put-the-repository-in-the-registry)). The release's `pxe`
`.deb` is published to that suite with the release's other packages
([package-repositories.md](../deploy/package-repositories.md)).

Put an operator token in a file of root's with mode 0600 or 0400, such as `/etc/pxe/token`.
`serve` opens it as root, drops to `user-id` and `group-id` once its sockets are bound, and only
then reads it. Settings name the file, never hold the token.

The farm LAN must be free of an active attacker while `serve` runs: nothing on the new machine
checks the initrd, the ISO or the seed (the design, sections 5 and 6, and its Question 2). Keep
the boot host in `pxe-boot` only for the install window ([step 8](#8-run-an-install-window)).

### 5. Write the settings

On the boot host, as root, write `/etc/pxe/pxe.toml`, owned by root and closed to writes by group
and others (`chown root:root`, `chmod 0600`). Both `serve` and `prepare` read it with the
daemon's own trusted-file rule and refuse a file that is not root's or that others can write. It
holds paths and choices, never the token. Copy the installed hosts' daemon config of step 3 to
`/etc/pxe/hostkeeper.toml` (`daemon-config` below), and put the CA of the private listener at
`/etc/pxe/ca.pem`, the SSH public keys at `/etc/pxe/operator-ssh-keys`, and the token at
`/etc/pxe/token`.

Proxy mode:

```toml
# /etc/pxe/pxe.toml
[serve]
mode = "proxy"
address = "192.0.2.10"
interface = "eth1"
daemon-config = "/etc/pxe/hostkeeper.toml"
ssh-authorized-keys = "/etc/pxe/operator-ssh-keys"
coordinator = "https://192.0.2.6:8443"
ca-file = "/etc/pxe/ca.pem"
token-file = "/etc/pxe/token"
user-id = 65534
group-id = 65534
```

Router mode is the same with `mode = "router"`; this one also sets the install disk, the
serial console and the repository entries of step 3a:

```toml
# /etc/pxe/pxe.toml
[serve]
mode = "router"
address = "192.0.2.10"
interface = "eth1"
daemon-config = "/etc/pxe/hostkeeper.toml"
ssh-authorized-keys = "/etc/pxe/operator-ssh-keys"
coordinator = "https://192.0.2.6:8443"
ca-file = "/etc/pxe/ca.pem"
token-file = "/etc/pxe/token"
user-id = 65534
group-id = 65534
install-disk = "serial=SN-*"
serial-console = "ttyS1"
repositories = "/etc/pxe/repositories"
```

The file has two tables and nothing else. Every key of `[serve]` is a flag of `pxe serve` without
its `--`; a string or an integer is the flag's value, and an array gives the flag once per element
(`reinstall = ["web-17", "web-18"]`). Any other kind of value, an unknown key, a missing required
key or a value the flag refuses stops `serve` and `prepare` with one line naming it. `dir`,
`settings` and `notify-systemd` are not keys: the unit gives them. The keys:

- `mode`: `"proxy"` or `"router"` (step 1). Required, no default.
- `address`: the host's IPv4 address on the farm LAN, which its answers, menus and seeds name.
  Required. `interface`: the farm-LAN NIC; with it, every socket takes only that interface's
  packets.
- `daemon-config`: the installed hosts' daemon config of step 3. Required.
- `ssh-authorized-keys`: a file of SSH public keys for the installed system's one local
  account, `operator`, whose password is locked and who has sudo without a password. A file with
  a private key block, or with no key, is refused. Required.
- `coordinator`, `ca-file`, `token-file`: the coordinator's private listener as an `https://` address with its port, the CA
  its certificate chains to, and the token file of step 4. All required.
- `user-id`, `group-id`: the unprivileged user and group to drop to, as numbers (on Ubuntu,
  65534 is `nobody` and `nogroup`; a user made for it does as well). Required.
- `install-disk`: the disk the installer writes, `serial=<glob>`, `model=<glob>` or
  `path=<glob>`, the smallest of those it matches. Without it, the smallest SSD (a disk the
  kernel reports as not rotational). Only that disk is written. See the disks of the board check.
- `serial-console`: `ttyS<n>` or `ttyAMA<n>`: the installed system's kernel console and a getty
  on that port at 115200 baud, so the BMC's serial console shows the key hash. Which port the
  BMC's serial console is depends on the board model **[V]** (the board check records it).
- `dhcp-listen`, `proxy-listen`, `tftp-listen`, `http-listen`: `address:port` of each socket; by
  default the protocol ports 67, 4011, 69 and 80 on every address. Keep the HTTP port at 80: a
  port other than 80 is written into GRUB's device name, which Ubuntu's signed GRUB is not known
  to read **[V]**. `dhcp-reply-to`: where a proxy offer goes, `255.255.255.255:68` by default.
- `tftp-timeout-ms`: how long a TFTP transfer waits for an ACK before it sends again, 1000 by
  default.
- `repositories`: the directory of step 3a, optional; without it a seed writes no repository
  entry.
- `reinstall`: host ids to reinstall ([Reinstall](#reinstall-a-known-host)).

`[prepare]` is optional and sets what `prepare` downloads and checks with. A farm of one
architecture names its own and keeps no ISO it never serves; without `platform` both platforms are
fetched:

```toml
# /etc/pxe/pxe.toml
[serve]
mode = "proxy"
address = "192.0.2.10"
interface = "eth1"
daemon-config = "/etc/pxe/hostkeeper.toml"
ssh-authorized-keys = "/etc/pxe/operator-ssh-keys"
coordinator = "https://192.0.2.6:8443"
ca-file = "/etc/pxe/ca.pem"
token-file = "/etc/pxe/token"
user-id = 65534
group-id = 65534

[prepare]
platform = ["linux-x86_64"]
```

The keys of `[prepare]`: `platform` (`"linux-x86_64"`, `"linux-arm64"`; an array), `ca-bundle` (the
PEM roots the downloads are checked against, `/etc/ssl/certs/ca-certificates.crt` by default),
`gh` (`/usr/lib/hostkeeper/gh`) and `trusted-root` (`/usr/lib/hostkeeper/trusted_root.json`), the
pinned `gh` and Sigstore trusted root the hostkeeper deb installs, and `download-timeout-secs`
(14400, the bound on each download). `prepare` also checks the whole `[serve]` table, so a
settings file that `serve` would refuse fails the install of the package, not the first boot.

A changed file takes effect at the next restart of `pxe.service` for `[serve]` (`serve` reads it
once, at its start), and at the next `prepare` for both tables; edit it, then
`systemctl restart pxe.service`. A file that `serve` refuses leaves the unit failed, with the
reason in its journal ([When something goes wrong](#when-something-goes-wrong)).

### 6. Set the router

Only the person who holds the router's settings can make these checks and changes.

**Router mode, on UniFi.** The network's DHCP settings have a "Network Boot" option that takes a
server address and a file name **[V]**: where it is, whether it fills the DHCP `siaddr` and `file`
fields or options 66 and 67, and whether it applies to every client of the network. Set the
server to the `pxe` host's farm-LAN address and the file to `hostkeeper/boot.efi`,
and nothing else. DHCP guarding stays as it is: router mode sends no DHCP packet for it to drop.
While the setting is on, every network-boot client of the network loads the shim from
`pxe`, and a firmware whose network boot comes first waits for a TFTP answer at every
boot while `serve` is not running **[V]**: how long each board model waits (the board check
records it). Where that wait matters, turn the setting on for the install window only.

The setting works when a machine's network boot shows, in `serve`'s log, its DHCPREQUEST
(`dhcp: DHCPREQUEST from <mac>, ... on the DHCP port: it takes <address>`) and then
`tftp <address>:<port>: hostkeeper/boot.efi is hostkeeper/shimx64.efi for <mac> (linux-x86_64)`
(or `shimaa64.efi`, `linux-arm64`). A machine that goes on to its disk with neither line names
another server or no file: check the setting.

**Proxy mode: the router names no boot file** (the design's Question 4). The router must offer an
address and no boot file name: no option 66 or 67 and an empty `file` field. A router that names
one of its own leaves the firmware a choice between two boot files, and what it chooses depends
on the firmware. Capture the router's answer to a network-boot client on the `pxe`
host, as root, while a machine boots from the network:

```
tcpdump -n -vvv -i <farm-lan-nic> -w <router-check.pcap> udp port 67 or udp port 68
```

Read it back with `tcpdump -n -vvv -r <router-check.pcap>` and look at the router's DHCPOFFER and
DHCPACK to that MAC. If either names a boot file, switch the router's boot file off once; it is
not worked around in `pxe`. A next-server address with no file name is expected to be
harmless **[V]**. On UniFi, that is the "Network Boot" setting of router mode, off.

**Proxy mode: DHCP guarding.** A UniFi network can drop DHCP server messages from every address
but its trusted DHCP servers ("DHCP Guarding") **[V]**: its name, where it is set, and whether
it is on by default. A proxy offer is a DHCP server message, so with guarding on and the
`pxe` host not trusted, the offer is dropped and the machine boots its disk as if
`serve` were not running. Either add the `pxe` host's farm-LAN address to the trusted
DHCP servers, or use router mode. The offer is dropped when `serve`'s log shows `proxy offer sent`
for a MAC and no `tftp` line follows from that machine.

**Both modes: the broadcasts reach `pxe`.** Each mode reads the firmware's DHCP
broadcasts on the farm LAN (in router mode, to learn which address took which MAC). That the
switches pass client broadcasts to the `pxe` host with guarding on is **[V]**: it is
true when `serve` logs a `dhcp:` line for a booting machine's MAC.

### 7. Roll out `pxe`: a host group and the desired version

Once, on the coordinator. `pxe` runs on the hosts of a host group that lists it
([hostgroups.md](hostgroups.md)). Make a group for the boot host, `pxe-boot`, that lists every
package the boot host runs now (`hkctl host-groups get` of its group shows them; a move uninstalls
what the new group does not list) and `pxe` after them, in the order they install:

```json
{ "hostGroup": { "name": "pxe-boot", "packages": ["hostkeeper", "pxe"] } }
```

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups put --request pxe-boot.json --reason "the boot server's host"
```

Each release carries `pxe_<version>_amd64.deb` and `pxe_<version>_arm64.deb` with their
attestations, and publishes them to the `testing` suite of the project's apt repository, as it
does hello's and mdm's. Nothing sets a desired version for `pxe`: you set it to the release whose
boot files you install, `<version>` (the package of that version fetches the boot files of the
same release). `cargo xtask release targets --service pxe`, run in a checkout of hostkeeper on a
directory of the release's assets, writes the targets, as for hello
([hello.md](../hello.md) step 2):

```sh
cargo xtask release targets --service pxe --version <version> --dir <assets> \
  --repo komira-ai/hostkeeper \
  | sed -n 's/^targets=//p' > targets.json
jq -n --arg version <version> --slurpfile targets targets.json \
  '{desired: {version: $version}, targets: $targets[0]}' > desired.json
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    releases set-desired --service pxe --request desired.json
```

The targets name the `.deb` of each Linux platform from the repository entry `komira-testing`; the
coordinator refuses the set (`repository_unknown`) while its registry lacks `komira`. A new
release is a new desired version; a host in a group that lists `pxe` then updates it, and a host
that is not is ordered nothing.

### 8. Run an install window

Move the boot host into `pxe-boot`. The daemon installs the package at its next heartbeat
after the move has admitted it ([hostgroups.md](hostgroups.md#moving-a-host)):

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id <boot-host> --host-group pxe-boot --reason "install window"
```

The daemon stages the `.deb` and seals its copy of the files, then runs the hook of the package,
`/usr/lib/pxe/pxe update-hook <step>` from that sealed copy, as root. The order is: stage; the
hook's **prepare** (from the staged tree, before `apt` installs anything); the installer's
activate (`apt install` of the `.deb`, which carries `/usr/lib/pxe/pxe` and the unit and no
maintainer script, and the unit enabled); **cutover**; **verify**; then **cleanup**:

- **prepare** reads `/etc/pxe/pxe.toml` and the daemon config, and downloads the release's boot
  files, checking the attestation of `netboot.json` and of each deb against the `[trusted_signers]`
  of `daemon-config`, every size and SHA-256 against the pins, and writing `seal.json` last. It
  seals them into `/var/lib/pxe/<version>`, readable by everyone (directories 0755, files 0644) so
  the unprivileged user `serve` drops to can read them. It can take long (several GiB of ISO) and
  has no time limit of its own; each download is bounded. Nothing a client sees changes. When
  the directory of that version is already sealed, `prepare` keeps it.
- **cutover** points `/var/lib/pxe/current` at that version and restarts `pxe.service`. The unit
  reports itself started only after `serve` has bound its sockets, dropped to the user, checked
  the seal and read the coordinator once.
- **verify** asks systemd whether `pxe.service` is active, and asks the daemon to try again until it is.
- **cleanup** removes every other version directory of `/var/lib/pxe`.

When a step refuses, the update fails and the version that was serving
keeps serving; on the first install nothing is left running and `/var/lib/pxe` holds no new
directory. The reason is in the host's `Last update` on the Host page and in the journal. The
usual ones: `bad_settings` (the settings file or the daemon config does not load, naming why),
`boot_set_refused` (an attestation, size or digest the fetch refused, with its text), `not_prepared`,
`not_started` (the restart failed: read the journal of `pxe.service`) and `locked` (another step
holds `/var/lib/pxe`). Fix the cause, then wait for the daemon's next try, or move the host out of `pxe-boot` and back.

**See it serving.** `journalctl --unit pxe.service` shows each line `serve` logs, as
`pxe serve: <unix seconds> <text>` (the journal adds its own time and host first; `--output cat`
leaves it out). A start logs:

- `started in proxy mode as uid 65534 gid 65534, answering as 192.0.2.10: the boot sets of
  hostkeeper <version> for linux-x86_64 and linux-arm64; DHCP 0.0.0.0:67, proxyDHCP
  0.0.0.0:4011, TFTP 0.0.0.0:69, HTTP 0.0.0.0:80` (`proxyDHCP not bound` in router mode);
- `the coordinator holds <n> records and <n> pending requests; <n> MACs are in the answer set`.

Check that number before a machine boots: it counts the data MACs `serve` would install now. In
router mode it counts only the records that have a platform. `serve` refuses to start, with one
line `pxe serve: <why>` in the journal, when a file of its settings does not load, a socket cannot
be bound, the sealed directory differs from its seal (naming the file), the coordinator cannot
be read, or a `reinstall` names no record; systemd then restarts it every five seconds until
you fix the cause.

`serve` keeps nothing but what it learned this run. A restart (`systemctl restart pxe.service`)
starts with nothing learned, and after a settings change it takes the new settings. For the
measurement of a first machine, keep the journal:
`journalctl --unit pxe.service --output cat > serve.log`.

**After the window,** when the window's machines are approved, move the host back into its own
group, which does not list `pxe`: the move orders the uninstall of `pxe` (an uninstall intent,
[hostgroups.md](hostgroups.md#uninstalls)):

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    host-groups move --node-id <boot-host> --host-group <its-own-group> --reason "window over"
```

The daemon stops and removes `pxe.service`, removes `/var/lib/pxe` (every boot set) and the
package. `/etc/pxe` stays: it is yours, and the next window reuses it. In router mode, turn the
router's Network Boot off again if the wait of a network boot with no server matters on the farm's
boards. A host left in `pxe-boot` keeps `pxe` installed and updated with every release.

## Once per board model: the board check

Before the first machine of a board model is installed, set its firmware and check how it boots.
Nothing in hostkeeper changes a firmware, BMC or router setting.

### Firmware settings

Over the BMC's remote console, in the firmware setup, the board needs:

- **UEFI boot.** Legacy BIOS boot is not installed: a firmware that asks as BIOS (option 93 value
  0) gets no proxy offer, and cannot run `hostkeeper/boot.efi`.
- **The UEFI network stack, with IPv4 PXE, on the NIC on the farm LAN.** That NIC must be the one
  the firmware boots from. A NIC on the storage network only cannot be installed: it has no DHCP.
- **Network boot of that NIC before the disk** in the boot order. This is the expected setup; on
  a board that does not have it, someone picks network boot in its boot menu once per machine.
- **Secure Boot as it is.** The boot chain is Ubuntu's shim, GRUB and kernel, signed through the
  Microsoft UEFI CA, so the install is expected to work with Secure Boot on or off **[V]**:
  whether the board trusts that CA by default, or ships with Secure Boot off.

On a Supermicro board, in its AMI setup utility, these are usually: Boot, Boot Mode
Select, `UEFI`; Advanced, Network Configuration (or Network Stack Configuration), Network Stack
`Enabled` and IPv4 PXE Support `Enabled`; Boot, Boot Option #1, the UEFI PXE IPv4 entry of the
farm-LAN port. The menu names differ by board and firmware version and are **[V]** on each board
model: record the names you find. HTTP boot (IPv4 HTTP Support) is not needed; whether a board's
HTTP boot accepts a proxy offer is **[V]**, and its UEFI PXE boot is used either way.

### What to check

Run the check on a machine of the model that has no inventory record, so that nothing is
installed: proxy mode gives it no answer, and router mode gives it the local-disk menu. Run an install window in either mode ([step 8](#8-run-an-install-window)), and the capture of [step 6](#6-set-the-router) to
keep the packets with the result. Power the machine on and read `serve`'s log for its MAC.

1. **Option 93, the firmware's architecture.** The DISCOVER and REQUEST lines say it:
   `dhcp: DHCPDISCOVER from <mac>, architecture 7, PXEClient, on the DHCP port: ...`. 7, 9 and 16
   are installed as x86-64, 11 and 19 as arm64; `architecture none` means the packet has no
   option 93; any other value is a platform that is not installed. x86-64 firmware commonly sends
   7 and arm64 firmware 11 **[V]**. Record the value of the DISCOVER and of the REQUEST (the
   REQUEST line appears in router mode as `it takes <address>`). In router mode a client whose
   packets carry no option 93 is served the shim of the platform of its record.
2. **Network boot comes first.** A DISCOVER at power-on, before the disk boots, says it does.
3. **No answer, next boot device.** In proxy mode the log says `no answer: not in the answer set:
   no record holds it`, and the machine must go on to its disk (or its next boot option) by
   itself. Record how long it waits.
4. **The local-disk menu (router mode).** The log shows `hostkeeper/boot.efi is ...`, the GRUB of
   the same directory, `grub/grub.cfg`, and `menu grub/hostkeeper/<cpu>/<mac>: local disk: ...`.
   The menu's one entry is GRUB's `exit`, which hands control back to the firmware to try its next
   boot option **[V]**. Record whether the board then boots its disk or stops at its own boot
   menu. A board that stops cannot be used in router mode until the menu generator chainloads the
   disk's boot loader for it (the design, section 4); record it and use proxy mode on it.
5. **The wait with no server (router mode).** Move the host out of `pxe-boot` (or `systemctl stop pxe.service`), leave the router's Network Boot on,
   and boot: record how long the firmware waits for TFTP before it goes on, and that it does.
6. **The disks.** List the board's disks, from the firmware's or the BMC's storage inventory, or
   with `lsblk -d -o NAME,SIZE,ROTA,RM,TRAN,MODEL,SERIAL` on a running system of the same model.
   The default `install-disk` takes the smallest non-rotational disk, and the installer's match
   has no key that leaves out a removable one: if a removable disk reports itself not rotational
   and is the smallest, the default would take it. On such a board, and on any board whose boot
   disk is not the smallest SSD, pass `install-disk = "serial=<glob>"` or `model=<glob>`.
7. **Memory.** The live system loads the whole ISO into memory, so a machine needs several GiB
   of RAM **[V]**: record the board's RAM and whether the install succeeded.
8. **The serial console.** Which `ttyS<n>` the BMC's serial console reads, for
   `serial-console`.
9. **After the first install of the model,** whether the board keeps the boot order curtin
   writes. Curtin puts the installed system first in the UEFI boot order after an install
   (`reorder_uefi`, at its default) **[V]**; on the installed host, `sudo efibootmgr` shows the
   order. Then, with `pxe.service` running, reboot the host: a DISCOVER from its MAC means network boot
   still comes first. Either way it is not installed again (it is enrolled, or pending). This
   tells you whether a reinstall needs a one-time network boot.

## Each machine

Before the first machine, check `serve`'s start line and the answer set's count ([step 8](#8-run-an-install-window)).

**Step 1. Record the machine.** From the chassis label: its data MACs, its BMC MAC and its
serial, as in [enrollment.md](enrollment.md#record-the-server). Record the MAC of the farm-LAN
NIC the firmware boots from among the data MACs. Give the record its host id and its host group
(one the coordinator holds, [hostgroups.md](hostgroups.md)): the record is the approval, so the machine that matches it, within its window and once, is enrolled
with no further step ([enrollment.md](enrollment.md#the-record-is-the-approval)). The window is
24 hours unless you give `--approve-for-secs` or `--approve-until-unix`; enter the record shortly
before the machine boots.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment add-record --host-id web-17 --host-group web \
    --data-macs 00:00:5e:00:53:01 --data-macs 00:00:5e:00:53:02 \
    --bmc-mac 00:00:5e:00:53:a1 --serial SN-0042 --platform linux_x86_64 \
    --approve-for-secs 43200 \
    --notes "rack 4, unit 17"
```

The answer shows `hostHeld` `HOST_HELD_NONE` for a host id no host holds. If it shows anything
else, that id is held (approved, or retired): this machine is not installed under it. Pick
another host id, or, to rebuild that host, see [Reinstall](#reinstall-a-known-host).

Then rack the machine and power it on, as every machine is. With `serve` running, its log shows,
in proxy mode:

- `dhcp: DHCPDISCOVER from 00:00:5e:00:53:01, architecture 7, PXEClient, on the DHCP port: proxy
  offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)`, then the proxy ACK on the
  proxyDHCP port;
- `tftp <address>:<port>: sent hostkeeper/shimx64.efi (<n> bytes)`, the GRUB beside it, and
  `grub/grub.cfg`;
- `menu grub/hostkeeper/x86_64/00:00:5e:00:53:01: install linux-x86_64 as host web-17`;
- the kernel, initrd and ISO by HTTP (`http <peer>: GET /vmlinuz-linux-x86_64: 200`, ...);
- `seed seed/00:00:5e:00:53:01/user-data: fetched by 00:00:5e:00:53:01, host web-17: its install
  started, and it leaves the answer set for the rest of the run`;
- the deb, `http <peer>: GET /deb/hostkeeper_amd64.deb: 200`.

The installer writes the disk, installs the deb and the daemon config, and reboots. The reboot
gets no install: its MAC left the answer set at its seed fetch, and from then on its pending
request keeps it out, after a restart of `serve` too. Its daemon makes its key, posts its
request, and its console shows the key hash.

**Step 2. Check that it enrolled.** The coordinator approves the request itself, as `web-17` in
the record's host group, and the daemon takes its certificate at its next poll. No approve is run. The
daemon logs `enrolled as host:web-17`, and the record is used: `list-records` shows it
`RECORD_STATE_USED`, with the request that used it.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list-records --limit 50
```

A MAC is not secret: anyone on the LAN can post the recorded MACs. The record, its one use and its
window are what keep the wrong machine out, and after enrollment the host proves itself by its
key. Compare the key hash on the machine's console with the used record's `used_by_request_id`
(the request's key hash) afterwards. A request the record does not approve waits in
`enrollment list`: one that matches no record or several, arrived outside the window (open the
record again with `enrollment open-record`), matches a used record, claims no BMC MAC or another
than the record's, or is contested by another request. Approve those by hand, by the key hash on
the console, as in [enrollment.md](enrollment.md#approve):

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list --limit 50
```

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
    --host-group web --reason "network install, key checked on the console"
```

A request that matches a record another request also matches is `contested` and cannot be
approved until the other is rejected ([enrollment.md](enrollment.md#the-pending-request-and-its-match)).
The coordinator then orders the packages of the host's group, in the group's order, as for every
host ([hostgroups.md](hostgroups.md)).

**A further step, on a board that does not try network boot first:** pick its UEFI network boot
entry once in the firmware's boot menu, over the BMC's remote console, when it powers on.

A machine whose install failed is not installed again in this run: its MAC left the answer set
at its seed fetch. Its console shows the installer's error. Restart `pxe.service` to try again.

When the window's machines are approved, move the boot host back out of `pxe-boot` (step 8).

## Measure the first machine

Once, on the first real machine of each platform: how long it took from power-on to serving,
with your wait (pending to approved) apart from the machine's time. Every time in it is one the
components record; nothing is timed by hand. Keep three files of that machine:

- The journal of `pxe.service`, as a file (`serve.log` below), for the whole run:
  `journalctl --unit pxe.service --output cat > serve.log`.
- Before you approve, in step 2: the answer of `enrollment list`, saved as
  `pending.json`. The request leaves the list at its approval, and its `firstSeenUnix` with it.

  ```
  hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
      enrollment list --limit 50
  ```

- Once the host serves a package of its group: its `hosts get`, saved as
  `host.json`. Its `firstServingUnix` is there once a heartbeat since the approval reported a
  service the coordinator manages for the host (not hostkeeper itself) healthy, with no update of
  it under way; until then take it again.

  ```
  hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
      hosts get --node-id web-17
  ```

Then, in a checkout of hostkeeper, name the machine as its record does and the mode `serve` ran
in:

```
cargo xtask netinstall-report --mac 00:00:5e:00:53:01 --host-id web-17 --mode proxy \
    --pxe-log serve.log --pending pending.json --host host.json
```

It prints each event in unix seconds and what recorded it (power-on is `serve`'s first
`DHCPDISCOVER` from the MAC in proxy mode, its first `DHCPREQUEST` in router mode; the install's
start is the seed fetch; pending, approved, the first heartbeat and serving are the
coordinator's, by its one clock), then each interval, the machine's time and your wait. An event
the files do not show is `not reached`. An interval whose end is stamped before its start says
`out of order`: the clock of `serve`'s host and the coordinator's disagree. Keep the report with
the machine's row in [What has been checked](#what-has-been-checked).

## Reinstall a known host

The host keeps its host id and its host group; its install disk is written anew, and it gets a
new key. Only the disk `install-disk` matches is written: other disks, such as a storage node's
data disks, are not touched by the installer. In router mode the record must have a platform;
to set one on a record, remove the record and add it again.

1. In `/etc/pxe/pxe.toml`, name the host id in `reinstall` of `[serve]` (an array, one id per
   host), and restart `pxe.service` (move the boot host into `pxe-boot` first if it is not there). For the
   records of those host ids only, `serve` sets aside the rule that a host or a pending request
   keeps a record out; the rest of the answer set's rule holds, and the MAC leaves the set when
   its install starts. It refuses to start when a `reinstall` names no record, and logs
   `--reinstall web-17: its record is installed even while a host or a pending request holds it`.

   ```toml
   # /etc/pxe/pxe.toml
   [serve]
   mode = "proxy"
   address = "192.0.2.10"
   interface = "eth1"
   daemon-config = "/etc/pxe/hostkeeper.toml"
   ssh-authorized-keys = "/etc/pxe/operator-ssh-keys"
   coordinator = "https://192.0.2.6:8443"
   ca-file = "/etc/pxe/ca.pem"
   token-file = "/etc/pxe/token"
   user-id = 65534
   group-id = 65534
   reinstall = ["web-17"]
   ```

2. Boot the host from the network. On a board that keeps network boot first, a reboot does it.
   Where curtin put the disk first (board check, item 9), pick network boot once: in the
   firmware's boot menu over the BMC's remote console, or on the running host, as `operator`,
   with `sudo efibootmgr` to find the network entry and `sudo efibootmgr --bootnext <entry>`
   before `sudo reboot`.
3. Once the host's new request appears in `enrollment list` (its install is done), take `reinstall` out of the file
   and restart `pxe.service` (or move the host out of `pxe-boot`); stopping it earlier, between the seed fetch and the deb's fetch,
   breaks the install. Do not leave the host id in `reinstall`: a later run would install the host
   once more at its next network boot.
4. The new daemon posts a request with a new key. Its MACs are bound to the enrolled host, so the
   request is `duplicate_claim`, pending with an alert, and matches the host's record. Compare the
   console's key hash and approve with `--replace true` and the host's group again (another group
   is refused `host_group_differs`):

   ```
   hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
       enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
       --host-id web-17 --host-group web --replace true \
       --reason "web-17 reinstalled from the network, key checked on the console"
   ```

5. The coordinator orders an `install` of each package of the host's group, in its order, since
   the new host reports none installed. The old key's certificate is refused at once (`forbidden` at its heartbeat,
   `renewal_not_allowed` at its renewal); nothing has to be revoked by hand.

A host that is retired instead (`hosts retire`, [enrollment.md](enrollment.md#retire-a-node))
keeps its record out of the answer set by its `hostHeld`; its record need not be removed.

## When something goes wrong

| What you see | Why, and what to do |
|---|---|
| The machine boots its disk; no `dhcp:` line for its MAC | Its firmware did not try network boot from the farm-LAN NIC, or its broadcasts do not reach the `pxe` host. Check its boot order and the NIC (board check), and `interface`. |
| `no answer: not in the answer set: no record holds it` | No record holds that MAC: record the MAC the firmware boots from. |
| `not in the answer set: the record of host <id>: a host is enrolled against it`, `a host is approved as that id`, `that id is denied, its retirement not final yet`, `that id is retired` | The record is held, so the machine is not installed. To rebuild that host, [reinstall](#reinstall-a-known-host). |
| `the record of host <id>: the coordinator does not say whether a host holds that id` | The coordinator answers `HOST_HELD_UNSPECIFIED`: it is of a release before the field, or runs without enrollment. Nothing is installed until it says `HOST_HELD_NONE`. |
| `the record of host <id>: the pending request <id> matches it` | A request waits for approval. If the machine is installed, approve it; if no machine of yours posted it, reject it, and the machine is installed at its next network boot. |
| `has no platform, which router mode needs` | Router mode installs nothing for a record with no platform. Remove the record and add it again with `--platform`, or use proxy mode. |
| `proxy offer sent` and no `tftp` line from the machine | DHCP guarding dropped the offer: trust the `pxe` host, or use router mode ([step 6](#6-set-the-router)). |
| `states the architecture <n>, which is not installed` | The firmware asks as legacy BIOS (0), 32-bit ARM (10) or another value: set UEFI boot. |
| `host <host id>: the record of <mac> is <platform>, and its firmware states <platform>` | The record's platform is wrong; it is not booted with the wrong kernel. |
| `tftp <address>: refused: hostkeeper/boot.efi from an address with no lease` | Router mode, and `serve` saw no DHCPREQUEST from that address in this run: the broadcasts do not reach it, or it started after the machine took its address. Boot the machine again. |
| `menu ...: local disk: ...` and the machine stops at its firmware's menu | The board does not boot its next option after GRUB's `exit` (board check, item 4): use proxy mode on it. |
| `the coordinator could not be read (<why>): this answer is made from the empty answer set` | The private listener, the CA or the token: fix it; the next network boot reads again. |
| `seed ...: refused: <mac> is no longer in the answer set as host <id>: <why>` | The record changed between the menu and the seed (a request, an approval, a removed record): nothing is installed. |
| `pxe.service` is failed or restarting, and its journal has `pxe serve: <why>` | `serve` refused its start: the settings, a socket, the coordinator or the seal, as the line says. Fix it and `systemctl restart pxe.service`. For a seal that differs, naming a file of `/var/lib/pxe/<version>`, move the host out of `pxe-boot` and back so `prepare` fetches anew. |
| The install of `pxe` fails, with `bad_settings` or `boot_set_refused` | `prepare` refused: the settings file or the daemon config, or a boot file that failed its attestation, size or digest (the host's `Last update` and the journal say which). The version that was serving keeps serving. Fix the file and wait for the next try. |
| The install stops with an error on the console | It is not installed again in this run: restart `serve` to try again. |
| The installed host posts no request | Its daemon cannot reach the LAN listener, or the coordinator's CA is not its `ca_sha256`; its console's issue line says which. See [enrollment.md](enrollment.md#bootstrap-a-linux-node). |

## What is served

By TFTP: `hostkeeper/boot.efi` (router mode: the shim of the reader's architecture),
`hostkeeper/shimx64.efi`, `hostkeeper/grubx64.efi`, `hostkeeper/shimaa64.efi`,
`hostkeeper/grubaa64.efi`, `grub/grub.cfg`, and the menu of each MAC,
`grub/hostkeeper/<x86_64|arm64>/<mac>`. By HTTP, `GET` and `HEAD` only: the same boot loaders,
`grub.cfg` and menus at `/` and their names, the ISO of each platform under its own file name,
`/vmlinuz-linux-<x86_64|arm64>`, `/initrd-linux-<x86_64|arm64>`,
`/deb/hostkeeper_<amd64|arm64>.deb`, and `/seed/<mac>/user-data` and `/seed/<mac>/meta-data`. A
seed is served only to a MAC that was given its install menu in this run. Every other name is
refused, and nothing is ever written. Another boot server could serve the same sealed files, but
it would have to keep the answer set's rule itself; hostkeeper does not support that.

## What has been checked

The **[V]** facts above, as each is checked on the real router and boards. Add a row when you
check one, with what you found.

**The router.** Not checked yet.

| Router | Network Boot: where, which fields | DHCP guarding: name, default | Broadcasts reach the `pxe` host |
|---|---|---|---|

**The board models.** None checked yet.

| Board model, firmware | Option 93, DISCOVER / REQUEST | Network boot first | After GRUB's `exit` | Wait with no server | Disks | RAM | Secure Boot | Serial console | Order kept after install |
|---|---|---|---|---|---|---|---|---|---|
