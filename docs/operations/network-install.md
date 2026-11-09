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

- `hostkeeper-pxe`, a boot server that runs in the foreground only while you run it. It ships in
  the hostkeeper deb and rpm as `/usr/bin/hostkeeper-pxe`, with no unit. `hostkeeper-pxe fetch`
  downloads and verifies a release's boot files into a new directory and seals it;
  `hostkeeper-pxe serve` serves that directory.
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
  `HOST_HELD_NONE`, unless `--reinstall` names its host id ([Reinstall](#reinstall-a-known-host));
- the record's `state` is `RECORD_STATE_OPEN`: its approval window is open and its one use not
  spent, so a machine whose window is over, or whose record is used, is not installed (open the
  record again with `enrollment open-record`, enrollment.md), unless `--reinstall` names its host
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

| | Router mode (`--mode router`) | Proxy mode (`--mode proxy`) |
|---|---|---|
| For | a farm LAN whose router you set, such as the new lab's UniFi router | a LAN whose router you leave as it is |
| The router | names `hostkeeper-pxe`'s address and the file `hostkeeper/boot.efi` for network-boot clients | gives addresses and names no boot file |
| `hostkeeper-pxe` | sends no DHCP packet; serves every network-boot client the shim and a menu made for its MAC | sends a second DHCP offer, with no address, only to a MAC of the answer set |
| A machine not to be installed | gets the shim and the menu that boots its local disk | gets no answer from `hostkeeper-pxe`, and its firmware goes on to its next boot device |
| DHCP guarding | needs no change | drops the proxy offer unless the `hostkeeper-pxe` host is trusted ([step 6](#6-set-the-router)) |
| UEFI HTTP boot | not served | served |
| The record's platform | required: it chooses the shim and the boot set | optional; if set, must agree with the firmware's architecture |

`--mode` has no default: the wrong mode answers nothing, or its answers are dropped.

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
writes by group and others (`chown root:root`, `chmod 0600`): `fetch` and `serve` load it with the
daemon's own loader and refuse what the daemon would refuse, before a seed is made.

### 3a. Write the repository entries of the installed hosts

Optional. With `--repositories <dir>`, every seed also writes the files of `<dir>` to
`/etc/hostkeeper/repositories/` on the installed host, before its `hostkeeper.toml`, so the daemon
starts with its repository entries (packages-repositories.md, sections 3.1 and 3.2). `<dir>` holds
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
Signed-By: /etc/hostkeeper/repositories/komira-archive-keyring.gpg
```

(`Suites: stable` for `komira-stable.sources`), and the keyring `komira-archive-keyring.gpg` is
fetched from `https://packages.komira.ai/apt/komira-archive-keyring.gpg` (see
[package-repositories.md](../deploy/package-repositories.md)). Make the directory and its files
root's, with no write for group or others (`chown -R root:root`, `chmod 0755` the directory,
`chmod 0644` the files); each keyring is at most 1 MiB. `serve` checks the directory at its start
with the daemon's own loader, as if it were at `/etc/hostkeeper/repositories/`, and refuses to
start on what the daemon would refuse (a `Trusted: yes`, a `Signed-By` naming `<dir>`, a file no
entry names, a link), naming the file, and on a directory that is missing or holds no entry. Check
the loader's decision before the install window, not on the first machine.

### 4. Choose the host of `hostkeeper-pxe`, and its token

`hostkeeper-pxe` needs a NIC on the farm LAN (in both modes it reads the DHCP broadcasts there)
and a route to the coordinator's private listener. Run it on the coordinator's own host, given a
NIC on the farm LAN: that host already holds every operator secret, so no credential moves. It
may run on another farm-LAN host that reaches the private listener, at the cost of a copy of an
operator token there. Install the hostkeeper deb on it, which brings `hostkeeper-pxe`, the pinned
`gh` and the Sigstore trusted root that `fetch` checks attestations with.

Put an operator token in a file of root's with mode 0600 or 0400. `serve` opens it as root,
drops to `--user-id` and `--group-id` once its sockets are bound, and only then reads it.

The farm LAN must be free of an active attacker while `serve` runs: nothing on the new machine
checks the initrd, the ISO or the seed (the design, sections 5 and 6, and its Question 2). Run
`serve` only for the install window.

### 5. Fetch and seal a release's boot files

As root, on the `hostkeeper-pxe` host, into a directory that does not exist yet:

```
hostkeeper-pxe fetch --release https://github.com/komira-ai/hostkeeper/releases/download/v<version> \
    --dir <boot-dir> --daemon-config <installed-hostkeeper.toml>
```

It checks the attestation of the release's `netboot.json` and of each deb against the
`[trusted_signers]` of `--daemon-config`, downloads each platform's ISO, takes the kernel,
initrd, shim and GRUB out of it, checks every size and SHA-256, and writes `seal.json` last. It
prints `sealed <boot-dir>: hostkeeper <version>, Ubuntu <version>, linux-arm64 and linux-x86_64`
and exits 0. Any refusal is one line on standard error, exit 1, and no directory is left. Both
platforms are fetched unless `--platform` names some: a farm of one architecture fetches only
its own.

```
hostkeeper-pxe fetch --release https://github.com/komira-ai/hostkeeper/releases/download/v<version> \
    --dir <boot-dir> --daemon-config <installed-hostkeeper.toml> --platform linux-x86_64
```

The files are readable by everyone (directories 0755, files 0644), so the unprivileged user
`serve` drops to can read them; the directory's parents must let it through as well.

### 6. Set the router

Only the person who holds the router's settings can make these checks and changes.

**Router mode, on UniFi.** The network's DHCP settings have a "Network Boot" option that takes a
server address and a file name **[V]**: where it is, whether it fills the DHCP `siaddr` and `file`
fields or options 66 and 67, and whether it applies to every client of the network. Set the
server to the `hostkeeper-pxe` host's farm-LAN address and the file to `hostkeeper/boot.efi`,
and nothing else. DHCP guarding stays as it is: router mode sends no DHCP packet for it to drop.
While the setting is on, every network-boot client of the network loads the shim from
`hostkeeper-pxe`, and a firmware whose network boot comes first waits for a TFTP answer at every
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
on the firmware. Capture the router's answer to a network-boot client on the `hostkeeper-pxe`
host, as root, while a machine boots from the network:

```
tcpdump -n -vvv -i <farm-lan-nic> -w <router-check.pcap> udp port 67 or udp port 68
```

Read it back with `tcpdump -n -vvv -r <router-check.pcap>` and look at the router's DHCPOFFER and
DHCPACK to that MAC. If either names a boot file, switch the router's boot file off once; it is
not worked around in `hostkeeper-pxe`. A next-server address with no file name is expected to be
harmless **[V]**. On UniFi, that is the "Network Boot" setting of router mode, off.

**Proxy mode: DHCP guarding.** A UniFi network can drop DHCP server messages from every address
but its trusted DHCP servers ("DHCP Guarding") **[V]**: its name, where it is set, and whether
it is on by default. A proxy offer is a DHCP server message, so with guarding on and the
`hostkeeper-pxe` host not trusted, the offer is dropped and the machine boots its disk as if
`serve` were not running. Either add the `hostkeeper-pxe` host's farm-LAN address to the trusted
DHCP servers, or use router mode. The offer is dropped when `serve`'s log shows `proxy offer sent`
for a MAC and no `tftp` line follows from that machine.

**Both modes: the broadcasts reach `hostkeeper-pxe`.** Each mode reads the firmware's DHCP
broadcasts on the farm LAN (in router mode, to learn which address took which MAC). That the
switches pass client broadcasts to the `hostkeeper-pxe` host with guarding on is **[V]**: it is
true when `serve` logs a `dhcp:` line for a booting machine's MAC.

### 7. Start `serve`

As root, on the `hostkeeper-pxe` host. Proxy mode:

```
hostkeeper-pxe serve --mode proxy --address 192.0.2.10 --interface <farm-lan-nic> \
    --dir <boot-dir> --daemon-config <installed-hostkeeper.toml> \
    --ssh-authorized-keys <operator-ssh-keys> --repositories <repositories-dir> \
    --coordinator https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    --user-id 65534 --group-id 65534
```

Router mode is the same with `--mode router`:

```
hostkeeper-pxe serve --mode router --address 192.0.2.10 --interface <farm-lan-nic> \
    --dir <boot-dir> --daemon-config <installed-hostkeeper.toml> \
    --ssh-authorized-keys <operator-ssh-keys> \
    --coordinator https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    --user-id 65534 --group-id 65534
```

- `--address`: the host's IPv4 address on the farm LAN, which its answers, menus and seeds name.
  `--interface`: the farm-LAN NIC; with it, every socket takes only that interface's packets.
- `--ssh-authorized-keys`: a file of SSH public keys for the installed system's one local
  account, `operator`, whose password is locked and who has sudo without a password. A file with
  a private key block, or with no key, is refused.
- `--user-id`, `--group-id`: the unprivileged user and group to drop to, as numbers (on Ubuntu,
  65534 is `nobody` and `nogroup`; a user made for it does as well).
- `--repositories`: the directory of step 3a, optional; without it a seed writes no repository
  entry.
- `--install-disk`: the disk the installer writes, `serial=<glob>`, `model=<glob>` or
  `path=<glob>`, the smallest of those it matches. Without it, the smallest SSD (a disk the
  kernel reports as not rotational). Only that disk is written. See the disks of the board check.
- `--serial-console ttyS<n>` or `ttyAMA<n>`: the installed system's kernel console and a getty
  on that port at 115200 baud, so the BMC's serial console shows the key hash. Which port the
  BMC's serial console is depends on the board model **[V]** (the board check records it).
- `--dhcp-listen`, `--proxy-listen`, `--tftp-listen`, `--http-listen`: by default the protocol
  ports 67, 4011, 69 and 80 on every address. Keep the HTTP port at 80: a port other than 80 is
  written into GRUB's device name, which Ubuntu's signed GRUB is not known to read **[V]**.
- `--tftp-timeout-ms`: how long a TFTP transfer waits for an ACK before it sends again, 1000 by
  default.

It refuses to start, with one line on standard error and exit 1, when a file of its flags does
not load, a socket cannot be bound, the sealed directory differs from its seal (naming the file),
the coordinator cannot be read, or a `--reinstall` names no record. Each line it logs goes to
standard error as `hostkeeper-pxe serve: <unix seconds> <text>`; keep them (redirect standard
error to a file) for the measurement of a first machine. A start logs:

- `started in proxy mode as uid 65534 gid 65534, answering as 192.0.2.10: the boot sets of
  hostkeeper <version> for linux-x86_64 and linux-arm64; DHCP 0.0.0.0:67, proxyDHCP
  0.0.0.0:4011, TFTP 0.0.0.0:69, HTTP 0.0.0.0:80` (`proxyDHCP not bound` in router mode);
- `the coordinator holds <n> records and <n> pending requests; <n> MACs are in the answer set`.

Check that number before a machine boots: it counts the data MACs `serve` would install now. In
router mode it counts only the records that have a platform.

Stop it with Ctrl-C or SIGTERM; it logs `stopped` and exits 0. It keeps nothing: the next run
starts with nothing learned.

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
installed: proxy mode gives it no answer, and router mode gives it the local-disk menu. Start `serve` in either mode, and the capture of [step 6](#6-set-the-router) to
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
5. **The wait with no server (router mode).** Stop `serve`, leave the router's Network Boot on,
   and boot: record how long the firmware waits for TFTP before it goes on, and that it does.
6. **The disks.** List the board's disks, from the firmware's or the BMC's storage inventory, or
   with `lsblk -d -o NAME,SIZE,ROTA,RM,TRAN,MODEL,SERIAL` on a running system of the same model.
   The default `--install-disk` takes the smallest non-rotational disk, and the installer's match
   has no key that leaves out a removable one: if a removable disk reports itself not rotational
   and is the smallest, the default would take it. On such a board, and on any board whose boot
   disk is not the smallest SSD, pass `--install-disk serial=<glob>` or `model=<glob>`.
7. **Memory.** The live system loads the whole ISO into memory, so a machine needs several GiB
   of RAM **[V]**: record the board's RAM and whether the install succeeded.
8. **The serial console.** Which `ttyS<n>` the BMC's serial console reads, for
   `--serial-console`.
9. **After the first install of the model,** whether the board keeps the boot order curtin
   writes. Curtin puts the installed system first in the UEFI boot order after an install
   (`reorder_uefi`, at its default) **[V]**; on the installed host, `sudo efibootmgr` shows the
   order. Then, with `serve` running, reboot the host: a DISCOVER from its MAC means network boot
   still comes first. Either way it is not installed again (it is enrolled, or pending). This
   tells you whether a reinstall needs a one-time network boot.

## Each machine

Before the first machine, check `serve`'s start line and the answer set's count (above).

**Step 1. Record the machine.** From the chassis label: its data MACs, its BMC MAC and its
serial, as in [enrollment.md](enrollment.md#record-the-server). Record the MAC of the farm-LAN
NIC the firmware boots from among the data MACs. Give the record its host id and its role: the
record is the approval, so the machine that matches it, within its window and once, is enrolled
with no further step ([enrollment.md](enrollment.md#the-record-is-the-approval)). The window is
24 hours unless you give `--approve-for-secs` or `--approve-until-unix`; enter the record shortly
before the machine boots.

```
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment add-record --host-id web-17 --role web \
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

**Step 2. Check that it enrolled.** The coordinator approves the request itself, as `web-17` with
the record's role, and the daemon takes its certificate at its next poll. No approve is run. The
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
    --role web --services api --reason "network install, key checked on the console"
```

A request that matches a record another request also matches is `contested` and cannot be
approved until the other is rejected ([enrollment.md](enrollment.md#the-pending-request-and-its-match)).
The coordinator then orders the services assigned to the host or its role, as for every host.

**A further step, on a board that does not try network boot first:** pick its UEFI network boot
entry once in the firmware's boot menu, over the BMC's remote console, when it powers on.

A machine whose install failed is not installed again in this run: its MAC left the answer set
at its seed fetch. Its console shows the installer's error. Restart `serve` to try again.

When the window's machines are approved, stop `serve`. In router mode, turn the router's Network
Boot off again if the wait of a network boot with no server matters on the farm's boards.

## Measure the first machine

Once, on the first real machine of each platform: how long it took from power-on to serving,
with your wait (pending to approved) apart from the machine's time. Every time in it is one the
components record; nothing is timed by hand. Keep three files of that machine:

- `serve`'s standard error, as a file (`serve.log` below), for the whole run.
- Before you approve, in step 2: the answer of `enrollment list`, saved as
  `pending.json`. The request leaves the list at its approval, and its `firstSeenUnix` with it.

  ```
  hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
      enrollment list --limit 50
  ```

- Once the host serves a service assigned to it: its `hosts get`, saved as
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

The host keeps its host id, role and services; its install disk is written anew, and it gets a
new key. Only the disk `--install-disk` matches is written: other disks, such as a storage node's
data disks, are not touched by the installer. In router mode the record must have a platform;
to set one on a record, remove the record and add it again.

1. Start `serve` with `--reinstall <host id>` (repeatable). For the records of those host ids
   only, it sets aside the rule that a host or a pending request keeps a record out; the rest of
   the answer set's rule holds, and the MAC leaves the set when its install starts. It refuses to
   start when a `--reinstall` names no record, and logs `--reinstall web-17: its record is
   installed even while a host or a pending request holds it`.

   ```
   hostkeeper-pxe serve --mode proxy --address 192.0.2.10 --interface <farm-lan-nic> \
       --dir <boot-dir> --daemon-config <installed-hostkeeper.toml> \
       --ssh-authorized-keys <operator-ssh-keys> \
       --coordinator https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
       --user-id 65534 --group-id 65534 --reinstall web-17
   ```

2. Boot the host from the network. On a board that keeps network boot first, a reboot does it.
   Where curtin put the disk first (board check, item 9), pick network boot once: in the
   firmware's boot menu over the BMC's remote console, or on the running host, as `operator`,
   with `sudo efibootmgr` to find the network entry and `sudo efibootmgr --bootnext <entry>`
   before `sudo reboot`.
3. Once the host's new request appears in `enrollment list` (its install is done), stop that run
   of `serve`; stopping it earlier, between the seed fetch and the deb's fetch, breaks the
   install. Do not start it again with the same `--reinstall`: a new run would install the host
   once more at its next network boot.
4. The new daemon posts a request with a new key. Its MACs are bound to the enrolled host, so the
   request is `duplicate_claim`, pending with an alert, and matches the host's record. Compare the
   console's key hash and approve with `--replace true`, the role and the services again:

   ```
   hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
       enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
       --host-id web-17 --role web --services api --replace true \
       --reason "web-17 reinstalled from the network, key checked on the console"
   ```

5. The coordinator orders an `install` of each assigned service, since the new host reports none
   installed. The old key's certificate is refused at once (`forbidden` at its heartbeat,
   `renewal_not_allowed` at its renewal); nothing has to be revoked by hand.

A host that is retired instead (`hosts retire`, [enrollment.md](enrollment.md#retire-a-node))
keeps its record out of the answer set by its `hostHeld`; its record need not be removed.

## When something goes wrong

| What you see | Why, and what to do |
|---|---|
| The machine boots its disk; no `dhcp:` line for its MAC | Its firmware did not try network boot from the farm-LAN NIC, or its broadcasts do not reach the `hostkeeper-pxe` host. Check its boot order and the NIC (board check), and `--interface`. |
| `no answer: not in the answer set: no record holds it` | No record holds that MAC: record the MAC the firmware boots from. |
| `not in the answer set: the record of host <id>: a host is enrolled against it`, `a host is approved as that id`, `that id is denied, its retirement not final yet`, `that id is retired` | The record is held, so the machine is not installed. To rebuild that host, [reinstall](#reinstall-a-known-host). |
| `the record of host <id>: the coordinator does not say whether a host holds that id` | The coordinator answers `HOST_HELD_UNSPECIFIED`: it is of a release before the field, or runs without enrollment. Nothing is installed until it says `HOST_HELD_NONE`. |
| `the record of host <id>: the pending request <id> matches it` | A request waits for approval. If the machine is installed, approve it; if no machine of yours posted it, reject it, and the machine is installed at its next network boot. |
| `has no platform, which router mode needs` | Router mode installs nothing for a record with no platform. Remove the record and add it again with `--platform`, or use proxy mode. |
| `proxy offer sent` and no `tftp` line from the machine | DHCP guarding dropped the offer: trust the `hostkeeper-pxe` host, or use router mode ([step 6](#6-set-the-router)). |
| `states the architecture <n>, which is not installed` | The firmware asks as legacy BIOS (0), 32-bit ARM (10) or another value: set UEFI boot. |
| `host <host id>: the record of <mac> is <platform>, and its firmware states <platform>` | The record's platform is wrong; it is not booted with the wrong kernel. |
| `tftp <address>: refused: hostkeeper/boot.efi from an address with no lease` | Router mode, and `serve` saw no DHCPREQUEST from that address in this run: the broadcasts do not reach it, or it started after the machine took its address. Boot the machine again. |
| `menu ...: local disk: ...` and the machine stops at its firmware's menu | The board does not boot its next option after GRUB's `exit` (board check, item 4): use proxy mode on it. |
| `the coordinator could not be read (<why>): this answer is made from the empty answer set` | The private listener, the CA or the token: fix it; the next network boot reads again. |
| `seed ...: refused: <mac> is no longer in the answer set as host <id>: <why>` | The record changed between the menu and the seed (a request, an approval, a removed record): nothing is installed. |
| `serve` refuses to start, naming a file of `<boot-dir>` | The directory differs from its seal: fetch it again into a new directory. |
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

| Router | Network Boot: where, which fields | DHCP guarding: name, default | Broadcasts reach the `hostkeeper-pxe` host |
|---|---|---|---|

**The board models.** None checked yet.

| Board model, firmware | Option 93, DISCOVER / REQUEST | Network boot first | After GRUB's `exit` | Wait with no server | Disks | RAM | Secure Boot | Serial console | Order kept after install |
|---|---|---|---|---|---|---|---|---|---|
