# Network install of new Linux machines

Status: design, for review. This document makes network install of new Linux machines part of hostkeeper v1. It
replaces the non-goal of [hostkeeper-design.md](hostkeeper-design.md) section 2.2 ("No network boot service in
scope") and the first item of its section 12.1 ("the operator's own, no build items here"). Both now point here. Where
this document and hostkeeper-design.md disagree on network install, this document wins. Nothing here is built yet:
every statement about new code is **Designed**, and section 12 lists the build items.

The tags are those of hostkeeper-design.md. **Built** names code on `main` with a path. **Designed** means nothing
implements it yet. **[V]** marks a fact about software or hardware we do not own, which must be checked on the real
thing before the item that relies on it is built.

## 1. What changes for the operator

**Before.** A new Linux machine is installed by hand: someone writes Ubuntu to its disk, configures the network,
installs the hostkeeper deb, writes `/etc/hostkeeper/hostkeeper.toml`, and starts the daemon. Only then does it appear
as a pending enrollment request (hostkeeper-design.md section 5.7, docs/operations/enrollment.md "Bootstrap a Linux
node").

**After.** The operator:

1. Records the machine's MACs (its data MACs, and its BMC MAC and serial from the chassis label) as an inventory
   record. A Linux request is matched by its data MACs and BMC MAC; the serial is shown beside the claim for the
   person to compare, and takes no part in the match. This step is built: `hostkeeper ctl enrollment add-record`, or the UI's Inventory page.
2. Powers the machine on and picks network boot in the firmware's boot menu.

The machine then installs Ubuntu 24.04 and the hostkeeper deb, reboots, and its daemon posts an enrollment request.
That request shows up in the pending list, matched to the record. The machine's console shows the key hash of the
request.

3. The operator compares that key hash with the one in the UI, approves the request, and the coordinator installs the
   services assigned to the host or its role, as it does today.

That is three manual steps for each machine. Two things are done once per farm: the coordinator runs with a LAN
listener (section 7), and the operator runs `hostkeeper-pxe` on a host of the farm LAN while machines are being
installed (section 3).

**What does not change.**

- No Linux host is approved without a person. Q15 of hostkeeper-design.md stays at (a) in v1: an inventory match fills
  in the host id and role, and nothing more.
- The image carries no secret. The host's only trust root is still `hostkeeper.toml`, and its contents are as
  hostkeeper-design.md section 4.1 gives them.
- The six existing farm nodes are not reinstalled. They keep the bootstrap and adopt path they use today.
- Macs stay on MDM (hostkeeper-design.md section 5.9): Apple silicon cannot net-boot.
- No firmware, BMC or router setting is changed by hostkeeper.

## 2. The flow

```
operator          hostkeeper-pxe          new machine                    coordinator (LAN listener)
   | add-record ---------------------------------------------------------------> inventory
   | start ------> reads records, hosts, pending
   | power on, pick network boot ---------> firmware DHCP DISCOVER
   |                 (router's DHCP gives the address; hostkeeper-pxe's proxy offer gives the boot file)
   |              <- TFTP/HTTP: shim, GRUB, grub.cfg, kernel, initrd, ISO, seed
   |                                       autoinstall: disk, Ubuntu, hostkeeper deb, config
   |                                       reboot from disk
   |                                       daemon: P-256 key, claims ----> Enroll (no certificate)
   |                                       console shows key hash          pending, matched
   | compare key hash, approve ------------------------------------------------> approval by key hash
   |                                       Poll ----------------------------> 7-day certificate
   |                                       Heartbeat (mutual TLS) ---------> install orders
```

## 3. Who serves network boot

**Recommendation: a separate, optional binary, `hostkeeper-pxe`.** It is a proxyDHCP server and a read-only TFTP and
HTTP file server. It runs only while the operator runs it, in the foreground; its packages enable no unit. It answers
only the machines the operator recorded.

- **proxyDHCP, so the router is untouched.** A proxyDHCP server (PXE specification 2.1) never gives an address. The
  farm's router keeps its DHCP. When a firmware's DHCPDISCOVER says it is a network-boot client (vendor class
  `PXEClient` or `HTTPClient`) for x86-64 UEFI (client architecture 7 or 9, or 16 for HTTP boot), `hostkeeper-pxe`
  sends a second offer. That offer carries no address (`yiaddr` 0). It echoes the vendor class (option 60), carries
  the PXE vendor options (option 43, with discovery control set so the client uses the boot file of the offer), and
  names the boot file (option 67): a TFTP file name for `PXEClient`, an `http://` URL for `HTTPClient`. A PXE client
  that then sends its DHCPREQUEST to the proxy on UDP 4011 gets an ACK with the same boot file. The firmware takes its
  address from the router's offer and its boot file from ours. Whether a given firmware's HTTP boot accepts a proxy
  offer is **[V]**; one that does not still boots by UEFI PXE, which every UEFI server firmware has.
- **It answers only recorded machines with no host and no request.** At start, and again before each answer, it reads
  through the operator API the inventory (`Enrollment.ListRecords`), the hosts (`Hosts.List`) and the pending
  requests (`Enrollment.List`). It answers a DISCOVER only when its client MAC is a data MAC of a record that is not
  enrolled, whose host id the coordinator knows no host by (in any state, retired included), and that no pending
  request matches. A machine that is enrolled, retired, or installed and waiting for approval, and has network boot
  first in its boot order, gets no answer, so its firmware goes on to the disk. Without that rule, every such machine
  would be wiped at its next reboot while `hostkeeper-pxe` runs. The `enrolled` mark of a record alone is not enough:
  it is false for a retired host and for a machine whose request waits for approval. Reinstall of a known host is a
  separate, named opt-in (section 8).
- **It answers a machine until that machine's install starts.** Once a machine has fetched its seed, `hostkeeper-pxe`
  stops answering that MAC for the rest of the run, so the reboot after the install goes to the disk. The installer
  also puts the installed disk first in the UEFI boot order (curtin's default `reorder_uefi`, **[V]** on each firmware).
  A machine whose install failed is booted again by restarting `hostkeeper-pxe`.
- **What it serves.** By TFTP, for firmware that does UEFI PXE: the boot loader files and `grub.cfg`. By HTTP, for UEFI
  HTTP boot and for everything after the boot loader: the kernel, the initrd, the Ubuntu ISO, the hostkeeper deb and
  each machine's seed. It serves only files it verified at start (section 5) and the seeds it generated. Every request
  for any other path is refused, and it never accepts a write.
- **Flags, not environment** (rule S2): the interface to answer on, each listen address and port (the protocol ports
  67, 4011 and 69, and an HTTP port, by default), the coordinator's private listener and the operator token file to
  read with, the verified boot directory, the seed inputs (section 4), and `--reinstall <host id>` (section 8).
- **Where it runs, and as whom.** It runs on a host with a NIC on the farm LAN that can also reach the coordinator's
  private listener (Question 3). It starts as root only to bind its ports, then drops to an unprivileged user before it
  reads the token file (opened before the drop and kept as a descriptor) or parses a packet. Sending broadcasts needs
  no privilege once the socket is bound.

**The L2 requirement.** A DHCPDISCOVER is a broadcast. A proxyDHCP server sees it only on the same broadcast domain,
unless a relay forwards it; this design adds no relay. A proxyDHCP server also relies on a base DHCP server on that
segment, because it gives no address. The farm has two segments:

- **The farm LAN** has the router's DHCP. `hostkeeper-pxe` serves this segment. A new machine must have one NIC on it,
  and that NIC must be the one the firmware boots from.
- **The storage network** is static, with no DHCP. A proxyDHCP server there has no base offer to add to, so a machine
  that is only on it cannot be installed this way. That would need a full DHCP server or a relay, which is not in v1
  (section 10).

The farm's router must not offer a boot file of its own (options 66 and 67, or a `file` field). If it does, the
firmware may prefer the router's offer, and the result depends on the firmware. A next-server address with no file
name is harmless **[V]**. This is Question 4.

**Alternatives considered.**

- **The coordinator serves network boot.** This was rejected. It would need a UDP 67 listener, broadcast and raw
  socket rights, and a TFTP server in the farm's control plane, which runs all the time. Network install is wanted for
  a few hours when machines arrive. It would also put the coordinator on the farm LAN's broadcast domain, which ties
  where the coordinator runs to where machines are racked. The coordinator stays as simple as hostkeeper-design.md
  section 4.3 makes it. It gains one listener (section 7), and that listener serves only the host-facing API that
  already exists.
- **A documented recipe only** (dnsmasq or the router's own options, with hostkeeper shipping only the image). This
  was rejected as the primary path. Answering only recorded, unenrolled MACs is the safety rule that keeps an enrolled
  host from being wiped, and in a recipe it is a list the operator keeps by hand. A recipe also has no code that CI can
  test end to end, and it changes the router or adds a second DHCP configuration to look after. It is still possible:
  the verified boot directory and the seeds are plain files, and section 12's runbook says what another server must
  serve.
- **A full DHCP server.** This was rejected. It would take over the router's job on the farm LAN, and two DHCP servers
  giving addresses on one segment is an outage.
- **Setting options 66 and 67 on the router.** This was rejected. It depends on the router, CI cannot test it, it
  points every PXE client at the boot server all the time (not only for the window), and it cannot tell enrolled
  machines from new ones.

## 4. What a bare machine boots into

**Recommendation: the Ubuntu 24.04 live-server installer, driven by autoinstall, with a NoCloud seed that
`hostkeeper-pxe` generates for each machine.** The installer writes the root file system, installs the hostkeeper
deb, writes `hostkeeper.toml`, and reboots. The daemon starts from the installed disk and enrolls.

- **The boot chain.** The firmware loads Ubuntu's signed shim, which loads Ubuntu's signed GRUB. GRUB reads a
  `grub.cfg` from `hostkeeper-pxe` with a single entry and no menu wait. That entry loads the live-server kernel and
  initrd with `ip=dhcp`, `url=` set to the ISO on `hostkeeper-pxe`, `autoinstall`, and
  `ds=nocloud;s=http://<pxe>/seed/<mac>/`. The MAC comes from GRUB's `net_default_mac`, so each machine fetches its own
  seed. This is the netboot path Canonical documents for 24.04 live-server **[V]**, and it is why section 6 can leave
  Secure Boot as it is. The live system fetches the whole ISO into memory, so a machine needs several GiB of RAM
  **[V]: the figure, and that every farm server has it**. The semicolon of `ds=nocloud;s=...` is escaped in
  `grub.cfg`, where it would otherwise end the command, and a golden pins that.
- **The install is offline.** The seed turns off everything that would reach the internet: the installer's own update
  (`refresh-installer: update: no`), the apt mirror and security pockets (`apt: fallback: offline-install`, with no
  mirror), updates during install, and geoip. It installs from the ISO's package pool only. The farm LAN does have a
  route out, but a fetch that depends on it would make the install depend on it; in the end-to-end test (section 11)
  there is no route out at all.
- **The seed** is generated by `hostkeeper-pxe` from the inventory record and its flags. Each output is a pure function
  of its inputs, and goldens pin it (section 11).
  - The host name is the record's host id.
  - Storage: the disk is chosen by `--install-disk` as an autoinstall match. The default is the smallest
    non-removable SSD. Only that disk is written. This is Question 5.
  - One local operator account, with its password locked and the SSH public keys given by `--ssh-authorized-keys` (a
    file of public keys; a private key block in it is refused).
  - The hostkeeper deb, installed into the target from `hostkeeper-pxe` by two fixed `late-commands`: fetch it to a
    path in the target, then `dpkg -i` it in the target. The commands hold no logic. The deb declares no dependencies
    (packaging/linux/nfpm.yaml, **Built**), so it installs offline.
  - `/etc/hostkeeper/hostkeeper.toml`, root-owned and mode 0600. It is the daemon config given by `--daemon-config`.
    `hostkeeper-pxe` loads that file with the daemon's own loader (`crates/hostkeeper-config`, **Built**) before it
    writes a single seed, so a config the daemon would refuse is refused at the start of the run, not on 20 machines.
    The installed system's cloud-init gets it through the autoinstall `user-data` section. `cloud-init.service` runs
    before `network-online.target`, and `hostkeeper.service` runs after it, so the file is in place when the daemon
    starts. A daemon that finds no config is started again every second (hostkeeper-design.md section 4.1, unit
    `Restart=always` with no start limit), so this ordering is not load-bearing.
  - With `--serial-console <port>`, the installed kernel's `console=` names that serial port and its serial getty is
    enabled, so the BMC's serial console shows the login prompt (section 7).
  - The installed system pins its cloud-init to the NoCloud seed and nothing else (`datasource_list: [ NoCloud, None
    ]`). This is the farm's current node shape (below).
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

1. **Boot files are verified before they are served.** Each hostkeeper release carries `netboot.json` for
   `linux-x86_64`. It lists, for every file of the boot set, the URL on Canonical's release site, the size and the
   SHA-256: the live-server ISO, and the shim, GRUB, kernel and initrd that match it. The pins live in
   `packaging/netboot/pins.toml`, and are checked against Canonical's signed `SHA256SUMS` when they are changed.
   `netboot.json` is attested by the release workflow, like every release asset (`.github/workflows/artifacts.yml`,
   **Built** for the other assets).
   - `hostkeeper-pxe fetch` downloads the boot set and the hostkeeper deb into a new directory. It then verifies
     `netboot.json`'s attestation and the deb's attestation against the operator's `trusted_signers`, with the same
     verifier the daemon uses for packages (hostkeeper-design.md section 5.5). It checks every file's size and
     SHA-256 against `netboot.json` in one pass while writing it, and writes a seal.
   - `hostkeeper-pxe serve` checks the seal at start and refuses to start on any difference. A tampered asset, a
     stale download or a mixed set is therefore never served.
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
chain from disk.

**What it protects, and what it does not.** With Secure Boot on, a LAN attacker cannot make the machine run a boot
loader or kernel Canonical did not sign. They can still change the kernel command line, the initrd and the ISO (point
2 of section 5), and so still control the install. With Secure Boot off, they control everything from the first file.
Either way, in v1 the farm LAN must be free of an active attacker while `hostkeeper-pxe` runs. Q15 of
hostkeeper-design.md does not rely on a trusted network: it keeps approval a person's click because a MAC is only a
claim. That defence does not reach this case, because an attacker who controls the install controls the key hash the
console shows too. The residual risk is stated here as a decision for the CEO (Question 2), not hidden.

**Later: a fully verified chain.** Enroll a key of the farm's own in each machine's Secure Boot `db` (by the BMC, once
BMC control exists, section 10). Sign our iPXE with that key. Have iPXE check every following file (section 5's
alternative), including an image whose digest it checks. With a TPM claim recorded beside the MACs (hostkeeper-design
section 12.1), this is also what would let Q15 move to (b).

## 7. LAN enrollment

**Today** (**Built**). The coordinator has two listeners: the heartbeat listener (`--heartbeat-listen`) and the private
listener (`--private-listen`). Enrollment has no listener of its own. `Enroll` and `Poll` are calls of
`hostkeeper.agent.v1.Coordinator` on the heartbeat listener, which with `--enrollment-dir` lets a TLS client with no
certificate finish the handshake. `Heartbeat` and `Renew` refuse such a caller (`crates/hostkeeper-coordinator/src/
listen.rs`, `grpc/mod.rs`). On the farm the heartbeat listener is bound to the tailnet address, and a bare machine has
no tailscale.

**Designed: `--lan-listen <address:port>`**, a third listener. It serves the agent service and nothing else:

- `Enroll` and `Poll` to a caller with no certificate.
- `Heartbeat` and `Renew` to a host certificate only, with exactly the checks of the heartbeat listener. This depends
  on Question 1.
- Never the operator API, the JSON gateway or the UI. A call to any of them on this listener is refused
  `UNIMPLEMENTED`, as an operator call on the heartbeat listener is today (`grpc/mod.rs`).

The address must be one interface's, and a wildcard address is refused at start, as for `--private-listen` today. The
daemon config of a network-installed host names this listener as `coordinator`. Existing hosts keep the tailnet
address. One coordinator serves both groups, with one CA and one enrollment store.

**The server certificate's names.** Today the coordinator's server certificate carries DNS names only (`--server-name`,
**Built**), and it is made again only when its file is missing or a third of its life is left. Two changes are
designed, in the same pull request as the flag:

- The IP address of `--lan-listen` is added to the certificate as an IP SAN. A bare machine then dials the coordinator
  by address, and needs neither a name in the router's DNS nor a hosts file. `--server-name` stays for names.
- The certificate is made again at start, and at the reload tick, whenever its names differ from the flags. Today a
  changed `--server-name` takes effect only when someone deletes the file.

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
  While a request is pending, the daemon writes its key hash, the coordinator it posted to, and the request's state to
  `/run/hostkeeper/enrollment.issue`. The deb ships `/etc/issue.d/hostkeeper.issue` as a link to it, so the login
  prompt shows it. **[V]**: that agetty on 24.04 reads `issue.d`, and that it shows a file made after the prompt was
  drawn only when the prompt is drawn again; so the daemon asks the gettys to redraw (`agetty --reload`) each time it
  writes the file. Which consoles show it: the local console (tty1) always; the BMC's serial console only when the
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

**Q15 stays (a) for Linux in v1.** None of the claims above approves anything by itself, and nothing auto-approves.

## 8. Reinstall of a known host

The host keeps its host id, role and services. Its disk is written anew, and it gets a new key.

1. The operator starts `hostkeeper-pxe` with `--reinstall <host id>` (repeatable). Only for the records of those hosts
   does it set aside the rule of section 3 that a record with a host or a pending request gets no answer. This is a
   named opt-in, so a reinstall is never an accident of the boot order. The answer-until-seed rule still applies.
2. The machine is booted from the network, and installs as in section 4. Only the disk matched by `--install-disk` is
   written. Other disks, for example a storage node's data disks, are not touched by the installer, and what the
   host's services find on them is the services' business.
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
| A recorded machine boots while `hostkeeper-pxe` is not running | No proxy offer. The firmware tries its next boot device, and nothing is installed. |
| An enrolled host with network boot first in its boot order | No answer (section 3), so it boots from disk. With `--reinstall` naming it, it is reinstalled. |
| A retired host whose record is still in the inventory | No answer: the coordinator still knows a host by that id (section 3). |
| An installed machine waiting for approval reboots, or `hostkeeper-pxe` is restarted meanwhile | No answer: a pending request matches its record (section 3). |
| Someone posts a request matching a record before the machine boots | That machine gets no answer until the operator rejects the request; the record shows a pending request that no installing machine made. |
| An unrecorded machine | No answer. |
| The router also offers a boot file | Depends on the firmware (Question 4). The runbook says to check this first. |
| The boot set on disk differs from its seal, or was never verified | `hostkeeper-pxe serve` refuses to start, naming the file. |
| `netboot.json` or the deb is attested by another signer, workflow or ref | `hostkeeper-pxe fetch` refuses, naming the attestation, and keeps nothing. |
| The daemon config given to `hostkeeper-pxe` does not load | Refused at start with the daemon loader's error, before any seed is written. |
| The install fails mid-way | The machine is not answered again in this run. Its console shows the installer's error. The operator restarts `hostkeeper-pxe` to retry. |
| The installed host cannot reach the LAN listener | The daemon retries, as today, and the console's issue line shows the coordinator it is trying. No request appears. |
| The coordinator's CA is not the pinned one | Nothing is sent (`UntrustedCoordinator`, **Built**), and the issue line says so. |
| More than `--max-pending-enrollments` requests | `pending_full`, retried every 5 s (**Built**). |
| Two pending requests match one record | Both `contested`, and approval is refused until all but one are rejected (section 7). |
| A machine is on the storage network only | No base DHCP, so it cannot be installed (section 3). |

## 10. v1 scope, and later

**In v1:** `linux-x86_64` machines with UEFI firmware, new machines only, booted from a NIC on the farm LAN, installed
with Ubuntu 24.04. A person powers each one on and picks network boot.

**Later, not designed here:**

- `linux-arm64` (a second boot set, with arm64 UEFI architecture 11 in the proxy offer).
- Legacy BIOS PXE.
- **BMC power and boot-device control.** Today the BMC MAC is recorded and nothing else (hostkeeper-design.md section
  12.1). With it, the BMC sets a one-time network boot and powers the machine on, and step 2 of section 1 goes away.
- The fully verified boot chain with the farm's own Secure Boot keys (section 6).
- A TPM claim, and with it Q15 (b).
- An optional product UUID field on the inventory record (section 7).
- A full DHCP server or relay for segments with no DHCP, such as the storage network, or a separate provisioning VLAN.
- Approving many new machines at once.
- Reinstalling the six existing nodes this way. They keep the adopt path, and moving them is its own decision.

## 11. Tests, and the measurement

Every crate these items add or touch is measured by the 99% line and branch gate (rule T1) in the pull request's own
CI. The end-to-end job counts toward no gate, so nothing relies on it for coverage. Tests assert behaviour, never
time (rule T10), are Rust (rule T11), and use the repository's scratch helpers and fakes (rule T12).

**Unit and integration tests, by the item that builds them (section 12):**

- proxyDHCP packets (N7, pure: a packet and the set of MACs to answer in, a packet or nothing out):
  - For a MAC in the set, a DISCOVER with a `PXEClient` or `HTTPClient` vendor class and an x86-64 UEFI architecture
    gets an offer with no address (`yiaddr` 0), the echoed vendor class, option 43 and the boot file (a TFTP name or an
    `http://` URL by class). This catches a server that hands out addresses and fights the router.
  - A DHCPREQUEST to UDP 4011 for a MAC in the set gets an ACK with the same boot file; one for a MAC not in it gets
    nothing.
  - Nothing is sent for: a MAC not in the set, a DHCP message with no PXE vendor class (the installer's own DHCP and
    every ordinary client), BIOS and arm64 architectures, a REQUEST naming another server, and truncated, oversized or
    malformed packets.
- TFTP packets (N7, pure): only names of the file set are served; a write request, a path with `..`, an absolute path
  and an unknown name are refused. Block numbering and the final short block follow RFC 1350, and option negotiation
  RFC 2347.
- Boot-set verification (N8): `fetch` refuses each of these, naming it: a size mismatch, a digest mismatch, a missing
  file, a `netboot.json` attested by another organisation, workflow or ref, and a deb whose attestation does not
  verify. The verifier is the daemon's, given a fake attestation port, as the daemon's own tests do.
- Serving (N9). Every listen address and port is a flag, so the tests bind ephemeral loopback ports with real UDP and
  TCP sockets, and talk to a fake coordinator (the fake operator service pattern of `crates/hostkeeper-ctl/tests`).
  - The answer set: a record with a host in any state, retired included, or with a pending request that matches it,
    is left out, unless `--reinstall` names its host id; a record is dropped from the set once its seed was fetched.
  - HTTP serves only sealed files and generated seeds, and refuses any other path and every method but GET and HEAD.
  - `serve` refuses to start on a seal mismatch, naming the file.
  - It drops privileges before it reads a packet (the user to drop to is a flag; the test passes its own).
- Seed generator (N6): goldens, compared by exact equality, for a record with and without a role, with one SSH key and
  several, and for each `--install-disk` form. It refuses a daemon config the loader refuses, and a private key in
  the SSH keys file. A test asserts that the seed is exactly the golden, with no field beyond the documented ones, so a
  token cannot slip in unnoticed.
- LAN listener (N2): `Enroll` and `Poll` work with no certificate. `Heartbeat` and `Renew` without one are refused.
  Each operator service, the JSON gateway and the UI are refused on it. A wildcard address is refused. The listener's
  IP is in the certificate's SANs, and a changed name list makes a new certificate.
- Console key hash (N3): exact content of the issue file for pending, approved and untrusted-coordinator states.
- Contested records (N4): `Approve` is refused `record_contested` while two pending requests match one record, and
  succeeds after the other is rejected. `List` names the contested requests both ways.
- Reinstall (N4, store level): the existing `a_host_role_is_read_only_for_the_key_the_host_holds` already proves that
  the old key names the host no more. N2 adds the coordinator-level test that a heartbeat with the old certificate,
  on the LAN listener, is refused `forbidden`.

**The end-to-end proof (N10).** One Rust integration test in `crates/hostkeeper-privileged`, behind its feature
`privileged`, in a new job `net-install` of `privileged.yml`. That workflow runs only on a push to `main` and by hand,
never on a pull request (rule B9), and the workflow lint is extended to know the job.

- The test makes a Linux bridge with no route out, and a QEMU guest with OVMF UEFI firmware on it. The guest has an
  empty scratch disk, a fixed `-uuid` and an SMBIOS serial.
- It runs, on the bridge:
  - a minimal fake of the router's DHCP, inside the test, that gives addresses and no boot file;
  - the real `hostkeeper-pxe`, with a boot set the job fetched by pinned URL and SHA-256 before the test;
  - the real coordinator, with `--lan-listen` on the bridge address.
- It waits on progress events, never on time. Each event is a line on the guest's serial console, read as a stream, or
  an answer from `hostkeeper ctl`. Each wait ends on its event or on the guest's exit, and the job's `timeout-minutes`
  is the only bound.
- In order, it checks that:
  - the fake DHCP saw the guest's DISCOVER, and `hostkeeper-pxe` sent its proxy offer;
  - the seed was fetched;
  - a pending request appears, `matched` to the record, with claims equal to the guest's UUID and serial;
  - the console line shows that request's key hash;
  - `Approve` by that hash succeeds;
  - the first heartbeat arrives;
  - an assigned fixture service reports installed.
  It then reinstalls the guest with `--reinstall` and checks the `duplicate_claim`, the `--replace` approval, and that
  the old certificate's heartbeat is refused.
- It needs `/dev/kvm` on the hosted `ubuntu-24.04` runner **[V]**. Without it, the job fails with that reason; it does
  not skip.

**The measurement for the white paper (N11).** The two numbers are the time from power-on to approved-and-serving, and
the number of manual steps.

- The time is computed from times the components already record. No test asserts it.
  - power-on: `hostkeeper-pxe`'s log of the first DISCOVER from the MAC;
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
- The manual steps are counted from the numbered steps of the runbook (N12): three per machine (section 1) and two per
  farm. The first real machine records them. For comparison, the steps of today's hand install of a node are counted
  from its current runbook at the same time. No count is quoted before then.

## 12. Tasks

One pull request each, in this order. "After" lists prerequisites. Sizes are those of hostkeeper-design.md section
12.2.

| # | Item | After | Size |
|---|---|---|---|
| N1 | This design, and the pointers from hostkeeper-design.md sections 2.2, 4.6 and 12.1 | none | S |
| N2 | Coordinator `--lan-listen` (agent service only, per Question 1); IP SAN of its address; server certificate made again when its names change; the tests of section 11 | N1 | M |
| N3 | Daemon: the console issue file of the pending request; the deb and the overlay ship the `issue.d` link | N1 | S |
| N4 | Enrollment: `contested` on `PendingEnrollment` and refusal `record_contested`; the UI's approval screen puts the hard-to-copy claims beside the record and adds the "matched is not proof" text | N1 | M |
| N5 | `packaging/netboot/pins.toml` and `netboot.json` for `linux-x86_64`, built and attested in the artifacts workflow | N1 | M |
| N6 | Seed generator: a pure crate turning a record, a daemon config and flags into `grub.cfg`, autoinstall `user-data` and `meta-data`, with goldens | N1 | M |
| N7 | proxyDHCP and TFTP packet handling: a pure crate, no sockets, no inventory | N1 | M |
| N8 | `hostkeeper-pxe fetch`: download, verify against the release attestation and `netboot.json`, seal | N5 | M |
| N9 | `hostkeeper-pxe serve`: sockets and privilege drop, the answer set from records, hosts and pending requests, answer-until-seed, `--reinstall`, HTTP and TFTP from the sealed directory, the seal check; released as a binary with no enabled unit | N6, N7, N8 | M |
| N10 | Privileged end-to-end: the `net-install` job and its test (section 11), and the workflow lint | N2, N3, N9 | M |
| N11 | `cargo xtask netinstall-report`, and the report artifact of N10 | N10 | S |
| N12 | Operator runbook `docs/operations/network-install.md`: router check, firmware settings, the per-machine steps, reinstall | N9 | S |
| N13 | First real machine on the farm, by the runbook, recording the measurement. Done by hand by the operator, not in CI | N10, N12 | S |

N2, N3, N4, N5, N6 and N7 are independent and can be built in parallel. The open install-path issues
(komira-ai/hostkeeper#109, komira-ai/hostkeeper#110, komira-ai/hostkeeper#115, komira-ai/hostkeeper#116 and
komira-ai/hostkeeper#102) do not block N10: it checks a first install, which works today
(`an_assigned_package_installs_once_across_restarts_and_the_boot_pass_keeps_it`, privileged).

## 13. Questions for the CEO

Each question has options and a recommendation. Until it is answered, the design follows the recommendation.

**Question 1. How does a network-installed host reach the coordinator once approved?** Its daemon config names one
coordinator address, and the daemon both enrolls and heartbeats at that address (hostkeeper-design.md section
4.1, **Built**). A bare machine has no tailscale.

- (a) **The LAN listener serves the whole agent service**: enrollment with no certificate, and heartbeat and renewal
  with a host certificate, exactly as the tailnet listener does. It never serves the operator API or the UI. The host
  needs nothing else.
- (b) **The LAN listener serves `Enroll` and `Poll` only.** Then the daemon config needs a second, additive key for
  the enrollment address, and the heartbeat stays on the tailnet. Each new host then needs someone to join it to the
  tailnet by hand after approval, which is a fourth manual step per machine. The services ordered to it wait until
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
by unauthenticated DHCP, and an attacker on it can already disturb every node. Note that Q15 (a) does not cover this
case: a person's approval protects against a forged request, not against an install the attacker controls.

**Question 3. What credential does `hostkeeper-pxe` read the inventory with, and where does it run?** It needs the
records, hosts and pending requests (section 3), from the coordinator's private listener, and it must sit on the farm
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

**Question 4. Which segment, and does the farm's router stay out of network boot?** Recommendation: new machines are
installed from a NIC on the farm LAN, not the storage network (section 3). The runbook's first step confirms that the
router offers no boot file name (options 66 and 67, or the `file` field). If it does, that is switched off on the
router once; it is not worked around in `hostkeeper-pxe`. Only the CEO can see and change the router's settings.

**Question 5. Which disk does the installer take by default?** The installer wipes the disk it matches on any machine
the operator recorded. Options:

- (a) The smallest non-removable SSD, with `--install-disk` to override.
- (b) No default: `--install-disk` is required.
- (c) Follow the existing nodes' layout once their recipe is known.

Recommendation: (a), changed to (c) when the recipe is found. A storage node's many data disks are larger than its
boot SSD, so "largest" would be the wrong default.
