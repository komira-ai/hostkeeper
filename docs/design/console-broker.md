# hostkeeper console broker design

Status: design, for the maintainers' comments. Nothing in this document is built unless it is tagged **Built** with a path. It extends [hostkeeper-design.md](hostkeeper-design.md) section 12.1 ("power control through the BMC") and [network-install.md](network-install.md) section 10 ("BMC power and boot-device control"), and ties into [hostkeeper-mdm.md](hostkeeper-mdm.md) for the Macs.

How to read it: **Built** is code on `main` (with a path). **Designed** is this document's proposal. **Measured** is a fact read from the lab on 2026-11-10. **Inferred** is a figure derived from protocol structure or from numbers in this repository and not yet measured (task C0 measures them). **Unverified** is a claim taken from vendor documentation or a forum that no one has run against the lab's firmware. Test names are in code font and are **(to build, Cn)** unless they exist. The repository is public: this document uses `<bmc-ip>`, `<mac-ip>` and the documentation address ranges, never a lab address or host name.

## 1. Problem

A server that has stopped answering can only be fixed from its BMC, and a Mac that has lost its session can only be fixed from its screen. Today neither can be reached by an operator who is not standing in the lab.

The motivating incident: node-02 was force-rebooted, came back at the PXE menu, and needed its boot order changed. The BMCs sit on the lab LAN, not on the tailnet. Fixing it meant going to the lab, or logging in to some other machine on the LAN first and opening the BMC web UI from there.

The coordinator already knows which machine is which: each server's data MAC and BMC MAC are in its inventory record (**Built**: `crates/hostkeeper-coordinator/src/inventory.rs`, `Hosts.Get` and `Hosts.List`). It does not know any address (section 4), and nothing in hostkeeper carries an interactive byte stream (section 4).

## 2. Goals and non-goals

Goals, in the owner's order:

1. From any machine on the tailnet, an operator opens, for a Linux server: the serial console (SOL), the BMC's HTML5 KVM and web UI, and power control; for a Mac: its screen through Screen Sharing (RFB, TCP 5900).
2. The guard is the coordinator: who may open a console is decided by the operator token, and every open and every power action is one audit line.
3. Built fast: a first vertical slice (the Mac screen) in four pull requests, with no new network device and no change to the daemon.

Decided by the owner and not reopened: **one shared BMC username and password for every BMC.** There is no rotation, no vault, no per-host secret. The Mac is reached with the Mac's own login; the broker holds no Mac credential.

Non-goals: Redfish (section 6, D7), a browser terminal or noVNC in the first build (section 6, D3), recording a session's content, a Mac that is not on the lab LAN, and holding the BMC off the tailnet by any means other than not routing it (section 11).

## 3. What an operator sees

```
hostkeeper console sol  node-02            # serial console in this terminal
hostkeeper console kvm  node-02            # opens the BMC's KVM in the browser
hostkeeper console mac  farm-mac-1         # opens Screen Sharing on this Mac
hostkeeper console power node-02 cycle --reason "stuck at PXE" --live-ok
hostkeeper console bootdev node-02 disk --reason "fix boot order"
hostkeeper console list                    # open sessions: who, what, since
hostkeeper console close <session>
```

Before: go to the lab. After: the operator's own terminal and browser, over the tailnet, for one node or Mac by its host id. The BMC password is never shown. What does not change: the daemon, the update path, the operator token, `hkctl`, and every existing listener. An operator whose token has no `console` capability (section 7.1) is refused, and every existing token starts without it.

For the incident: `console bootdev node-02 disk` (one IPMI command, no screen), then `console power node-02 cycle`, then `console sol node-02` to watch it boot if the kernel prints to the serial port (section 5.1).

## 4. State today

- **Operators.** One class: a named bearer token (**Built**: `crates/hostkeeper-coordinator/src/tokens.rs`, `OperatorTokens::name_of`). The private listener (`--private-listen`) is TLS server-auth only, reached over the tailnet. Authorization is the token name; there are no roles. `Caller` is `Operator | Ci | Anonymous` (`src/operator.rs`).
- **Audit.** Every ops change is one `AuditEntry` (`seq, by, action, service?, node_id?, host_group?, reason?`) and one fsynced JSON line in `<ops-file>.audit` (**Built**: `src/ops.rs`, `src/audit_file.rs`). A change whose line cannot be appended is not made (`audit_unavailable`). `action` is a free string, so a new action name is additive. Nothing audited today is a read or a stream.
- **Daemon channel.** The daemon connects out, listens on no port, and speaks four unary RPCs; a heartbeat reply carries at most one order from a closed set of verbs (**Built**: `agent.proto`, `crates/hostkeeper-link/src/heartbeater.rs`, 15 s period). **No order can carry a command or a stream.** The daemon is root. Putting a byte relay in it would give the root process a listener or a long-lived stream, which its design excludes ([hostkeeper-design.md](hostkeeper-design.md) section 3).
- **Listeners.** `src/listen.rs` closes a connection `CLIENT_PATIENCE` (10 s) after the handshake unless it is a registered long wait (`Releases.WaitConverged`, `src/wait.rs`, at most `MAX_WAITS` held). `--lan-listen` is the precedent for an extra listener: one interface, a wildcard address refused, its own `Surface`.
- **Addresses.** **No IP address of a host or a BMC is stored.** `Claims.addresses` is the link-layer addresses read from `/sys/class/net/*/address`. The daemon reads the BMC MAC only (`DevIpmi` in `crates/hostkeeper-daemon/src/ipmi.rs`, parameter 5, channels 1 to 11); it does not read the BMC's IP (parameter 3). A Mac claims only its serial and platform UUID.
- **Records.** A host's `Host` message carries `data_macs` and `bmc_mac` only when exactly one inventory record matches it. The six lab nodes enrolled before records existed, so their `bmc_mac` is empty until the operator adds a record (`Enrollment.AddRecord`, leaving the role empty so no approval window opens, [enrollment.md](../operations/enrollment.md)). That is the bootstrap, task C0.
- **UI.** The static UI refuses `new WebSocket(`, other-origin URLs, inline scripts and tokens in the address (`crates/hostkeeper-ui-tests/src/scan.rs`), and sends `default-src 'self'`. A console page does not fit it as is.
- **MDM.** The gate is linked into the coordinator and talks to NanoHUB on loopback only (**Built**: `crates/hostkeeper-coordinator/src/mdm.rs`, `crates/hostkeeper-mdm-gate/src/command.rs`, `COMMAND_TYPES`: five types, pinned by `the_gate_enqueues_only_its_five_command_types`).
- **Lab (Measured, one node of six).** The board is a Supermicro X10 with BMC firmware 3.91, IPMI 2.0. The BMC takes a DHCP address on the same LAN as the host. `ipmitool lan print` lists RMCP+ cipher suites 1, 2, 3, 6, 7, 8, 11, 12: **suite 17 is not offered, suite 3 is.** SOL is enabled at 115.2 kbit/s, privilege OPERATOR. The kernel command line is `console=tty1 console=ttyS0`. The host's neighbour table holds the BMCs and hosts as MAC and address pairs, mostly STALE, and **one MAC maps to several addresses** (a host on two subnets). `/dev/ipmi0` works. The other five nodes, any Redfish endpoint, the BMC's HTTPS behaviour and the Mac are not measured.

## 5. What each device offers, and the constraints that follow

### 5.1 Serial over LAN

SOL is IPMI 2.0 RMCP+ on UDP 623. On this board it needs `-C 3` (HMAC-SHA1, AES-CBC-128); the cipher suite is a flag (`--console-ipmi-cipher`, default 3), not a constant, because suite 17 appears on later boards and some firmware turns suite 3 off (**Unverified**, vendor notes).

Two facts the operator must know, so the doc states them:

- **SOL shows boot output only if the kernel has `console=ttyS<n>`** on its command line, with `n` the port the BMC wires to SOL (board-specific). Nodes built by `hostkeeper-pxe` get it only when `--serial-console` was passed (**Built**: `hostkeeper-pxe-seed`). The six existing nodes were not installed that way; one of them has it (Measured).
- **The firmware's own screens (POST, the PXE menu, setup) appear on SOL only if the BIOS has console redirection enabled.** Not measured on the lab. For the incident, where the node sat at the PXE menu, KVM is the dependable screen; SOL is for the kernel and the login.

One SOL session exists per BMC at a time (a second `sol activate` fails or takes over, depending on the BMC).

### 5.2 KVM and web UI

The X10 firmware 3.91 serves the ATEN HTML5 console on HTTPS with its own login (`/cgi/login.cgi` returning a `SID` cookie) and a secure websocket after it (**Unverified**: vendor documentation, forum reports, search snippets). Reports say an HTTP-aware reverse proxy breaks it on some firmware and a TCP-level proxy does not. The design therefore rewrites no HTTP: it carries the BMC's TLS bytes unchanged.

### 5.3 Power and boot device

IPMI `chassis power status|on|off|cycle|reset|soft` and `chassis bootdev pxe|disk|bios` need only RMCP+ and the same credential, so they work with the host down. Redfish `ComputerSystem.Reset` is not used (D7).

### 5.4 Mac screen

Screen Sharing is RFB on TCP 5900. The server banner is `RFB 003.889` and the client answers in kind; auth runs inside the stream (Apple's types, including the ARD Diffie-Hellman type), so a TCP relay carries it unchanged (**Unverified** end to end, task C0). Enabled from the command line, Screen Sharing gives **a black screen until someone approves at the Mac, or MDM enables it** (**Measured** on the farm Mac, macOS 27; Apple stopped letting `kickstart` enable it from 12.1). The broker cannot fix a black screen; it can say "port 5900 closed" (section 10) and section 9 removes the cause.

## 6. Decisions

| # | Question | Decision | Why |
|---|---|---|---|
| D1 | Where does the relay run? | A new unprivileged binary, `hostkeeper-console-relay`, run on a lab-LAN host (hostkeeper delivers it as a package, like any service). It **dials out** to the coordinator. Not in the daemon, not a subnet router. | The daemon is root, listens on nothing and carries no stream (section 4); adding that is the largest change to its trust. A subnet router makes every BMC reachable by any tailnet member with the shared password, with no coordinator check and no audit: it defeats the owner's guard. A relay needs no inbound port on the lab host and no new device. `hostkeeper-pxe` is the precedent for an operator-run LAN-side process. A down node (node-02) has no daemon, so the BMC path cannot depend on one. |
| D2 | How does the coordinator reach the relay? | The relay holds one long poll open to the coordinator (`/console/v1/relay/wait`, at most 25 s, then re-issued); an open command is the reply. The relay then dials a data connection back (`/console/v1/relay/<session>`, HTTP/1.1 `Upgrade: hostkeeper-console/1`); the coordinator splices it to the operator's connection. | Reuses the pull idiom: a held poll gets the command at once, not at the next 15 s heartbeat. No WebSocket crate in the first build. Needs a console listener (D3) because `listen.rs` closes ordinary connections after `CLIENT_PATIENCE`. |
| D3 | How does the operator connect? | A CLI first: `hostkeeper console <kind> <host>` calls the operator RPC `Console.Open` (token), receives a one-time ticket, and attaches to the console listener with `Upgrade` and the ticket; it serves the stream on `127.0.0.1:<random>`. The web page comes later (task C10). | A browser WebSocket cannot send `Authorization`, the UI scan refuses `new WebSocket(`, and the UI's CSP is `default-src 'self'`. A CLI sidesteps all three and is what the incident needs. The ticket (256 random bits, 60 s, one use, bound to the token name, host and kind) is what the later page also presents. |
| D4 | Session lifetime and audit | Idle 30 min, total 8 h, both flags. Open is one audit line written **before** the session starts; no line, no session (`audit_unavailable`), as for any change that steers the fleet. Close is one best-effort line (cannot refuse a close). | Matches `audit_file.rs`. The close line carries cause, duration and bytes in `reason`; typed fields are a later additive change. |
| D5 | Where is the one BMC credential? | `--console-bmc-credential-file` on the coordinator: a secret file read by `hostkeeper_wire::secret::read_secret`, mode 0600/0400, owned by the coordinator user (as `--mdm-api-key-file`). The coordinator sends it inside each open command, over the relay's TLS stream, to the relay that needs it; the relay holds it in memory for that session and never logs it. The operator never receives it. | The owner's decision. A flag naming a file satisfies S1 and S2. Sending it per open keeps it off the relay host's disk. |
| D6 | SOL and power through | `ipmitool -I lanplus -C <cipher>` run as a child of the relay, the password in the child's environment (not `argv`, not `ps`). A native RMCP+ client is later (C12). | The workspace pins `ipmi-rs` to its device-file connection only and leaves out its RMCP+ client (root `Cargo.toml`, MD5). A native client is weeks; `ipmitool` is days and was measured working on the lab. Cost: one tool on the relay host; the relay's output is mapped to a closed error enum and **never echoes the tool's stderr** (it contains the user name). The relay's child spawn is behind a port with a scripted double, as `Ipmi` is in `hostkeeper-enroll`. |
| D7 | SOL vs Redfish | IPMI. No Redfish. | Firmware 3.91's Redfish is a separate download and unconfirmed on the lab; the vendor's Redfish lists the serial interface as a resource, not a stream (**Unverified**). IPMI is measured. |
| D8 | KVM: proxy the web UI with an injected login, or the BMC's own token | A TCP tunnel to `<bmc-ip>:443` for the page and websocket, with a **server-side login**: the relay posts the credential to `/cgi/login.cgi` itself and returns only the `SID`. The client serves a one-shot plain-HTTP page on `127.0.0.1:<p0>` that sets the cookie and redirects to `https://127.0.0.1:<p1>/` (the tunnel). Cookies are not isolated by port. | The operator never sees the password; the browser sees the BMC's own TLS (self-signed, so a warning once); no HTTP rewriting. A session cookie is revocable; a password is not. **Unverified:** cookie names, whether firmware 3.91 sets `Secure`, and its Origin and Host checks, all in task C0. Fallback if the cookie hand-off fails: an HTTP-aware proxy on the client that adds `Cookie: SID=...` and rewrites nothing else. |
| D9 | Mac screen | A plain TCP relay to `<mac-ip>:5900`. The Mac's own login authenticates. The client serves `127.0.0.1:<p>` and, on a Mac operator machine, opens `vnc://127.0.0.1:<p>`. | RFB authenticates inside the stream. No credential in the broker. A browser client is not planned: noVNC's support for Apple's auth types is unconfirmed. High Performance mode through a relay is **Unverified**; the standard path is the design. |
| D10 | Power actions | Typed RPCs, not a console: `Console.Power{host, action, reason, live_ok}` and `Console.SetBootDevice{host, device, reason}`. `action` is a closed enum (status, on, off, cycle, reset, soft); `device` is pxe, disk or bios, one-time only. `off`, `cycle` and `reset` need a `reason`, and are refused `host_live` when the host's heartbeat is fresh unless `live_ok` is set. | Each is one audit line with the actor, host and action. The `live_ok` flag is one boolean, not a policy engine (B20). |
| D11 | Locating a device | By MAC: the relay resolves the inventory MAC to an address, section 8. | The owner's decision; the MAC is the stable key and the address is derived. |
| D12 | Who may open a console | A new optional key in `--tokens-file`: `console = true` on a `[[token]]`. Absent means false. | Additive (B8): older files parse; every existing token starts without console access. The smallest step above "any operator token". Roles are not needed for a group this size. |
| D13 | Two operators on one SOL | The coordinator allows one SOL session per host. A second open is refused `console_busy` and names the holder's token name and start time. `--takeover` closes the first (the coordinator sends `sol deactivate`) and writes `console_takeover`. | BMCs allow one SOL session; deciding in the coordinator is predictable and audited. Read-only observers are later. |

## 7. Components

![Components: what hostkeeper runs for the console broker](diagrams/console-broker-components.svg)

The orange dashed box is exactly what hostkeeper runs: the client verb on the operator's machine, the coordinator's console module, and the relay. Outside it: the tools that are not hostkeeper (`ipmitool`, the browser, the Screen Sharing viewer), the BMC, the Mac, and NanoHUB.

New code, all designed:

- **`crates/hostkeeper-console`** (library): the session table, ticket mint and redeem, the one-SOL-per-host rule, idle and total timers, the closed error enum, the frame types between client, coordinator and relay. Pure; time comes from an injected clock.
- **Coordinator, `src/console/`** (a directory, because `service.rs`, `config.rs` and `operator.rs` are each near 1000 lines): `Console.*` operator RPCs (`console.proto`, a new file), the console listener (`--console-listen`: one interface, a wildcard refused, TLS server-auth with the private listener's certificate, its own limits `--console-max-sessions` default 8 and `--console-max-relays` default 4), the relay long poll and the splice.
- **`hostkeeper-console-relay`** (binary, forwards to a library): relay token file, the resolver (section 8), the ipmitool port, the SID login, TCP dials.
- **`hostkeeper console`** (a verb of the existing `hostkeeper` binary, `crates/hostkeeper`): the client. `hkctl` is a bash and curl script generated from the command table and cannot carry a stream; it gets `console open` for free from the proto, which mints a ticket and prints it.

Relay authentication: `--console-relay-tokens-file`, the same hashed-token TOML as `tokens.rs`, one name per relay; a relay token opens nothing else, and an operator token cannot act as a relay. The relay passes its own `--host-id` so the coordinator can avoid it as the relay for its own host (section 10).

## 8. Finding a device by MAC

The coordinator gives the relay the MACs (the BMC MAC for SOL, KVM and power; the data MACs for a Mac) and the relay returns an address. The coordinator asks every connected relay and takes the first hit; a relay whose `--host-id` is the target does not answer for it.

1. **Neighbour table.** The relay reads `/proc/net/arp` (no tool, no `unsafe`, no root). A complete entry for the MAC is a candidate. Cost: one file read.
2. **Probe.** On a miss, the relay sends a one-byte UDP datagram to each address of its scan range (`--console-scan-cidr`, repeatable, prefix at least /22) so the kernel resolves each by ARP, waits `--console-probe-ms` (default 1000), and reads the table again. No raw socket.
3. **Several addresses for one MAC** (Measured: a host on two subnets). The relay checks each candidate against the device: for a BMC, an RMCP presence ping (ASF, UDP 623, 12 bytes) that the BMC answers; for KVM, a TCP connect to 443; for a Mac, a TCP connect to 5900. The first that answers wins.
4. **Cache** for `--console-resolve-ttl` (default 60 s). A cached address that fails its device check is dropped and steps 2 and 3 run once more; a second failure is `device_not_found`. This is how a DHCP change or a moved MAC heals.

The BMC's address is DHCP-assigned (Measured), so it can change; only the MAC is stored. For a Mac, the record needs its Ethernet or Wi-Fi MAC as a data MAC (the operator adds it to the record; a Mac still matches enrollment by serial, **unverified** that an added data MAC does not change that match, task C5). Later the gate asks MDM for `WiFiMAC` and `EthernetMACs` in `DeviceInformation` (additive) and the operator types nothing (section 9).

## 9. Operations

Common to all four: `Console.Open` checks the token and its `console` capability, finds the host with `Hosts.Get`, writes the audit line, mints the ticket, and answers with the console listener's address. Hops are numbered in the diagrams; the budgets are **Inferred** until C0 measures them. "RTT-t" is a tailnet round trip, "RTT-l" a lab LAN round trip; neither is measured here.

### 9.1 Open SOL

![Open SOL: sequence](diagrams/console-sol-sequence.svg)

Serial hops to the first byte, in order: (1) client to coordinator `Console.Open`, one RTT-t plus TLS on a new connection (about 2 RTT-t) and one audit fsync; (2) coordinator to relay, the held poll, half an RTT-t; (3) address: a cache hit costs nothing, a miss costs the probe (1 s, flag) ; (4) relay dials back and the coordinator splices, about 2 RTT-t (TLS handshake); (5) `ipmitool` RMCP+ session setup, about 6 RMCP exchanges of RTT-l each plus the BMC's service time, which dominates: **Inferred** 1 to 3 s; (6) `sol activate`, then the first output is whatever the console prints next, so the client sends a carriage return and the prompt is the first byte. Budget: under 5 s warm, under 6 s with a probe. Throughput is 115.2 kbit/s at the BMC; the relay adds no buffering beyond one frame.

### 9.2 Open KVM

![Open KVM: sequence](diagrams/console-kvm-sequence.svg)

Hops: (1) to (4) as SOL, then (5) the relay's TLS connection to `<bmc-ip>:443` and `POST /cgi/login.cgi`, one TCP plus one TLS handshake plus one request: **Inferred** 1 to 2 s on an X10 (a slow CPU) ; (6) the client serves the cookie page and opens the browser at the tunnel, a local round trip; (7) the browser's TLS handshake to the BMC through the tunnel (2 RTT-t plus RTT-l), the page load, then the websocket upgrade; (8) the first frame. Budget: 3 to 6 s to the first frame, dominated by the BMC page load. Video is a few Mbit/s and crosses the coordinator in both directions.

### 9.3 Power action and boot device

![Power action: sequence](diagrams/console-power-sequence.svg)

No stream. Hops: (1) `Console.Power` with the token and `reason`; (2) the coordinator checks `host_live` and writes the audit line; (3) the open command reaches the relay (held poll); (4) address (cache hit or probe); (5) `ipmitool chassis power ...`, an RMCP+ session and one command, **Inferred** 1 to 3 s; (6) the relay returns the result (`POST /console/v1/relay/<id>/result`) and the RPC answers. Budget: under 5 s. The answer is the BMC's acknowledgement, not the host's state: `cycle` answers when the BMC accepted it. The operator watches `Hosts.Get` `live` come back.

### 9.4 Open a Mac screen

![Open a Mac screen: sequence](diagrams/console-mac-screen-sequence.svg)

Hops: (1) to (4) as SOL; (5) the relay's TCP connect to `<mac-ip>:5900`, RTT-l, and the banner read (`RFB 003.889`), one more RTT-l; (6) the client's viewer connects to `127.0.0.1:<p>` and RFB runs end to end through the splice: version, the Mac's auth (Apple type, the Mac's own user and password), then the first framebuffer update. Budget: under 3 s to the login prompt; the first frame after login is the Mac's and the viewer's, **to measure** (C0). Added latency per input event: two tailnet legs through the coordinator, **Inferred** the sum of the client-to-coordinator and coordinator-to-relay one-way times.

## 10. Failure modes and edge cases

| Case | Mechanism |
|---|---|
| BMC unreachable (powered off, cable, wrong subnet) | The device check in section 8 fails after one probe: `device_not_found`, 1 s plus the checks. The audit open line is already written; the close line says `device_not_found`. |
| MAC moved or DHCP address changed | A cached address fails its device check, the entry is dropped, one probe re-resolves; second failure `device_not_found`. |
| One MAC maps to several addresses | The device check picks the one that answers on the right port. |
| No relay connected | `no_relay`. Presence is "a long poll held within 2 poll periods". |
| Relay host is the target (power cycle of the host that runs the only relay) | A relay never answers for its own `--host-id`; with a second relay, that one serves. With only one, `Console.Power` is refused `no_relay_other_than_target` before the audit line. A relay that is cycled by a second relay loses its sessions: its long poll stops, the coordinator closes its sessions `relay_lost` and writes their close lines. |
| A host reaching its own BMC through a shared NIC | Many BMCs cannot be reached from their own host. The same rule avoids it; the lab's NIC mode is unmeasured (C0). |
| Two operators on one SOL | D13: `console_busy`, or `--takeover`. |
| SOL shows nothing | No `console=ttyS` on the kernel, or BIOS redirection off (section 5.1). Not detectable by the broker; the doc says so and the audit shows the session opened. KVM is the screen. |
| Wrong shared credential | `ipmitool` or the login fails; the relay maps it to `bmc_auth_failed` and returns no text from the tool. The operator sees that token only. A fix is the coordinator's credential file and a reload (`--tls-reload-secs`, as tokens). |
| Credential leak into logs or errors | The credential is only in the child's environment and the login request body. Logs carry the closed error token. A test asserts the full error message for a wrong password by exact equality (T13). |
| Operator token revoked mid-session | The coordinator re-reads tokens every `--tls-reload-secs` and closes that name's sessions `token_revoked`. |
| Operator disconnects | The splice ends, the relay closes the device side (`sol deactivate`, so the next open is not `busy`), the close line is written. |
| Ticket stolen or replayed | One use, 60 s, bound to the token name; a second redeem is `ticket_used`. |
| Audit unavailable | No audit line, no session or power action (`audit_unavailable`); a close is still attempted. |
| Mac screen black | Screen Sharing enabled but not approved. The relay reads the RFB banner only to prove the port is open. The cause is outside the broker; section 11 removes it for MDM-managed Macs. |
| Mac port closed | `mac_screen_sharing_off` (connection refused); the operator enables it at the Mac or by MDM. |
| Coordinator restart | All sessions end; relays re-poll at once. Tickets are in memory only, so unredeemed tickets die. |
| Session cap | `console_busy`-style refusal at `--console-max-sessions`; a splice is two sockets and a few KiB of buffer, so the cap is for the BMCs, not the coordinator. |

## 11. Threat model

[hostkeeper-design.md](hostkeeper-design.md) section 5.1 already says a compromised coordinator is root on any host through signed packages. This adds a row to "what a compromise gets": **raw BMC power, KVM and every Mac's screen** (the Mac's login still stands), and the shared BMC password, held by the coordinator and the active relay. A stolen operator token with `console = true` reaches the same consoles, and the audit shows its name. The guard is the coordinator, per the owner. A per-host or rotating secret would change none of this for an attacker who owns the coordinator, which is why the design does not add one. Keeping the BMCs off the tailnet is what makes the guard real: do not advertise the lab subnet as a tailnet route. Supermicro's iKVM has had weak launch tokens (a 2026 CVE report, **Unverified**), another reason the BMC is reached only through the relay.

## 12. How it fits MDM

The Macs join MDM ([hostkeeper-mdm.md](hostkeeper-mdm.md)), on Path B for the farm's existing Macs. Section 6 of that document already lists Remote Desktop as "yes" under A and B1 and "only if supervised" under B2. The design adds:

1. **`EnableRemoteDesktop`** (Apple's `remotedesktop.enable`) as a sixth entry of `COMMAND_TYPES` in `crates/hostkeeper-mdm-gate/src/command.rs`, and an update of `the_gate_enqueues_only_its_five_command_types` (renamed for six). The gate sends it only to a Mac that `IsSupervised` reports supervised (**Built** check in `answer.rs`). With it, Screen Sharing works with no click and the black screen goes away. What the command sets, and whether it covers every local user, is **Unverified** (Apple's page was not available when this was written); task C11 reads Apple's MDM reference first.
2. **`Console.OpenMac` on an MDM-managed Mac** that has not been enabled enqueues `EnableRemoteDesktop` once (idempotent, one audit line `mdm_enable_remote_desktop`), then opens. Until the Macs are in MDM, the operator approves at the Mac once.
3. **The Mac's MAC** from `DeviceInformation` (`WiFiMAC`, `EthernetMACs`) instead of the operator typing it (section 8). Additive queries, like the gate's existing three.
4. **Where the coordinator runs.** The gate talks to NanoHUB on loopback only (`--mdm-api-url`), and "a coordinator elsewhere is a later change" ([hostkeeper-mdm.md](hostkeeper-mdm.md) section 3). The farm's coordinator is on the dev box. Items 1 to 3 therefore need either the coordinator and NanoHUB on one host, or the loopback rule replaced by an authenticated remote API URL. Q1.

The openable Macs are the hosts with platform Mac; nothing new is stored.

## 13. Tests, per piece

Hermetic and deterministic (T12): no network, no real BMC or Mac, no fixed ports, no sleeps (T10). Timers use an injected clock; the latency budgets in section 9 are design figures, **not tests**, and are measured once by hand in C0. All names **(to build)**.

- **`hostkeeper-console`** (C2): `a_ticket_redeems_once`, `a_ticket_expires_at_sixty_seconds_of_the_injected_clock`, `a_ticket_is_bound_to_token_host_and_kind`, `a_second_sol_open_is_busy_and_names_the_holder`, `takeover_closes_the_first_and_audits_it`, `idle_and_total_timers_close_a_session`, `an_error_message_never_contains_the_credential` (exact equality).
- **Tokens and audit** (C1): `a_token_without_console_is_refused`, `an_older_tokens_file_still_parses` (B8: the previous release reads the new file and this one reads the old), `console_open_is_not_made_when_the_audit_line_cannot_be_appended`.
- **Listener and splice** (C3): over an in-memory transport with a scripted relay: `a_held_poll_is_not_closed_at_the_patience`, `bytes_cross_the_splice_both_ways_unchanged`, `a_relay_token_cannot_attach_as_an_operator_and_conversely`, `a_lost_relay_closes_its_sessions`, `the_session_cap_is_enforced`.
- **Resolver** (C5): `a_complete_arp_entry_resolves`, `a_miss_probes_once_then_reads_again`, `several_addresses_pick_the_one_that_answers`, `a_stale_cache_entry_is_dropped_and_re_resolved_once` (fake table and fake clock).
- **ipmitool port** (C6, C7): a scripted double returns each outcome: `power_cycle_builds_exactly_the_typed_command`, `a_wrong_password_maps_to_bmc_auth_failed_without_the_tools_text`, `the_credential_is_in_the_childs_environment_and_not_in_argv`, `sol_deactivates_when_the_operator_disconnects`.
- **KVM** (C8): against a fake BMC web server: `the_login_returns_only_the_sid`, `the_cookie_page_sets_the_cookie_and_redirects_to_the_tunnel`, `tunnel_bytes_are_not_rewritten`.
- **Mac** (C5): against a fake RFB peer: `the_banner_proves_the_port_and_the_stream_is_spliced_unchanged`, `a_refused_connection_is_mac_screen_sharing_off`.
- **Gate** (C11): `the_gate_enqueues_only_its_six_command_types`, `remote_desktop_is_sent_only_to_a_supervised_mac`.
- **Coverage.** Each touched crate at 99% lines and branches (T1, T3, T4); the relay's `main.rs` only forwards to its library. The real `ipmitool` and the real BMC are never run by a test.

## 14. Tasks, one pull request each, in order

Interfaces land before the code that uses them (P7). Sizes: S under a day, M one to three days, L three to five.

| # | Task | Size |
|---|---|---|
| C0 | Lab spike, no code merged: add inventory records for the six nodes; on a node other than the target run `ipmitool` SOL and power; walk the KVM login and cookie hand-off by hand; check BIOS redirection and the shared-NIC question; connect Screen Sharing through a plain TCP forward; measure every **Inferred** figure here. Results go into this document. | S |
| C1 | `console.proto` (`Console.Open`, `Power`, `SetBootDevice`, `List`, `Close`), the `console` token key, the audit action names. Additive; compat baseline updated. | S |
| C2 | `hostkeeper-console` crate: tickets, session table, timers, error enum, with fakes. | M |
| C3 | Coordinator console listener, relay long poll, splice, relay tokens; a scripted relay in tests. | L |
| C4 | `hostkeeper console` client: ticket, attach, local port. | M |
| C5 | `hostkeeper-console-relay` with the resolver and the Mac/TCP target. **First vertical slice: a Mac's screen works end to end.** | M |
| C6 | Relay ipmitool port: `Power` and `SetBootDevice`. Fixes the incident. | M |
| C7 | SOL. | M |
| C8 | KVM tunnel and server-side login. | M |
| C9 | `docs/operations/console.md`; the relay's package and unit. | S |
| C10 | Web page: a console route outside `ui/`, its own static root, scan and CSP, a ws crate. Only if wanted after C4 is in use. | L |
| C11 | MDM: `EnableRemoteDesktop`, `WiFiMAC` and `EthernetMACs`, `Console.OpenMac` enabling. After the Macs are in MDM. | M |
| C12 | Native RMCP+ SOL and power in place of `ipmitool`. Only if the tool becomes a burden. | L |

Stale text to fix in the same pull request as C6 (T19, P10): [hostkeeper-design.md](hostkeeper-design.md) sections 5.7 ("The BMC MAC's later use") and 12.1 (the BMC bullet), and [network-install.md](network-install.md) section 10 (BMC power and boot-device control), each pointing here.

## 15. Questions for the maintainers

- **Q1.** The coordinator is on the dev box and NanoHUB's API is loopback-only. For section 12, do we put the coordinator and the MDM host together, or replace the loopback rule with an authenticated remote URL? Section 12 items 1 to 3 wait on the answer; C1 to C10 do not.
- **Q2.** Which lab host runs the relay, and which is the second? Two are needed for section 10's "relay host is the target". Any node on the lab LAN will do; the dev box does if it has an interface on that LAN (not verified).
- **Q3.** Is `console = true` per token enough, or does a console need a second operator's confirmation for `off`, `cycle` and `reset`? The design assumes enough; the audit line and `live_ok` are the check.
