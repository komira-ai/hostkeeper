# Network install of new Linux machines

Status: design, for review. This document makes network install of new Linux machines part of hostkeeper v1. It
replaces the non-goal of [hostkeeper-design.md](hostkeeper-design.md) section 2.2 ("No network boot service in
scope") and the first item of its section 12.1 ("the operator's own, no build items here"). Both now point here. Where
this document and hostkeeper-design.md disagree on network install, this document wins. What is built is marked
**Built** where it is described (so far items N2, N3, N4, N5, N6, N7, N8, N9 and N12 of section 12); every other statement about
new code is **Designed**, and section 12 lists the build items.

This revision (item N1b of section 12) amends the first: network boot early in the boot order is the expected setup, so
a machine takes two manual steps; a router mode, in which the farm's router names the boot server, stands beside
proxyDHCP; `linux-arm64` is in v1; and the inventory record gains an optional platform.

The tags are those of hostkeeper-design.md. **Built** names code on `main` with a path. **Designed** means nothing
implements it yet. **[V]** marks a fact about software or hardware we do not own, which must be checked on the real
thing before the item that relies on it is built.

## 1. What changes for the operator

**Before.** A new Linux machine is installed by hand: someone writes Ubuntu to its disk, configures the network,
installs the hostkeeper deb, writes `/etc/hostkeeper/hostkeeper.toml`, and starts the daemon. Only then does it appear
as a pending enrollment request (hostkeeper-design.md section 5.7, docs/operations/enrollment.md "Bootstrap a Linux
node").

**After.** The farm's server boards already try network boot early in their boot order, and this design takes that as
the expected setup of a new machine (**[V]** on each board model; a board that does not is the exception below). The
operator:

1. Records the machine as an inventory record: its data MACs, its BMC MAC and serial from the chassis label, and its
   platform, `linux-x86_64` or `linux-arm64` (section 3). A Linux request is matched by its data MACs and BMC MAC; the
   serial is shown beside the claim for the person to compare, and takes no part in the match. The record is built
   (`hostkeeper ctl enrollment add-record`, or the UI's Inventory page); its platform is a designed, additive field
   (item N14).

The machine is then racked and powered on, as every machine is. Its firmware tries network boot, it installs Ubuntu
24.04 and the hostkeeper deb of its platform, reboots, and its daemon posts an enrollment request. That request shows
up in the pending list, matched to the record. The machine's console shows the key hash of the request.

2. The operator compares that key hash with the one in the UI, approves the request, and the coordinator installs the
   services assigned to the host or its role, as it does today.

That is two manual steps for each machine. A machine whose firmware does not try network boot before its disk needs a
third, once: someone picks network boot in its firmware's boot menu. Three things are done once per farm: the
coordinator runs with a LAN listener (section 7); the operator chooses how network boot is offered on the farm LAN,
router mode or proxy mode, and sets the router up for it (section 3); and the operator runs `hostkeeper-pxe` on a host
of the farm LAN while machines are being installed (section 3).

**Why network boot first is safe.** With network boot ahead of the disk, every reboot of every machine asks the network
first, enrolled hosts included. No enrolled, retired or pending machine is ever given an install: in proxy mode it gets
no DHCP answer, and in router mode its boot menu boots the local disk (section 3). That rule does not rely on the boot
order. After an install, curtin also puts the installed system first in the UEFI boot order (its `reorder_uefi`, which
the seed leaves at its default), so a host's later reboots skip network boot where the firmware keeps that order. Both
are **[V]**: that `reorder_uefi` is on by default in the 24.04 installer on both architectures, and that each board
model keeps the order rather than restoring its own. If either fails, the safety rule above is unchanged, and a
reinstall needs no one-time network boot (section 8).

**What does not change.**

- A match approves only in one case. An exact, uncontested match of an open, unused record that names a role is approved
  by the coordinator within the record's window, once (audited `enrollment.auto_approved`); any other match fills in the
  form and nothing more, and a person approves it. Q15 of hostkeeper-design.md is read accordingly.
- The image carries no secret. The host's only trust root is still `hostkeeper.toml`, and its contents are as
  hostkeeper-design.md section 4.1 gives them.
- The six existing farm nodes are not reinstalled. They keep the bootstrap and adopt path they use today.
- Macs stay on MDM (hostkeeper-design.md section 5.9): Apple silicon cannot net-boot.
- No firmware, BMC or router setting is changed by hostkeeper. The operator sets the router once (section 3).

## 2. The flow

```
operator          hostkeeper-pxe          new machine                    coordinator (LAN listener)
   | add-record (MACs, platform) --------------------------------------------> inventory
   | start ------> reads records (host_held), pending
   |                                       power on; firmware tries network boot: DHCP DISCOVER
   |   proxy mode:  the router's DHCP gives the address; hostkeeper-pxe's proxy offer gives the boot file
   |   router mode: the router's DHCP gives the address and names hostkeeper-pxe and its boot file
   |              <- TFTP: shim and GRUB of the machine's architecture, grub.cfg, the menu of its MAC
   |              <- HTTP: kernel, initrd, ISO, seed, deb of its platform
   |                                       autoinstall: disk, Ubuntu, hostkeeper deb, config
   |                                       reboot: no install answer for this MAC, so the disk boots
   |                                       daemon: P-256 key, claims ----> Enroll (no certificate)
   |                                       console shows key hash          pending, matched
   | compare key hash, approve ------------------------------------------------> approval by key hash
   |                                       Poll ----------------------------> 7-day certificate
   |                                       Heartbeat (mutual TLS) ---------> install orders
```

## 3. Who serves network boot

**Recommendation: a separate, optional binary, `hostkeeper-pxe`.** It is a read-only TFTP and HTTP file server and, in
proxy mode, a proxyDHCP server. It runs only while the operator runs it, in the foreground; its packages enable no
unit. It installs only the machines the operator recorded. It keeps no durable state: what it learns in a run (below)
is in memory, and it reads the inventory from the coordinator each time.

**Two modes, one rule.** How a firmware learns the boot server's address and file name differs by network, so
`hostkeeper-pxe` has two modes, chosen by the flag `--mode proxy|router`. It has no default: the wrong mode either
answers nothing or is dropped by the switches (below), so the operator names the one the farm LAN is set up for.

- **Router mode, for a farm LAN whose router the operator sets.** The router's DHCP names `hostkeeper-pxe`'s address
  as the boot server and one fixed file name, `hostkeeper/boot.efi`, in every answer to a network-boot client. On a
  UniFi router this is the network's "Network Boot" setting, a server address and a file name **[V]**: where it is,
  whether it fills the `siaddr` and `file` fields or options 66 and 67, and that it applies to every client of the
  network. `hostkeeper-pxe` sends no DHCP packet in this mode. Every PXE client of the LAN then loads the signed shim
  and GRUB from it, enrolled hosts and machines nobody recorded included, and GRUB reads a boot menu made for its MAC
  (section 4). The menu of a MAC that may be installed has the install entry. The menu of every other MAC has one
  entry, which boots the local disk. The safety rule therefore moves from the DHCP answer to the boot menu. This is the
  mode for the farm's new lab, whose router is UniFi.
- **Proxy mode, for a LAN whose router is left as it is.** A proxyDHCP server (PXE specification 2.1) never gives an
  address. The router keeps its DHCP and names no boot file. When a firmware's DHCPDISCOVER says it is a network-boot
  client (vendor class `PXEClient` or `HTTPClient`) of an architecture `hostkeeper-pxe` has a boot set for (below), and
  its MAC may be installed, `hostkeeper-pxe` sends a second offer. That offer carries no address (`yiaddr` 0). It echoes
  the vendor class (option 60), carries the PXE vendor options (option 43, with discovery control set so the client uses
  the boot file of the offer), and names the boot file (option 67): the TFTP name of the shim of that architecture for
  `PXEClient`, an `http://` URL for `HTTPClient`. A PXE client that then sends its DHCPREQUEST to the proxy on UDP 4011
  gets an ACK with the same boot file. The firmware takes its address from the router's offer and its boot file from
  ours. Every other DISCOVER gets no answer, so its firmware goes on to its next boot device. Whether a given firmware's
  HTTP boot accepts a proxy offer is **[V]**; one that does not still boots by UEFI PXE, which every UEFI server
  firmware has. The x86-64 end-to-end test of section 11 installs in this mode first, then switches to router mode.
  Proxy mode is also the mode for any LAN whose router is not ours to set.

**DHCP guarding.** A UniFi network can drop DHCP server messages from any address but the trusted DHCP servers ("DHCP
Guarding") **[V]**: its name, where it is set, and that it is on by default. A proxy offer is a DHCP server message from
UDP 67, so with guarding on it is dropped and the machine boots its disk as if `hostkeeper-pxe` were not running. The
runbook (item N12) therefore either adds the `hostkeeper-pxe` host's address to the trusted servers, or uses router
mode, in which `hostkeeper-pxe` sends no DHCP message for guarding to drop. Router mode needs no change to guarding.

**Which machines may be installed: the answer set.** At start, and again before each answer, `hostkeeper-pxe` reads
through the operator API the inventory (`Enrollment.ListRecords`) and the pending requests (`Enrollment.List`). A MAC is
in the answer set when it is a data MAC of a record that is not enrolled, whose `host_held` is `NONE`, whose
approval window is open (`InventoryRecord.state` is `OPEN`: `CLOSED`, `USED` and `UNSPECIFIED`, a coordinator before
windows, leave it out), that no pending request matches, and whose platform agrees with the firmware's (below). Only a MAC in the set gets the proxy offer, in
proxy mode, or the install entry, in router mode. A machine that is enrolled, retired, or installed and waiting for
approval therefore boots its disk whenever it boots the network first. Without that rule, every such machine would be
wiped at its next reboot while `hostkeeper-pxe` runs, which matters more now that network boot early in the boot order
is the expected setup (section 1). The `enrolled` mark of a record alone is not enough: it is false for a retired host
and for a machine whose request waits for approval. Reinstall of a known host is a separate, named opt-in (section 8).

**`host_held` is the rule, `Hosts.List` is not.** Each record of `Enrollment.ListRecords` carries `host_held`: what the
coordinator's durable enrollment store holds for the record's host id, read from the store at each call. It is `NONE`
(no host by that id), `APPROVED` (a host approved as the id and not denied), `DENIED` (`Hosts.Retire` denied the id and
its retirement is not final yet) or `RETIRED` (denied and final), and `UNSPECIFIED` from a coordinator of a release
before the field or one without enrollment. Only `NONE` admits a record; every other value, `UNSPECIFIED` included,
leaves it out, so a coordinator that has not said installs nothing. `Hosts.List` cannot carry the rule: it lists only
the hosts heard from since the coordinator started, and a retired host is refused at its heartbeat, so after a
coordinator restart a retired host, or an approved host that has not yet sent a heartbeat, is not listed, and its
record would look free to install. `host_held` is the same before and after a restart, and the same for a host never
heard from as for one heartbeating.

**A failed read fails closed.** The answer set is made from the reads taken for that answer, never from an earlier
run's or an earlier answer's. When either of the two reads fails (the private listener does not answer, the token is
refused, or an answer does not parse), the answer set for that answer is empty: no proxy offer in proxy mode, the
local-disk menu in router mode. `hostkeeper-pxe` logs the failure, and the machine's next network boot reads again. A
coordinator that cannot be read therefore never causes an install, and an enrolled host is never installed from a
stale list.

**A MAC leaves the answer set when its install starts.** Once a machine has fetched its seed, `hostkeeper-pxe` takes
its MAC out of the set for the rest of the run, so the reboot after the install boots the disk: no proxy offer in proxy
mode, the local-disk menu in router mode. Curtin also puts the installed disk first in the UEFI boot order (section 1).
A machine whose install failed is booted again by restarting `hostkeeper-pxe`.

**The architecture, and the record's platform.** Each sealed directory holds one boot set per platform the operator
fetched (section 5): `linux-x86_64`, `linux-arm64`, or both.

- **The firmware's architecture.** A network-boot client states its architecture in DHCP option 93. In the IANA
  registry (RFC 4578 and its updates) 9 is x86-64 UEFI, 11 is arm64 UEFI, 16 is x86-64 UEFI HTTP boot and 19 is arm64
  UEFI HTTP boot. 7 is registered as EFI byte code, but x86-64 UEFI firmware commonly sends it, so it is taken as
  x86-64 **[V]**: which value each board model sends, which the runbook records (item N12). Every other value (0, legacy
  BIOS; 10, 32-bit ARM UEFI; the rest) is a platform this design does not install, and gets no proxy offer. In proxy
  mode `hostkeeper-pxe` reads option 93 from the DISCOVER it answers. In router mode it does not answer DHCP, but it
  still listens on UDP 67. A client's DISCOVER and its DHCPREQUEST in the selecting state are broadcasts (RFC 2131
  section 4.3.2). The REQUEST carries the client's MAC (`chaddr`) and the address the router offered (option 50), and
  PXE 2.1 has a client send option 93 in both. So `hostkeeper-pxe` learns, for each address, the MAC of the client
  that took it, and its architecture: from the REQUEST's option 93, or else from the option 93 of a DISCOVER with the
  same `chaddr`. Two facts here are **[V]**: that each board model's firmware sends option 93 in those packets (the
  PXE specification is not proof of a firmware), which the runbook checks with a capture (item N12); and that the
  farm's switches pass client broadcasts to the `hostkeeper-pxe` host with guarding on. A client whose packets carry
  no option 93 is placed by its record's platform instead (below). GRUB states its architecture too, in the path of
  the menu it asks for (section 4).
- **The record's platform.** The record carries an optional platform (item N14): `linux-x86_64` or `linux-arm64`. It
  chooses the boot set of the install entry, and the deb the seed installs. It is additive everywhere it appears: a new
  optional field of `InventoryRecord` and `AddRecordRequest`, typed as the existing `hostkeeper.agent.v1.Platform` with
  `PLATFORM_UNSPECIFIED` meaning "not recorded"; a `--platform` flag of `hostkeeper ctl enrollment add-record` and a
  field of the UI's Inventory page; and a value the coordinator's store keeps for the record, behind the store's
  interface with the rest of the inventory (`crates/hostkeeper-coordinator/src/inventory.rs`, **Built**), so the move of
  that state to Postgres takes it as a column with a default. In today's file store the inventory is in the ops file,
  whose reader refuses unknown keys, so a platform there would make the release before refuse the whole file. The
  platform is therefore kept as the update plan's position is: under a new key of the halts file beside the ops file, a
  map from host id to platform, which the release before reads leniently and skips, and which the store joins back to
  the records on reading (hostkeeper-design.md, its table of stored files). A rollback past item N14 then loses
  only the platforms. Only the Linux platforms are accepted; a Mac's record leaves it empty.
- **Disagreement is refused.** A record whose platform disagrees with the architecture the firmware or GRUB states is
  left out of the answer set, and `hostkeeper-pxe` logs the record's host id with both platforms. This catches a record
  typed with the wrong platform before the wrong kernel is booted on a machine's disk.
- **A record with no platform.** In proxy mode it takes the firmware's, from option 93. In router mode it is left out
  of the answer set, and logged: the router names one file for every client, so the record is where the install's
  platform is stated. Its MAC still gets the shim and the local-disk menu when its firmware states an architecture.
- **A platform with no boot set.** A record whose platform has no boot set in the sealed directory is left out of the
  answer set, and logged. `serve` logs at start the platforms it has.

**The first file in router mode.** The router names `hostkeeper/boot.efi` for every client, and that one name must load
the shim of the client's architecture. `hostkeeper-pxe` serves it, to a TFTP read request from an address, as the shim
of the architecture it learned for that address (above), or, when the client's packets stated none, of the platform of
the record whose data MAC the client has. An address it learned no MAC for, a MAC with neither, and an architecture with
no boot set in the sealed directory get a TFTP error, and its firmware moves on to its next
boot device **[V]** on each board model. The shim then reads GRUB by its own name, which differs by architecture
(`grubx64.efi`, `grubaa64.efi`), from the same directory **[V]**. Both boot loaders of each platform are therefore
served from `hostkeeper/`, the directory of `hostkeeper/boot.efi`: `hostkeeper/shimx64.efi` and
`hostkeeper/grubx64.efi`, `hostkeeper/shimaa64.efi` and `hostkeeper/grubaa64.efi`, by TFTP and under the HTTP root
alike, and the proxy offer names the shim by that path, so the shim finds GRUB beside it in both modes. **Built**:
the packet handling, `crates/hostkeeper-pxe-packets` (item N7).

**What it serves, and how it runs.**

- **What it serves.** By TFTP, for firmware that does UEFI PXE: the shim and GRUB of each boot set, `grub.cfg`, and the
  boot menu of each MAC (section 4). By HTTP, for UEFI HTTP boot and for everything after the boot loader: the kernel,
  the initrd and the Ubuntu ISO of each boot set, the hostkeeper deb of each platform, and each machine's seed. It
  serves only files it verified at start (section 5) and the menus and seeds it generates. Every request for any other
  path is refused, and it never accepts a write.
- **Flags, not environment** (rule S2): `--mode`, the interface to answer on, each listen address and port (the
  protocol ports 67, 4011 and 69, and an HTTP port, by default; router mode does not use 4011), the coordinator's
  private listener and the operator token file to read with, the verified boot directory, the seed inputs (section 4),
  and `--reinstall <host id>` (section 8).
- **Where it runs, and as whom.** It runs on a host with a NIC on the farm LAN that can also reach the coordinator's
  private listener (Question 3). It needs the farm LAN in both modes: in proxy mode for the DISCOVER broadcasts, in
  router mode for the REQUEST broadcasts it learns architectures from. It starts as root only to bind its ports, then
  drops to an unprivileged user before it reads the token file (opened before the drop and kept as a descriptor) or
  parses a packet. Sending broadcasts needs no privilege once the socket is bound.
- **Built**: `crates/hostkeeper-pxe/src/serve.rs` and `serve/` (item N9). The flags as built: `--mode`; `--address`,
  the server's farm-LAN address that its answers, menus and seeds name; `--interface`; `--dhcp-listen`,
  `--proxy-listen`, `--tftp-listen` and `--http-listen`, by default the protocol port on every address;
  `--dhcp-reply-to`, where a proxy offer goes, by default `255.255.255.255:68`; `--coordinator`, `--ca-file` and
  `--token-file`; `--dir`; the seed inputs; `--reinstall`; `--user-id` and `--group-id`, the user to drop to, as
  numbers, so nothing reads the account database; and `--tftp-timeout-ms`, how long a TFTP transfer waits for an
  ACK before it sends again. The start reads the coordinator once and refuses to start when that read fails, or
  when a `--reinstall` names no record. The fixed `grub.cfg` and the menus are served under `grub/` by HTTP as
  well as by TFTP, so a GRUB loaded by UEFI HTTP boot finds them at the same path below the server. A seed is
  served only to a MAC given its install menu in the run; its first fetch reads the answer set again and is
  refused unless the MAC is still in it. The binary ships in the hostkeeper deb and rpm as
  `/usr/bin/hostkeeper-pxe`, with no unit of its own. It reads `Enrollment.ListRecords` and `Enrollment.List`,
  not `Hosts.List`, and leaves out every record whose `host_held` is not `NONE`. One fact the built code holds
  differently from the text above, until item N14 closes it: `InventoryRecord` has no platform yet, so every
  record reads as one with no platform, and router mode installs nothing yet.

**The L2 requirement.** A DHCPDISCOVER and a selecting DHCPREQUEST are broadcasts. `hostkeeper-pxe` sees them only on
the same broadcast domain, unless a relay forwards them; this design adds no relay. Both modes rely on the router's
DHCP on that segment for the address. The farm has two segments:

- **The farm LAN** has the router's DHCP. `hostkeeper-pxe` serves this segment. A new machine must have one NIC on it,
  and that NIC must be the one the firmware boots from.
- **The storage network** is static, with no DHCP. Neither mode has an address to give there, so a machine that is
  only on it cannot be installed this way. That would need a full DHCP server or a relay, which is not in v1 (section
  10).

**In proxy mode the router names no boot file.** If it offers one of its own (options 66 and 67, or a `file` field),
the firmware may prefer the router's offer, and the result depends on the firmware. A next-server address with no file
name is harmless **[V]**. In router mode the router names `hostkeeper-pxe` and `hostkeeper/boot.efi`, and nothing else.
The runbook's first step checks which of the two the router does. This is Question 4.

**Alternatives considered.**

- **The coordinator serves network boot.** This was rejected. It would need a UDP 67 listener, broadcast and raw
  socket rights, and a TFTP server in the farm's control plane, which runs all the time. Network install is wanted for
  a few hours when machines arrive. It would also put the coordinator on the farm LAN's broadcast domain, which ties
  where the coordinator runs to where machines are racked. The coordinator stays as simple as hostkeeper-design.md
  section 4.3 makes it. It gains one listener (section 7), and that listener serves only the host-facing API that
  already exists.
- **A documented recipe only** (dnsmasq or the router's own options, with hostkeeper shipping only the image). This
  was rejected as the primary path. Installing only recorded, unenrolled MACs is the safety rule that keeps an enrolled
  host from being wiped, and in a recipe it is a list the operator keeps by hand. A recipe also has no code that CI can
  test end to end. It is still possible: the verified boot directory and the seeds are plain files, and the runbook
  of item N12 says what another server must serve.
- **A full DHCP server.** This was rejected. It would take over the router's job on the farm LAN, and two DHCP servers
  giving addresses on one segment is an outage.
- **The router's boot options pointing at a plain TFTP server.** This was rejected: it cannot tell enrolled machines
  from new ones, so with network boot first every enrolled host would be reinstalled at its next reboot. Router mode
  keeps the router's options and moves that decision into `hostkeeper-pxe`'s per-MAC menu. What router mode keeps of
  the earlier objection: the router points every PXE client at the boot server for as long as the setting is on, and a
  firmware whose network boot comes first waits for its TFTP request to fail at every boot while `hostkeeper-pxe` is
  not running **[V]**: how long each board model waits. The runbook turns the router's setting on for the install
  window where that wait matters. CI cannot test a real UniFi router; the end-to-end test's fake router DHCP names the
  boot file the way router mode needs (section 11).
- **Choosing the first file by the router.** A router that names a file per client architecture (by option 93) would
  make the router-mode mapping of `hostkeeper/boot.efi` unneeded. Whether UniFi can is **[V]**; the design does not
  need it.

## 4. What a bare machine boots into

**Recommendation: the Ubuntu 24.04 live-server installer, driven by autoinstall, with a NoCloud seed that
`hostkeeper-pxe` generates for each machine.** The installer writes the root file system, installs the hostkeeper
deb, writes `hostkeeper.toml`, and reboots. The daemon starts from the installed disk and enrolls.

- **The boot chain.** The firmware loads Ubuntu's signed shim of its architecture (`shimx64.efi` or `shimaa64.efi`),
  which loads Ubuntu's signed GRUB of the same architecture. GRUB reads a fixed `grub.cfg` from `hostkeeper-pxe`, the
  same for every machine, whose one command loads the menu of the machine's MAC: `configfile` of a path naming GRUB's
  CPU (`${grub_cpu}`) and MAC (`${net_default_mac}`) **[V]**: that Ubuntu's signed network GRUB of 24.04, for both
  architectures, sets both and reads its `grub.cfg` from the TFTP server's `grub/` directory. `hostkeeper-pxe`
  generates that menu for any well-formed MAC and either architecture, at the moment GRUB asks for it, from the answer
  set (section 3). The fixed `grub.cfg` is `configfile ${prefix}/hostkeeper/${grub_cpu}/${net_default_mac}`, so the
  menus sit beside `grub.cfg` wherever GRUB's `$prefix` puts it, and a MAC is read in any spelling the inventory reads
  (**Built**: `crates/hostkeeper-pxe-seed/src/menu.rs`, which also chooses the menu; `hostkeeper-pxe serve` gives it the
  answer set):
  - **For a MAC in the answer set,** a single install entry and no menu wait. It loads the kernel and initrd of the
    install's platform (section 3) with `ip=dhcp`, `url=` set to that platform's ISO on `hostkeeper-pxe`, `autoinstall`,
    and `ds=nocloud;s=http://<pxe>/seed/<mac>/`. GRUB loads the kernel and initrd by HTTP, from the device
    `(http,<address>)`; an HTTP port other than 80 is written `(http,<address>:<port>)`, which needs a GRUB that reads a
    port there **[V]** for Ubuntu's signed GRUB of 24.04, so the default port, 80, names none.
    The architecture GRUB names in the path is checked against the record's
    platform, when the record has one; a disagreement gets the local-disk menu instead, and is logged (section 3). This
    is the netboot path Canonical documents for 24.04 live-server **[V]**, and it is why section 6 can leave Secure Boot
    as it is. The semicolon of `ds=nocloud;s=...` is escaped in the menu, where it would otherwise end the command, and
    a golden pins that. The live system fetches the whole ISO into memory, so a machine needs several GiB of RAM **[V]:
    the figure, and that every farm server has it**.
  - **For every other MAC,** a single entry, with no menu wait, that boots the local disk: GRUB's `exit`, which hands
    control back to the firmware to try its next boot option **[V]** on each board model. If a board stops at its own
    boot menu instead, the entry chainloads the disk's EFI boot loader on that board model, a change to the menu
    generator (item N6) made before router mode is used on it; the runbook's first boot of each board model checks which
    (item N12). In proxy mode only a MAC in the answer set reaches GRUB at all, so this entry serves a MAC that left the
    set during the run.
- **The install is offline.** The seed turns off everything that would reach the internet: the installer's own update
  (`refresh-installer: update: false`), the apt mirror and security pockets (`apt: fallback: offline-install`, with no
  mirror), updates during install, and geoip. It installs from the ISO's package pool only. "No mirror" is written as
  one mirror candidate, `http://<pxe>/no-mirror/`, a path `hostkeeper-pxe` never serves, so whether or not the
  installer would take an empty candidate list, no default mirror on the internet is a candidate. Two facts here are
  **[V]**: that the 24.04 installer, finding its one candidate refused, takes the offline install rather than
  stopping; and that an offline install fetches nothing from the security pocket, so that `updates` (whose values are
  `security` and `all`, with no "off") reaches nothing. The seed names no `updates` key and no `security` mirror.
  The farm LAN does have a route out, but a fetch that depends on it would make the install depend on it; in the
  end-to-end test (section 11) there is no route out at all, so a fetch that needed one fails there.
- **The seed** is generated by `hostkeeper-pxe` from the inventory record, the platform of the install (section 3) and
  its flags. Each output is a pure function of its inputs, and goldens pin it (section 11).
  - The host name is the record's host id.
  - Storage: the disk is chosen by `--install-disk` as an autoinstall match, in the layout `direct`. The default is the
    smallest SSD (`ssd: true`, a disk the kernel reports as not rotational); `serial=<glob>`, `model=<glob>` and
    `path=<glob>` match on that field instead. Every match also takes the smallest of the disks it matches, so a glob
    that matches several still writes one. Only that disk is written. This is Question 5, answered (a). That the
    default leaves out removable disks is **[V]**: the 24.04 match has no key for it, so a removable disk that reports
    itself not rotational and is the smallest would be taken; the runbook's board check (item N12) lists each board
    model's disks.
  - One local operator account, `operator`, with its password locked, the SSH public keys given by
    `--ssh-authorized-keys` (a file of public keys; a private key block in it is refused, and so is a file with none),
    and sudo without a password, which is the only way a locked password can run as root (a reinstall's `efibootmgr
    --bootnext`, section 8). The installer installs the SSH server, with password login off.
  - The hostkeeper deb of the install's platform (`amd64` or `arm64`, both **Built** by the artifacts workflow),
    installed into the target from `hostkeeper-pxe` by two fixed `late-commands`: fetch it to a path in the target
    (with `curl`, which the live-server system has **[V]**; without it the install fails, and nothing boots
    half-installed), then `dpkg -i` it in the target. The commands hold no logic. The deb declares no dependencies
    (packaging/linux/nfpm.yaml, **Built**), so it installs offline.
  - `/etc/hostkeeper/hostkeeper.toml`, root-owned and mode 0600. It is the daemon config given by `--daemon-config`.
    `hostkeeper-pxe` loads that file with the daemon's own loader (`crates/hostkeeper-config`, **Built**) before it
    writes a single seed, so a config the daemon would refuse is refused at the start of the run, not on 20 machines.
    The installed system's cloud-init gets it through the autoinstall `user-data` section, as base64, so it is written
    byte for byte. `cloud-init.service` runs
    before `network-online.target`, and `hostkeeper.service` runs after it, so the file is in place when the daemon
    starts. A daemon that finds no config is started again every second (hostkeeper-design.md section 4.1, unit
    `Restart=always` with no start limit), so this ordering is not load-bearing.
  - With `--serial-console <port>` (`ttyS<n>` or `ttyAMA<n>`, at 115200 baud), the installed kernel's `console=` names
    that serial port and its serial getty is enabled, so the BMC's serial console shows the login prompt (section 7).
    The kernel's console is a GRUB default the first boot's cloud-init writes and applies, so it takes effect from the
    second boot; the getty is started on the first.
  - Curtin's `reorder_uefi` is left at its default, which puts the installed system's entry first in the UEFI boot order
    after the install (section 1, **[V]**). The design's safety does not rest on it: a board that keeps network boot
    first still gets no install answer for an installed machine (section 3).
  - The installed system pins its cloud-init to the NoCloud seed and nothing else (`datasource_list: [ NoCloud, None
    ]`), in `/etc/cloud/cloud.cfg.d/99_hostkeeper.cfg`, whose name sorts after the installer's own `99-installer.cfg`,
    so the pin holds whatever list that file names. This is the farm's current node shape (below).
  - `meta-data` holds the instance id, `hostkeeper-<host id>`, and nothing else. The record's role is not in the
    seed: the coordinator gives it at approval, from the record.
  - **Built**: `crates/hostkeeper-pxe-seed/src/seed.rs`; the paths the menus and seeds name, which `hostkeeper-pxe
    serve` serves, are in its `layout.rs`.
- **The farm's current node shape is the reference.** The six existing nodes run Ubuntu 24.04. Their cloud-init is
  pinned to a NoCloud seed baked into a prebuilt root file system, and they have no installer logs. This was checked on
  2 of the 6 nodes; the other four are likely the same but were not checked. The recipe that built that root file
  system is not in this repository. It is being traced, and the seed generator's disk layout and package set follow it
  once it is known. A new machine installed this way looks to hostkeeper like an existing node: same OS, same seed
  mechanism, same daemon config.

**Alternatives considered.**

- **Write a prebuilt root file system image to the disk** (the existing nodes' recipe, made reproducible). This was
  rejected for v1. It needs our own installer in the initrd: partitioning, a streaming write, bootloader install, and
  fixing up the UUIDs. Autoinstall and curtin already do all of that and are Canonical's to maintain. Once the node
  recipe is found, its contents become the seed's package set and layout, not a second installer.
- **A diskless (live) image that never installs.** This was rejected. A farm node keeps state on its disk: packages
  under `/var/lib/hostkeeper`, step records, service data. It must boot without the boot server. A diskless node would
  depend on `hostkeeper-pxe` running all the time, which is the opposite of section 3.
- **Our own iPXE build as the network boot loader.** This was rejected for v1 (section 6): it is not signed for Secure
  Boot, so every machine would need Secure Boot turned off. It is the path to a fully verified chain later.

## 5. The trust chain, with no secret in the image

The image carries only public facts: the coordinator's LAN address, `ca_sha256`, `release_channel` and
`trusted_signers` (hostkeeper-design.md section 4.1). Everything a host proves later rests on a key it makes itself.

1. **Boot files are verified before they are served.** Each hostkeeper release carries one `netboot.json` with a boot
   set for each of `linux-x86_64` and `linux-arm64`. It lists, for every file of each set, where it comes from, the
   name it is served as, the size and the SHA-256: the live-server ISO of that architecture (served under its own file
   name), and the shim (served as `hostkeeper/shimx64.efi` or `hostkeeper/shimaa64.efi`, section 3), the signed
   network GRUB (served as `hostkeeper/grubx64.efi` or `hostkeeper/grubaa64.efi`), the kernel (`vmlinuz-<platform>`)
   and the initrd (`initrd-<platform>`) that match it. The ISO is
   pinned by the URL on Canonical's release site, and its pin is checked against Canonical's signed `SHA256SUMS` when
   it is changed. The kernel and initrd are taken from that ISO, and the shim and GRUB from Canonical's signed packages
   of the same release, each pinned by its own SHA-256 once taken out. **Built**: the pins are
   `packaging/netboot/pins.toml`, and `cargo xtask release netboot` renders them into `netboot.json`, refusing pins
   that miss a platform, a file or a key or hold one of the wrong form. What was checked when the pins were taken
   (`packaging/netboot/README.md` has the detail and how to check a changed pin):
   - The arm64 ISO is not beside the amd64 one. The amd64 ISO is on `releases.ubuntu.com/24.04/`, the arm64 ISO on
     `cdimage.ubuntu.com/ubuntu/releases/24.04/release/`, and each is listed in the `SHA256SUMS` of its own directory,
     which Canonical signs in `SHA256SUMS.gpg` with its CD image key.
   - The four smaller files are inside the ISO, so its checksum covers them. The kernel and the initrd are the ISO's
     `casper/vmlinuz` and `casper/initrd`, those of its default boot entry. The shim is
     `usr/lib/shim/shim<x64|aa64>.efi.signed.latest` of the `shim-signed` deb in the ISO's pool, byte for byte the
     ISO's own first boot file. The network GRUB is
     `usr/lib/grub/<x86_64|arm64>-efi-signed/grubnet<x64|aa64>.efi.signed` of the `grub-efi-<amd64|arm64>-signed` deb
     in the pool; the ISO's own `grub<x64|aa64>.efi` is the CD GRUB, not this one. The two debs are the files the
     Ubuntu archive's signed package indexes list, with the same SHA-256.
   - Canonical's `netboot/` directories and `-netboot-` tarballs are in no signed checksum, so they are not a source.
     The arm64 kernel and initrd there are not those of the arm64 ISO, whose live system needs its own kernel.
   `netboot.json` is attested by the release workflow, like every release asset, with a bundle of its own
   (`netboot.json.sigstore.json`, `.github/workflows/artifacts.yml`, **Built**).
   - `hostkeeper-pxe fetch` downloads the boot set and the hostkeeper deb of each platform into a new directory: both
     platforms by default, or those named by `--platform` (repeatable), so a farm of one architecture does not keep an
     ISO it never serves. It verifies
     `netboot.json`'s attestation and the deb's attestation against the operator's `trusted_signers`, with the same
     verifier the daemon uses for packages (hostkeeper-design.md section 5.5). It checks every file's size and
     SHA-256 against `netboot.json` in one pass while writing it, and writes a seal. **Built**:
     `crates/hostkeeper-pxe` (item N8). Its flags: `--release`, the `https://` directory of the release's assets
     (a GitHub release download, whose redirects to `https://` it follows); `--dir`, the new directory;
     `--daemon-config`, loaded with the daemon's loader, whose `[trusted_signers]` is the signer and whose
     `bounds.attestation_timeout_secs` and `bounds.max_package_bytes` bound each check and each deb; `--platform`;
     and `--ca-bundle`, `--gh` and `--trusted-root`, by default where the hostkeeper deb puts them. In order: it
     downloads `netboot.json` and its bundle and checks the attestation before it reads a pin; downloads and checks
     each platform's deb (`hostkeeper_<version>_<arch>.deb` of the version `netboot.json` names), so a bad release
     is refused before an ISO is downloaded; downloads each ISO, stopped one byte past its pinned size; and only
     once the ISO matches its pin takes the kernel and initrd out of it, and the shim and GRUB out of the debs of
     its pool, by their Rock Ridge paths (`data.tar.xz` for `shim-signed`, `data.tar.zst` for `grub2-signed`, as
     the 24.04.5 ISOs carry them). Each file is written under its served name, the deb as
     `deb/hostkeeper_<arch>.deb`, and the seal, `seal.json`, last: every file with its size and SHA-256, and the
     commit and ref each attestation confirmed (`crates/hostkeeper-pxe/src/seal.rs`). On any refusal the directory
     is removed.
   - `hostkeeper-pxe serve` checks the seal at start and refuses to start on any difference. A tampered asset, a
     stale download or a mixed set is therefore never served. **Built**: `crates/hostkeeper-pxe/src/serve/sealed.rs`
     (item N9), which keeps each checked file open and serves it from that descriptor.
2. **The machine fetches the files over the farm LAN**, by TFTP and plain HTTP. With Secure Boot on, the firmware
   checks shim, shim checks GRUB, and GRUB checks the kernel, each against Canonical's signature. Nothing on the
   machine checks `grub.cfg`, the initrd, the ISO or the seed. An attacker who can answer DHCP on the farm LAN during
   the window, or can intercept its traffic, can install a system of their choosing on the new machine. Section 6 and
   Question 2 deal with this.
3. **The daemon makes a P-256 key** on the installed disk, writes it durably, and checks the coordinator's CA against
   `ca_sha256` before it sends anything (`crates/hostkeeper-enroll`, **Built**). It then posts `{csr, claims, image}`,
   and the request id is the SHA-256 of its public key (**Built**). A coordinator whose CA does not match gets nothing
   (`UntrustedCoordinator`, **Built**).
4. **Approval is by key hash**, compared whole (**Built**), after the operator has read the same hash on the machine's
   console (section 7). The certificate is a 7-day host certificate. Renewal works as today (**Built**).

**What the trusted signers protect.** Every package the coordinator orders after enrollment is verified on the host
against `trusted_signers` (**Built**). The boot set and the hostkeeper deb are verified by `hostkeeper-pxe fetch`
against the same signers, before they are served. Nothing on the new machine checks the deb again before `dpkg`
installs it: the deb is on the unverified hop between `hostkeeper-pxe` and the new machine (point 2), with the rest of
the boot set.

**How the release's signer is protected.** `trusted_signers` names hostkeeper's release workflow, on its refs, on a
hosted runner (hostkeeper-design.md section 4.1). There is no long-lived signing key: the attestation is keyless, by
the workflow's OIDC identity, and the workflow rules of this repository (rule B10, the workflow lint) limit which jobs
can obtain it. No key exists for an image build to protect, and none is added here.

**Alternative considered: a signed iPXE script.** Here, iPXE carries a code-signing certificate built into it,
`imgtrust --permanent` makes it refuse any unsigned image, and a signed script lists the kernel, initrd and image. This
was rejected for v1:

- It needs a long-lived code-signing private key that every release signs with, and today there is none (above).
- iPXE checks signatures, not digests, so "digests in the signed script" still means our own signing key **[V]**.
- Without Secure Boot, iPXE itself is the unverified first file, so an attacker on the LAN replaces iPXE and its
  checks together. It protects only together with Secure Boot keys of the farm's own (section 6, later).

## 6. Secure Boot

**Recommendation: leave Secure Boot as each machine has it.** The boot chain is Ubuntu's shim, GRUB and kernel, signed
through the Microsoft UEFI CA that server firmware trusts by default, so the install works with Secure Boot on or off
**[V]**. Nothing of ours is signed for Secure Boot, and nothing needs to be. The installed system boots the same signed
chain from disk. The arm64 chain is the same in kind: the pinned shim of each architecture carries one signature, by
the Microsoft Corporation UEFI CA 2011 (checked when the pins were taken, section 5), and whether a given board trusts
that CA by default, or ships with Secure Boot off, is **[V]** per board model. The install works either way, as on
x86-64.

**What it protects, and what it does not.** With Secure Boot on, a LAN attacker cannot make the machine run a boot
loader or kernel Canonical did not sign. They can still change the kernel command line, the initrd and the ISO (point
2 of section 5), and so still control the install. With Secure Boot off, they control everything from the first file.
Either way, in v1 the farm LAN must be free of an active attacker while `hostkeeper-pxe` runs. Q15 of
hostkeeper-design.md does not rely on a trusted network: it limits automatic approval to a record the operator opened
for a window, and keeps every other approval a person's click, because a MAC is only a claim. That defence does not
reach this case, because an attacker who controls the install controls the key hash the
console shows too. The residual risk is stated here as a decision for the CEO (Question 2), not hidden.

**Later: a fully verified chain.** Enroll a key of the farm's own in each machine's Secure Boot `db` (by the BMC, once
BMC control exists, section 10). Sign our iPXE with that key. Have iPXE check every following file (section 5's
alternative), including an image whose digest it checks. With a TPM claim recorded beside the MACs (hostkeeper-design
section 12.1), this is also what would let Q15 move to (b).

## 7. LAN enrollment

**Before N2** (**Built**). The coordinator had two listeners: the heartbeat listener (`--heartbeat-listen`) and the
private listener (`--private-listen`). Enrollment has no listener of its own. `Enroll` and `Poll` are calls of
`hostkeeper.agent.v1.Coordinator` on the heartbeat listener, which with `--enrollment-dir` lets a TLS client with no
certificate finish the handshake. `Heartbeat` and `Renew` refuse such a caller (`crates/hostkeeper-coordinator/src/
listen.rs`, `grpc/mod.rs`). On the farm the heartbeat listener is bound to the tailnet address, and a bare machine has
no tailscale.

**Built: `--lan-listen <address:port>`** (`crates/hostkeeper-coordinator/src/config.rs`, test
`crates/hostkeeper-coordinator/tests/lan_listen.rs`), a third listener. It needs `--enrollment-dir`. It is a second
listener of the heartbeat surface, with the heartbeat listener's TLS source, so it serves the agent service and nothing
else (Question 1 answered (a)):

- `Enroll` and `Poll` to a caller with no certificate.
- `Heartbeat` and `Renew` to a host certificate only, with exactly the checks of the heartbeat listener. The JSON
  heartbeat route (`POST /v1/heartbeat`) is served too, because a daemon's first heartbeat goes there before it learns
  that the coordinator speaks gRPC (`crates/hostkeeper-link/src/negotiated.rs`).
- Never the operator API, the JSON gateway or the UI. A gRPC call of the operator API on this listener is refused
  `UNIMPLEMENTED`, as on the heartbeat listener (`grpc/mod.rs`); a JSON gateway call, a JSON route of the private
  listener and a path under `/ui/` are answered 404 `not_found`, as there (`http.rs`).

The address must be one interface's, and a wildcard address is refused at start, as for `--private-listen`. The start
line says where it is bound (`lan=<address:port>`, after `control=`). In the compose deployment
([coordinator-storage.md](coordinator-storage.md) section 9) the coordinator's container uses the host's network, so
this is the host's own farm-LAN address, and its enrollment store is the database's. The daemon config of a
network-installed host names this listener as `coordinator`. Existing hosts keep the tailnet address. One coordinator
serves both groups, with one CA and one enrollment store.

**The server certificate's names** (**Built**, `crates/hostkeeper-coordinator/src/enrollment/setup.rs`). Before N2 the
coordinator's server certificate carried DNS names only (`--server-name`), and it was made again only when its file was
missing or a third of its life was left. Two changes came with the flag:

- The IP address of `--lan-listen` is added to the certificate as an IP SAN, after the DNS names (an IPv4 address
  written as IPv6 is named as IPv4). A bare machine then dials the coordinator by address, and needs neither a name in
  the router's DNS nor a hosts file. `--server-name` stays for names, and at least one is still required.
- The certificate is made again at start, and at the reload tick, whenever its names differ from the flags: a DNS name
  or the address added, removed or changed. The order of the names is not compared, so reordering `--server-name`
  makes no new certificate.

**The threat.** The enrollment port takes a request from anyone who can reach it, with no certificate (Q15 of
hostkeeper-design.md). On the farm LAN that is any machine on the LAN. A data MAC is in every frame the machine sends.
So whoever posts a recorded machine's MACs before the machine first boots gets a request that shows `matched`. Approval
therefore stays a person's click, by key hash, and the approval screen is designed to make the person check the right
thing:

- **Show the claims that are hard to copy, beside the record.** The DMI product UUID, the DMI serial, and the BMC MAC
  read in-band from the BMC (all **Built** as claims) are not on the data network. The serial and the BMC MAC are on
  the chassis label, and the product UUID is in the firmware. Someone who learned a data MAC from ARP does not have
  them. The UI's approval screen puts each beside the record's value: the record's serial and BMC MAC today, and its
  product UUID if the operator recorded one (a later, additive record field, section 10). Only the BMC MAC takes part
  in the Linux match (and only when both sides have one); the serial and the UUID are for the person to compare. A
  value the record does not hold is shown as "not recorded", and a claim the request leaves out as "not claimed";
  neither is shown as agreement. A request that leaves out its BMC MAC still matches on a data MAC (**Built**), so
  "not claimed" beside a recorded BMC MAC is shown as a warning. **Built**: `PendingEnrollment` carries the matched
  record's serial and BMC MAC (`record_serial`, `record_bmc_mac`) for the screen to show.
- **Say that "matched" is not proof.** Beside the match line of a matched Linux request, the screen says (**Built**):
  "Matched means this request claims a data MAC of the record and does not contradict its BMC MAC. The machine wrote
  the claims itself, and anyone who read the label or the network can write the same. Approve only the key hash shown
  on this machine's console."
- **Show the key hash on the machine's console.** This is the one check an attacker elsewhere on the LAN cannot fake.
  After each step of its enrollment loop, while its text changes, the daemon writes the request's state, the
  coordinator it posts to, and the key hash to `/run/hostkeeper/enrollment.issue`. The deb, the rpm and the boot-image
  overlay ship `/etc/issue.d/hostkeeper.issue` as a link to it, so the login prompt shows it. **[V]**: that agetty on
  24.04 reads `issue.d` and follows the link, and that it shows a file made after the prompt was drawn only when the
  prompt is drawn again; so the daemon asks the gettys to redraw (`/sbin/agetty --reload`) each time it writes the
  file, which changes nothing where agetty would have read it anyway. The text is printable ASCII, a backslash
  (agetty's escape character) written as `?`, so it reads the same whatever agetty does with escapes. The state is
  `pending approval`, `approved, enrolled as host:<id>`, `nothing sent` for an untrusted coordinator (without the hash
  that CA presents, so no second hash stands beside the key's), `denied: <reason>`, or `error: <why>` for a coordinator
  that cannot be reached or any other failure (**Built**: `crates/hostkeeper-daemon/src/console.rs`, the link in
  `packaging/linux/nfpm.yaml` and `packaging/linux/overlay/`; exact texts in `crates/hostkeeper-daemon/tests/console.rs`). Which consoles show it: the local console (tty1) always; the BMC's serial console only when the
  seed sets the installed kernel's `console=` to that port and enables its serial getty, which `--serial-console
  <ttyS port>` does. The operator doc's check with `openssl` (docs/operations/enrollment.md, "Check the key hash")
  stays as the way to read it over SSH, for a machine with neither.
- **Refuse a contested record.** Two pending requests that match one record both show `matched`, with that record's
  host id, and `Enrollment.List` marks each of them contested, naming the others (`PendingEnrollment.contested_by`,
  **Built**). `Enrollment.Approve` refuses `record_contested` while another pending request matches the same record,
  with or without `replace`, until the operator rejects the others (**Built**). An attacker who posted first therefore
  cannot be approved by a hurried click on the first `matched` row; the operator has to decide which machine is real,
  by the console's key hash. This is additive: a new field on `PendingEnrollment` and a new refusal value. A request
  that matches several records contests the requests of each.

**Q15 for Linux in v1.** None of the claims above approves anything by itself. The one automatic approval is an exact,
uncontested match of an open, unused record that names a role: the coordinator approves it within the record's window,
once. Any other match only fills in the form.

## 8. Reinstall of a known host

The host keeps its host id, role and services. Its disk is written anew, and it gets a new key.

1. The operator starts `hostkeeper-pxe` with `--reinstall <host id>` (repeatable). Only for the records of those hosts
   does it set aside the rule of section 3 that a record with a host or a pending request is left out of the answer
   set. This is a named opt-in, so a reinstall is never an accident of the boot order. The rest of the answer set's
   rule still applies: the record's platform must agree with the firmware's, and the MAC leaves the set when its
   install starts.
2. The machine is booted from the network, and installs as in section 4. On a board that keeps network boot first, a
   reboot does it. Where curtin's `reorder_uefi` put the disk first (section 1), the operator picks network boot once:
   in the firmware's boot menu over the BMC's remote console, or with `efibootmgr --bootnext` on the running host before
   its reboot. Either is a step of the reinstall, not of a new machine. Only the disk matched by `--install-disk` is
   written. Other disks, for example a storage node's data disks, are not touched by the installer, and what the host's
   services find on them is the services' business.
3. The new daemon makes a new key and posts a request. Its MACs are bound to the enrolled host, so the request is
   `duplicate_claim`, pending with an alert (**Built**). It matches the host's record.
4. The operator compares the console's key hash and approves with `--replace true` (**Built**). The host row is written
   anew with the new key; its first-heartbeat time is cleared, and any deny row of the host id becomes `cleared`
   (`crates/hostkeeper-enrollment/src/enroll/store.rs`, **Built**).
5. The coordinator orders an `install` of each assigned service, because the new host reports none installed
   (**Built**, B23).

**The old certificate.** No CRL is published, and none is needed: the coordinator is the only party that accepts a
host certificate. After the `--replace` approval, the old certificate still chains to the CA until it expires (7 days
at most). But the coordinator maps a certificate to a host only through the key that host id is approved for now, so
every heartbeat with the old certificate is refused `forbidden`, and its `Renew` is refused `renewal_not_allowed`, at
once (`host_role_for`, **Built**, test `a_host_role_is_read_only_for_the_key_the_host_holds`). Nothing has to be done
by hand. A machine that is retired instead of reinstalled is denied by host id, as today (`Hosts.Retire`, **Built**).

## 9. Failure modes

| Case | Outcome |
|---|---|
| A recorded machine boots while `hostkeeper-pxe` is not running, proxy mode | No proxy offer. The firmware tries its next boot device, and nothing is installed. |
| A machine boots network first while `hostkeeper-pxe` is not running, router mode | Its TFTP request gets no reply. The firmware tries its next boot device after its own wait **[V]**, and nothing is installed (section 3). |
| An enrolled host boots network first | Not in the answer set (section 3): no answer in proxy mode, the local-disk menu in router mode, so it boots from disk. With `--reinstall` naming it, it is reinstalled. |
| A retired host whose record is still in the inventory | Not in the answer set: its record's `host_held` is `RETIRED` (or `DENIED` until the retirement is final), read from the enrollment store, also after a coordinator restart (section 3). |
| An approved host that has not sent a heartbeat since the coordinator started | Not in the answer set: its record's `host_held` is `APPROVED`, though `Hosts.List` does not list it (section 3). |
| A coordinator of a release before `host_held`, or without enrollment | Every record's `host_held` is `UNSPECIFIED`, so none is in the answer set: no proxy offer, or the local-disk menu, until the coordinator says `NONE` (section 3). |
| An installed machine waiting for approval reboots, or `hostkeeper-pxe` is restarted meanwhile | Not in the answer set: a pending request matches its record (section 3). |
| A machine reboots after its install, with network boot still first | Its MAC left the answer set at its seed fetch, so it boots the disk; after a restart of `hostkeeper-pxe`, a pending request or its host keeps it out. |
| Someone posts a request matching a record before the machine boots | That machine is not installed until the operator rejects the request; the record shows a pending request that no installing machine made. |
| An unrecorded machine | Not in the answer set: no answer in proxy mode, the local-disk menu in router mode. |
| The record's platform disagrees with the architecture the firmware or GRUB states | Not in the answer set; `hostkeeper-pxe` logs the host id and both platforms (section 3). |
| A record with no platform, router mode | Not in the answer set, and logged. In proxy mode it takes the firmware's architecture. |
| The record's platform has no boot set in the sealed directory | Not in the answer set, and logged; `serve` names at start the platforms it has. |
| A firmware of another architecture (legacy BIOS, 32-bit ARM) | No proxy offer. In router mode it cannot run `hostkeeper/boot.efi`, and tries its next boot device **[V]**. |
| Router mode, a TFTP read of `hostkeeper/boot.efi` from an address whose DHCPREQUEST `hostkeeper-pxe` did not see | A TFTP error, logged with the address; the firmware tries its next boot device. |
| Router mode, a board whose firmware stops at its own menu after GRUB's `exit` | The machine waits at that menu. The runbook's board check finds this before router mode is used on that board (section 4). |
| Proxy mode, the network's DHCP guarding does not trust the `hostkeeper-pxe` host | The proxy offers are dropped, and the machine boots its disk. `hostkeeper-pxe` logs each offer it sent, and no TFTP read follows. The runbook trusts the host or uses router mode (section 3). |
| Proxy mode, the router also offers a boot file | Depends on the firmware (Question 4). The runbook checks this first. |
| A board that does not try network boot before its disk | It boots its disk, empty or not, and nothing is installed. Someone picks network boot in its firmware once (section 1). |
| The boot set on disk differs from its seal, or was never verified | `hostkeeper-pxe serve` refuses to start, naming the file. |
| `netboot.json` or the deb is attested by another signer, workflow or ref | `hostkeeper-pxe fetch` refuses, naming the attestation, and keeps nothing. |
| The daemon config given to `hostkeeper-pxe` does not load | Refused at start with the daemon loader's error, before any seed is written. |
| The install fails mid-way | The machine is not installed again in this run. Its console shows the installer's error. The operator restarts `hostkeeper-pxe` to retry. |
| The installed host cannot reach the LAN listener | The daemon retries, as today, and the console's issue line shows the coordinator it is trying. No request appears. |
| The coordinator's CA is not the pinned one | Nothing is sent (`UntrustedCoordinator`, **Built**), and the issue line says so. |
| More than `--max-pending-enrollments` requests | `pending_full`, retried every 5 s (**Built**). |
| Two pending requests match one record | Both `contested`, and approval is refused until all but one are rejected (section 7). |
| `hostkeeper-pxe` cannot read the coordinator when a machine boots | The answer set is empty for that boot: no proxy offer, or the local-disk menu, and the failure is logged (section 3). |
| A machine is on the storage network only | No DHCP to give it an address, so it cannot be installed (section 3). |

## 10. v1 scope, and later

**In v1:** `linux-x86_64` and `linux-arm64` machines with UEFI firmware, new machines only, booted from a NIC on the
farm LAN, installed with Ubuntu 24.04, in router mode or proxy mode (section 3). The expected setup is a board that
tries network boot before its disk; a person picks network boot once on a board that does not.

**Later, not designed here:**

- Legacy BIOS PXE, and 32-bit ARM.
- UEFI HTTP boot in router mode. The router names a TFTP boot file, and an HTTP boot client needs an `http://` URL and
  the `HTTPClient` vendor class in its answer, which only proxy mode gives (section 3).
- **BMC power and boot-device control.** Today the BMC MAC is recorded and nothing else (hostkeeper-design.md section
  12.1). With it, the BMC sets a one-time network boot and powers the machine on: the third step of a board that does
  not boot network first (section 1), and the one-time network boot of a reinstall (section 8), go away.
- The fully verified boot chain with the farm's own Secure Boot keys (section 6).
- A TPM claim, and with it Q15 (b).
- An optional product UUID field on the inventory record (section 7).
- A full DHCP server or relay for segments with no DHCP, such as the storage network, or a separate provisioning VLAN.
- Approving many new machines at once.
- Reinstalling the six existing nodes this way. They keep the adopt path, and moving them is its own decision.

## 11. Tests, and the measurement

Every crate these items add or touch is measured by the 99% line and branch gate (rule T1) in the pull request's own
CI. The end-to-end jobs count toward no gate, so nothing relies on it for coverage. Tests assert behaviour, never
time (rule T10), are Rust (rule T11), and use the repository's scratch helpers and fakes (rule T12).

**Unit and integration tests, by the item that builds them (section 12):**

- Boot sets (N5, **Built**, `crates/xtask/src/release/netboot/tests.rs`): the committed pins render a boot set for
  each Linux platform, with the served names above; the asset of fixed pins is a golden, by exact equality; and pins
  that miss a platform, a file or a key, carry an unknown key, a size that is not a whole number above 0, a digest that
  is not 64 lowercase hex digits, a URL that is not `https://`, a path that is not relative or climbs out, or an ISO or
  deb of the other architecture are refused, each naming the key, all at once, with nothing written. The workflow lint
  refuses an `artifacts.yml` that does not attest `netboot.json` or copy its bundle.
- proxyDHCP packets (N7, pure: a packet, the answer set and the platforms that have a boot set in, a packet or nothing
  out):
  - For a MAC in the set, a DISCOVER with a `PXEClient` or `HTTPClient` vendor class gets an offer with no address
    (`yiaddr` 0), the echoed vendor class, option 43 and the boot file (a TFTP name or an `http://` URL by class), for
    each architecture value: 7, 9 and 16 name the x86-64 shim, 11 and 19 the arm64 shim. This catches a server that
    hands out addresses and fights the router, and one that sends an arm64 machine the x86-64 shim.
  - A DHCPREQUEST to UDP 4011 for a MAC in the set gets an ACK with the same boot file; one for a MAC not in it gets
    nothing.
  - Nothing is sent for: a MAC not in the set, a DHCP message with no PXE vendor class (the installer's own DHCP and
    every ordinary client), the architectures 0 (BIOS) and 10 (32-bit ARM) and an unlisted value, an architecture with
    no boot set, a REQUEST naming another server, a packet a relay forwarded (`giaddr` set), and truncated, oversized
    or malformed packets.
- Router-mode learning (N7, pure: a packet in, an address, a MAC and an architecture or nothing out): a selecting
  DHCPREQUEST yields the address of its option 50, its `chaddr` and its option 93; a REQUEST with no option 93 takes the
  architecture of an earlier DISCOVER with the same `chaddr`, and with no such DISCOVER yields the address and MAC with
  no architecture; a REQUEST with no option 50, and a malformed packet, yield nothing. A REQUEST with option 50 and no
  server identifier (the INIT-REBOOT state of RFC 2131) is learned as a selecting one is. A run keeps at most 4096
  addresses and 4096 MACs, and drops the one learned longest ago first. In router mode no packet is ever sent, which a
  test asserts for every input above.
- TFTP packets (N7, pure): only names of the file set are served; a write request, a path with `..`, an absolute path
  outside the served tree and an unknown name are refused. One leading `/` names the root of the served tree, as a
  TFTP server confined to its directory reads it, so a GRUB that asks for `/grub/grub.cfg` gets `grub/grub.cfg`; once
  that one `/` is removed, a name outside the set (such as `/etc/passwd`) is refused as unknown, and a name that still
  starts with `/` (such as `//grub/grub.cfg`) is refused as unsafe. `hostkeeper/boot.efi` is the x86-64 or the arm64
  shim by the architecture learned for the reader's address, or by the record's platform of its MAC when it stated no
  architecture, and a TFTP error for an address with no MAC, a MAC with neither, or an architecture with no boot set.
  Block numbering and the final short block follow RFC 1350, and option negotiation RFC 2347.
- Boot menu (N6, goldens by exact equality): the fixed `grub.cfg`; the install menu for each platform; the local-disk
  menu. The menu for a MAC is the install menu exactly when the MAC is in the answer set and the architecture in the
  path agrees with the record's platform, and the local-disk menu for: a MAC not in the set, a MAC that is not
  well-formed, an architecture in the path that disagrees, and, in router mode, a record with no platform.
- The record's `host_held` (N9b, `crates/hostkeeper-coordinator/src/host_held_tests.rs`,
  `crates/hostkeeper-enrollment/tests/held.rs`, `crates/hostkeeper-proto/tests/additions_inventory.rs`): after a
  coordinator restart, `ListRecords` says `RETIRED` for a retired host's record, `DENIED` for a denied one whose
  retirement is not final, `APPROVED` for one approved and never heard from, and `NONE` only for an id no host holds,
  while `Hosts.List` lists none of them; `AddRecord` answers the same; a coordinator without enrollment says
  `UNSPECIFIED`; a deny or host file that does not decode is refused `store_failed` for the page that holds its
  record, not answered `NONE`; and a record written before the field reads `UNSPECIFIED`, not `NONE`.
- Boot-set verification (N8, **Built**: `crates/hostkeeper-pxe/src/fetch_tests.rs` and the reader tests beside it):
  `fetch` refuses each of these, naming it: a size mismatch, a digest mismatch, a missing file, a `netboot.json`
  attested by another organisation, workflow or ref, and a deb whose attestation does not verify. It fetches both
  platforms by default and only those `--platform` names when given. The verifier is the daemon's, given a fake
  attestation port, as the daemon's own tests do.
- Serving (N9, **Built**: `crates/hostkeeper-pxe/src/serve/tests/`). Every listen address and port is a flag, so the
  tests bind ephemeral loopback ports with real UDP and TCP sockets, and talk to a fake coordinator (the fake
  operator service pattern of `crates/hostkeeper-ctl/tests`, over TLS on loopback).
  - The answer set: a record whose `host_held` is not `NONE` (`APPROVED`, `DENIED`, `RETIRED` or `UNSPECIFIED`),
    that is enrolled, or with a pending request that matches it, is left out, unless `--reinstall` names its host
    id; a record is dropped from the set once its seed was fetched; a
    record whose platform disagrees with the firmware's, or has no boot set, is left out and logged; in router mode a
    record with no platform is left out, and in proxy mode it takes option 93's.
  - In each mode, for one recorded and one enrolled MAC: proxy mode answers only the recorded one, and router mode
    serves both the shim and the fixed `grub.cfg`, and the install menu only to the recorded one.
  - HTTP serves only sealed files and generated seeds, and refuses any other path and every method but GET and HEAD.
  - `serve` refuses to start on a seal mismatch, naming the file, and without `--mode`.
  - A failed read of the coordinator (no answer, a refused token, an answer that does not parse) gives the empty
    answer set for that answer in each mode, and the next answer reads again.
  - It drops privileges before it reads a packet (the user to drop to is a flag; the test passes its own).
- Seed generator (N6): goldens, compared by exact equality, for each platform (the deb of `amd64` and of `arm64`), for
  a record with and without a role, with one SSH key and several, and for each `--install-disk` form. It refuses a
  daemon config the loader refuses, and a private key in the SSH keys file. A test asserts that the seed is exactly
  the golden, with no field beyond the documented ones, so a token cannot slip in unnoticed.
- The record's platform (N14): `AddRecord` with each platform and with none, `ListRecords` showing it, and `AddRecord`
  refused `invalid_request` for a value that is not a Linux platform. Its store tests: an ops file written before the
  field reads unchanged, a record with no platform is written without the field (a golden of the ops file, byte for
  byte), and a record with one reads back the same. Across releases, in the pattern of
  `crates/hostkeeper-coordinator/src/ops_downgrade_tests.rs`: the reader of the release before, kept as it was, reads
  the ops file and halts file this release writes with platforms recorded, and this release reads the files the
  release before wrote.
- LAN listener (N2, **Built**: `crates/hostkeeper-coordinator/tests/lan_listen.rs` and
  `crates/hostkeeper-coordinator/src/enrollment/tests/server_names.rs`): `Enroll` and `Poll` work with no certificate.
  `Heartbeat` and `Renew` without one are refused. Each operator service, the JSON gateway and the UI are refused on it.
  A wildcard address is refused. The listener's IP is in the certificate's SANs, and a changed name list makes a new
  certificate.
- Console key hash (N3): exact content of the issue file for pending, approved and untrusted-coordinator states.
- Contested records (N4): `Approve` is refused `record_contested` while two pending requests match one record, and
  succeeds after the other is rejected. `List` names the contested requests both ways.
- Reinstall (N4, store level): the existing `a_host_role_is_read_only_for_the_key_the_host_holds` already proves that
  the old key names the host no more. N2 adds the coordinator-level test that a heartbeat with the old certificate,
  on the LAN listener, is refused `forbidden`
  (`an_enrolled_host_heartbeats_and_renews_on_the_lan_listener_until_it_is_replaced`, **Built**).

**The end-to-end proof (N10).** Rust integration tests in `crates/hostkeeper-privileged`, behind its feature
`privileged`, in new jobs of `privileged.yml`. That workflow runs only on a push to `main` and by hand, never on a pull
request (rule B9), and the workflow lint is extended to know the jobs and their runners.

- **x86-64, both modes (job `net-install`).** The test makes a Linux bridge with no route out, and a QEMU guest with
  OVMF UEFI firmware on it. The guest has an empty scratch disk, a fixed `-uuid` and an SMBIOS serial. Its NIC is first
  in QEMU's boot order, so, like the farm's boards, the guest tries network boot first at every boot, whatever curtin
  writes to the UEFI boot order **[V]**: that OVMF applies QEMU's boot order over the one in its variables.
- It runs, on the bridge, each in its own network namespace joined to the bridge by a veth pair with its own address,
  as the router and the boot server are separate boxes on a real LAN (so each binds UDP 67 in its own namespace; the
  namespaces are one reason the test needs root):
  - a minimal fake of the router's DHCP, inside the test, that gives addresses and, as each step sets it, either no
    boot file (proxy mode) or `hostkeeper-pxe`'s address and `hostkeeper/boot.efi` (router mode);
  - the real `hostkeeper-pxe`, with a boot set the job fetched by pinned URL and SHA-256 before the test;
  - the real coordinator, with `--lan-listen` on the bridge address.
- It waits on progress events, never on time. Each event is a line on the guest's serial console, read as a stream, a
  line of `hostkeeper-pxe`'s log, or an answer from `hostkeeper ctl`. Each wait ends on its event or on the guest's
  exit, and the job's `timeout-minutes` is the only bound.
- In order, it checks that:
  - in proxy mode: the fake DHCP saw the guest's DISCOVER, and `hostkeeper-pxe` sent its proxy offer;
  - the seed was fetched;
  - the reboot after the install got no proxy offer and booted the disk;
  - a pending request appears, `matched` to the record, with claims equal to the guest's UUID and serial;
  - the console line shows that request's key hash;
  - `Approve` by that hash succeeds;
  - the first heartbeat arrives;
  - an assigned fixture service reports installed.
  It then switches the fake DHCP and `hostkeeper-pxe` to router mode and reboots the enrolled guest: `hostkeeper-pxe`
  serves it the shim and its local-disk menu, and its heartbeat comes back from the same disk. Last, it reinstalls the
  guest in router mode with `--reinstall` and checks the install menu, the `duplicate_claim`, the `--replace`
  approval, and that the old certificate's heartbeat is refused. Then it retires the host (`Hosts.Retire`), restarts
  the coordinator, and network-boots the guest again without `--reinstall`: `hostkeeper-pxe` sends no answer in proxy
  mode and serves the local-disk menu in router mode, and the guest's disk is untouched.
- It needs `/dev/kvm` on the hosted `ubuntu-24.04` runner **[V]**. Without it, the job fails with that reason; it does
  not skip.

**arm64 in CI (job `net-install-arm64`).** Whether the end-to-end can boot an arm64 guest depends on hardware help.
QEMU can always emulate an arm64 guest, with the AAVMF UEFI firmware, by TCG on any runner. Without KVM, TCG
translates every instruction, and the full live-server install (the ISO fetched into memory, the live system, curtin)
may run for hours **[V]**, possibly past the limit of a hosted job. A hosted arm64 runner (`ubuntu-24.04-arm`, which
the artifacts workflow already uses to build the arm64 deb) would run the guest under KVM if it exposes `/dev/kvm`
**[V]**. The design holds either way, with two named tests:

- `arm64_installs_and_enrolls`: the x86-64 test's proxy-mode steps, through the fixture service, on the arm64 boot set.
  It runs on `ubuntu-24.04-arm`, under KVM, and fails with that reason when `/dev/kvm` is missing.
- `arm64_boots_to_seed_fetch`, the fallback: an arm64 guest under TCG on the hosted `ubuntu-24.04` runner, in router
  mode, until the seed fetch. It checks that the guest's REQUEST stated architecture 11, that `hostkeeper/boot.efi`
  was served as `hostkeeper/shimaa64.efi`, that GRUB asked for the arm64 menu of its MAC, and that the seed fetched names the
  `arm64` deb. That is every part of the boot chain that differs by platform. If **[V]** shows that even this does not
  fit a hosted job, it ends at the kernel's first request for the arm64 ISO, which still proves the shim, GRUB, the
  menu and the kernel.

Item N10 checks which of the two the hosted arm runner allows, puts that one test in the job, and says which in its pull
request; the job never chooses at run time, and never skips. The workflow lint lists the job's runner. With the
fallback, the full arm64 install, the deb and the enrollment are proven on the first real arm64 machine (N13), by the
runbook, and the unit tests above cover the platform choice in every component.

**The measurement for the white paper (N11).** The two numbers are the time from power-on to approved-and-serving, and
the number of manual steps.

- The time is computed from times the components already record. No test asserts it.
  - power-on: `hostkeeper-pxe`'s log of the first DISCOVER (proxy mode) or DHCPREQUEST (router mode) from the MAC;
  - install started: the seed fetch;
  - pending: `first_seen_unix`, **Built**;
  - approved: `enrolled_unix`, **Built**;
  - first heartbeat: `first_heartbeat_unix`, **Built**;
  - serving: the first assigned service reported installed and healthy.
- `cargo xtask netinstall-report` reads these and prints each interval, with the person's wait (pending to approved)
  apart from the machine's time. It is a pure function of its inputs, tested with fixed inputs, and reads no clock. An
  event that never happened is printed "not reached", never as zero.
- It runs in the end-to-end job, which keeps the report as an artifact, and the operator runs it once on the first
  real machine. The paper quotes the real machine and states the VM separately.
- The manual steps are counted from the numbered steps of the runbook (N12): two per machine (section 1), a third on a
  board that does not boot network first, and three per farm. The first real machine records them. For comparison, the
  steps of today's hand install of a node are counted from its current runbook at the same time. No count is quoted
  before then.

## 12. Tasks

One pull request each, in this order. "After" lists prerequisites. Sizes are those of hostkeeper-design.md section
12.2.

| # | Item | After | Size |
|---|---|---|---|
| N1 | This design, and the pointers from hostkeeper-design.md sections 2.2, 4.6 and 12.1 | none | S |
| N1b | This amendment: network boot first as the expected setup and two steps per machine, router mode beside proxy mode with the per-MAC boot menu, DHCP guarding, `linux-arm64` in v1, the record's platform, and arm64 in CI | N1 | S |
| N2 | Coordinator `--lan-listen` (agent service only, per Question 1); IP SAN of its address; server certificate made again when its names change; the tests of section 11 | N1 | M |
| N3 | Daemon: the console issue file of the pending request; the deb and the overlay ship the `issue.d` link | N1 | S |
| N4 | Enrollment: `contested_by`, `record_serial` and `record_bmc_mac` on `PendingEnrollment` and refusal `record_contested`; the UI's approval screen puts the hard-to-copy claims beside the record and adds the "matched is not proof" text | N1 | M |
| N5 | `packaging/netboot/pins.toml` and `netboot.json` with a boot set for each of `linux-x86_64` and `linux-arm64` (ISO, shim, signed network GRUB, kernel, initrd), built and attested in the artifacts workflow | N1b | M |
| N6 | Seed and menu generator: a pure crate turning a record, its platform, a daemon config and flags into the fixed `grub.cfg`, the install and local-disk menus of a MAC, autoinstall `user-data` and `meta-data`, with goldens for each platform | N1b | M |
| N7 | proxyDHCP, router-mode learning and TFTP packet handling: a pure crate, no sockets, no inventory; architectures 7, 9, 11, 16 and 19; the router-mode `hostkeeper/boot.efi` by learned architecture. **Built**: `crates/hostkeeper-pxe-packets` | N1b | M |
| N8 | `hostkeeper-pxe fetch`: download both platforms' boot sets and debs (or those `--platform` names), verify against the release attestation and `netboot.json`, seal | N5 | M |
| N9 | `hostkeeper-pxe serve`: `--mode` (proxy or router), sockets and privilege drop, the answer set from records (their `host_held`, N9b) and pending requests and its platform rule, the MAC leaving the set at its seed fetch, `--reinstall`, the per-MAC menus, HTTP and TFTP from the sealed directory, the seal check; released as a binary with no enabled unit | N6, N7, N8, N14 | M |
| N9b | `InventoryRecord.host_held`: what the enrollment store holds for the record's host id (`NONE`, `APPROVED`, `DENIED`, `RETIRED`; `UNSPECIFIED` without a store), read by host id in `Enrollment.ListRecords` and `Enrollment.AddRecord`; the tests of section 11 | N1b | S |
| N10 | Privileged end-to-end: the `net-install` job (x86-64, proxy mode then router mode) and the `net-install-arm64` job with the one arm64 test the hosted runners allow (section 11), and the workflow lint | N2, N3, N9 | M |
| N11 | `cargo xtask netinstall-report`, and the report artifact of N10 | N10 | S |
| N12 | Operator runbook `docs/operations/network-install.md`: choosing the mode; the UniFi "Network Boot" setting for router mode and DHCP guarding for proxy mode (both **[V]**); the router check of Question 4; the boot-order, local-disk and option 93 check of each board model (a capture of its DISCOVER and REQUEST); the per-machine steps; reinstall. **Built**: [docs/operations/network-install.md](../operations/network-install.md), whose `hostkeeper ctl` and `hostkeeper-pxe` lines a test parses with each program's parser | N9 | S |
| N13 | First real machine of each platform on the farm, by the runbook, recording the measurement. Done by hand by the operator, not in CI | N10, N12 | S |
| N14 | The record's optional platform: the field of `InventoryRecord` and `AddRecordRequest`, the platform kept behind the coordinator's store interface (in the halts file today), `add-record --platform`, the UI's Inventory page, `docs/operations/enrollment.md`, the runbook's note that router mode installs nothing yet (`docs/operations/network-install.md`), hostkeeper-design.md section 5.7 (the record's fields) and its table of stored files (the platforms in the halts file), and the tests of section 11 | N1b | S |

N2, N3, N4, N5, N6, N7 and N14 are independent and can be built in parallel. N10 checks a first install
(`an_assigned_package_installs_once_across_restarts_and_the_boot_pass_keeps_it`, privileged). The
later orders of an installed service, an update and an uninstall, are checked by the privileged
`a_new_desired_version_is_installed_over_the_installed_one` and
`an_uninstall_takes_the_package_its_file_and_its_unit_off_the_host`.

## 13. Questions for the CEO

Each question has options and a recommendation. Until it is answered, the design follows the recommendation.

**Question 1. How does a network-installed host reach the coordinator once approved?** Answered: (a), built in N2.
Its daemon config names one coordinator address, and the daemon both enrolls and heartbeats at that address
(hostkeeper-design.md section 4.1, **Built**). A bare machine has no tailscale.

- (a) **The LAN listener serves the whole agent service**: enrollment with no certificate, and heartbeat and renewal
  with a host certificate, exactly as the tailnet listener does. It never serves the operator API or the UI. The host
  needs nothing else.
- (b) **The LAN listener serves `Enroll` and `Poll` only.** Then the daemon config needs a second, additive key for
  the enrollment address, and the heartbeat stays on the tailnet. Each new host then needs someone to join it to the
  tailnet by hand after approval, which is a third manual step per machine (a fourth on a board that does not boot
  network first). The services ordered to it wait until
  then.
- (c) A tailnet auth key in the seed. This is rejected: it is a secret in the image.

Recommendation: (a). Heartbeat over the LAN is mutual TLS, with the same checks as over the tailnet, and the operator
API stays off the LAN.

**Question 2. May the farm LAN be trusted against an active attacker during an install window?** In v1 nothing on the
new machine verifies the initrd, the ISO or the seed (sections 5 and 6), so an attacker who can answer DHCP or
intercept traffic on the farm LAN while `hostkeeper-pxe` runs can install a system of their choosing. The machine's
console then shows that system's key hash, and the person approves it. The real hardware's claims are all correct.

- (a) **Accept it for v1**, with the files verified at `hostkeeper-pxe`, Canonical's signed chain under Secure Boot
  where it is on, `hostkeeper-pxe` running only for the window, and the console key-hash check.
- (b) Build the fully verified chain of section 6 first. That needs the farm's own Secure Boot keys enrolled in every
  machine, which needs BMC control or a person at each firmware screen.

Recommendation: (a) for v1, and (b) as the item after BMC control. The same LAN already serves every node its address
by unauthenticated DHCP, and an attacker on it can already disturb every node. Note that Q15 as built (a person's approval, or a record's one-time window) does not cover this
case: approval protects against a forged request, not against an install the attacker controls.

**Question 3. What credential does `hostkeeper-pxe` read the inventory with, and where does it run?** It needs the
records (each with its `host_held`) and pending requests (section 3), from the coordinator's private listener, and it must sit on the farm
LAN. The coordinator's tokens either operate or do not; there are no per-token grants (hostkeeper-design.md section
4.3).

- (a) An operator token from a root-only file. `hostkeeper-pxe` runs on the coordinator's own host, which then needs a
  NIC on the farm LAN; this is a property of that host, not of the coordinator, and the coordinator still serves no
  boot. It may instead run on another farm-LAN host that reaches the private listener, at the cost of a copy of the
  token there. What an attacker gains from that process: a full operator credential. It parses untrusted packets only
  after dropping privileges (section 3), but the token is in its memory.
- (b) No token: the operator passes the MACs as flags or a file. That is a second list, kept by hand, with the
  enrolled-host safety rule of section 3 left to the operator.
- (c) A read-only token class. This changes the token ruling.

Recommendation: (a), on the coordinator's host. The safety rule of section 3 needs the live state, and that host
already holds every operator secret, so no credential moves. If the CEO would rather not have a full credential in a
packet-parsing process, (c) is the fix, and it is small: one token flag for read-only calls.

**Question 4. Which segment, and how does the farm's router take part in network boot?** Recommendation: new machines
are installed from a NIC on the farm LAN, not the storage network (section 3). On the new lab's UniFi router, router
mode: the network's "Network Boot" names the `hostkeeper-pxe` host and `hostkeeper/boot.efi` **[V]**, and DHCP
guarding is left as it is. On a router that is not ours to set, proxy mode: the runbook's first step confirms that the
router offers no boot file name (options 66 and 67, or the `file` field), and, with DHCP guarding on, that the
`hostkeeper-pxe` host is a trusted DHCP server. If the router offers a boot file of its own, that is switched off on
the router once; it is not worked around in `hostkeeper-pxe`. Only the CEO can see and change the router's settings.

**Question 5. Which disk does the installer take by default?** The installer wipes the disk it matches on any machine
the operator recorded. Options:

- (a) The smallest non-removable SSD, with `--install-disk` to override.
- (b) No default: `--install-disk` is required.
- (c) Follow the existing nodes' layout once their recipe is known.

Recommendation: (a), changed to (c) when the recipe is found. A storage node's many data disks are larger than its
boot SSD, so "largest" would be the wrong default.
