# The console: setting it up, and moving a server to the serial baseline

What an operator does so that the coordinator's **Console** works: a Linux server's serial port
through its BMC's Serial over LAN (SOL), and its power and boot device through the same BMC, from
the web app's Console page and host page or from `hkctl console`. The design, and the reason for
every rule below, is [console-broker.md](../design/console-broker.md) (cited here as "the
design"); the Console page is described in [ui-serving.md](../ui-serving.md), every `hkctl
console` command in [cli.md](../cli.md#console), and the coordinator's package and its files in
[package.md](../deploy/coordinator/package.md).

The parts, and where each is set up:

| Part | Section |
|---|---|
| The coordinator's host reaches every BMC on UDP 623 | [The network path](#the-network-path) |
| The coordinator finds each BMC's address, which DHCP may move | [Each BMC's address](#each-bmcs-address) |
| Who may use the console, the one BMC credential, an address that overrides | [The three files](#the-three-files) |
| The coordinator can run `ipmitool` | [`ipmitool` beside the coordinator](#ipmitool-beside-the-coordinator) |
| The BMC carries the serial port at 115.2 kbit/s, and the firmware's screens reach it | [The BMC and the BIOS](#the-bmc-and-the-bios) |
| The installed system's console, GRUB and login prompt are on `ttyS1` at 115200 baud | [Moving a server to the serial baseline](#moving-a-server-to-the-serial-baseline) |

The commands of `hkctl` below reach the coordinator's private listener, as in
[enrollment.md](enrollment.md): `https://<private-listener>` is its address, `<ca.pem>` the CA
its certificate chains to, and `<token-file>` a file holding an operator token. The examples use
the documentation addresses `192.0.2.0/24` for the BMCs and `00:00:5e:00:53:xx` for MACs; put
your own in their place. The BMC credential is in no example: it is typed at a prompt or read
from its file, and never put on a command line, in a repository or in a log.

## The network path

The coordinator dials each BMC itself, on UDP 623 (RMCP+), at the address it finds for it
([Each BMC's address](#each-bmcs-address)). **Default: direct.** The coordinator's host has an address on the lab LAN that the BMCs
are on, so no route, router host or tailnet rule is needed. The operator's browser reaches the
coordinator over the tailnet as for every other page; the BMCs are never exposed to it.

**The coordinator's host must stay on the lab LAN.** Moved off it, every console call ends
`device_unreachable` (a stream closes 4001) until the alternative below is set up. When every
BMC fails the same way, check this first. From the coordinator's host, as any user, with the
credential's user and password (the password typed at the prompt, not on the command line):

```sh
read -rs IPMI_PASSWORD && export IPMI_PASSWORD
ipmitool -I lanplus -C 3 -L ADMINISTRATOR -H 192.0.2.12 -U <bmc-user> -E chassis power status
unset IPMI_PASSWORD
```

`Chassis Power is on` (or `off`) says the path and the credential are good. `Unable to establish
IPMI v2 / RMCP+ session` with no other line is no answer: the address, the cable, or the host
off the LAN.

**Alternative, only for a coordinator moved off the lab LAN: a Tailscale subnet route with an
ACL** (the design, section 9.1).

1. A lab host that is always on advertises the lab subnet (`tailscale set
   --advertise-routes=<lab-subnet>`, with IP forwarding on). A second lab host advertises the same
   route, so the path survives one of them being power-cycled. Traffic through the route is
   source-translated, so the BMCs need no route back.
2. The coordinator's host accepts routes (`tailscale set --accept-routes`).
3. The tailnet policy grants **only the coordinator's tag** the BMCs' addresses on UDP 623: one
   rule from `tag:<coordinator>` to `<bmc-range>:623`, and no rule that grants any other member
   the subnet. A rule that does exposes every BMC to anyone who holds the shared password, with no
   coordinator check and no audit line. The rule names the range the lab's DHCP server gives the
   BMCs. Through the route, the coordinator's neighbour table holds no BMC: a BMC whose host
   never reported its address is `no_address` unless the targets file names it.

## Each BMC's address

The lab's DHCP server cannot reserve an address, so a BMC's lease may move: the coordinator finds
each BMC's address rather than being told it once. A BMC is known by its MAC, the `bmcMac` of the
host's inventory record (`hkctl enrollment list-records`); a host whose record has none is
`no_bmc_mac`: add the record's BMC MAC ([enrollment.md](enrollment.md)).

```sh
hkctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list-records
```

The coordinator tries these addresses, in order, until a BMC answers at one:

1. **The targets file**, when it names the BMC's MAC: its address alone, an override, for a BMC
   whose host runs no hostkeeper daemon (or one an operator pins). Optional; see
   [The three files](#the-three-files).
2. **The host's own report.** The hostkeeper daemon on each Linux server, as root, reads its
   BMC's address and MAC from the BMC itself, in-band over `/dev/ipmi0` with no credential (what
   `ipmitool lan print 1` shows as `IP Address` and `MAC Address`), at its start and every ten
   minutes, and reports them in its heartbeats. The coordinator keeps the last report with the
   host's enrollment, so every coordinator on the same database has it, after a restart too. A
   report of another MAC than the record's `bmcMac` is not used. While the host is not
   heartbeating (no heartbeat for 45 seconds, as when it is off or hung, the reason to open its
   console), this is the address it last reported, and it may be stale: the answer says so.
3. **The coordinator's neighbour table** (`/proc/net/arp` of the coordinator's host), when it
   holds a complete entry for the BMC's MAC at another address. Nothing is probed to fill it: it
   holds a BMC the coordinator's host spoke to within the last few minutes.

With none of them the host is `no_address`. Each answer of `hkctl console power`,
`set-boot-device` and `open` names the address and where it came from: `address` with `source`
`CONSOLE_ADDRESS_SOURCE_TARGETS_FILE`, `_HEARTBEAT` (with `stale` true while the host is not
heartbeating, and `reported_unix`) or `_NEIGHBOR_TABLE`.

An address is never trusted for what is at it: before every command the coordinator reads the MAC
of the BMC at the address and compares it with the record's `bmcMac`. A difference sends nothing
there, and the next address is tried; at the last one the call is `address_mismatch`, or
`device_unreachable` when a BMC did not answer at one of them. A host that heartbeats again
reports its BMC's new address within ten minutes.

## The three files

The console is off until the coordinator is given the operators file and the credential file; the
targets file is optional. Each is a flag of `serve`, written as a key of `serve.toml`
([package.md](../deploy/coordinator/package.md#the-flags-servetoml)): `console-operators-file`,
`console-targets-file` and `console-bmc-credential-file`. The operators file needs the credential
file beside it (`serve` refuses it without); with no operators file nobody has console access and
the other two are not read. The coordinator reads all three again
on the reload tick (`tls-reload-secs`): a change takes effect with no restart, and a file refused
on that look is logged, naming its flag, and the one read before stays in use. A file refused at
the start stops the coordinator, naming its flag and never its path.

**Who may use the console** (`console-operators-file`; root's, readable by the coordinator's group,
no secret). One `[[operator]]` for each name that may use it: a user's name, for the web app's
sign-in and for every API token that user owns (an API token is checked as its owner, never under
its own name), or a token's name of the tokens file for `hkctl`. `power` is true unless the entry says
false; an operator with `power = false` may open, list and close consoles but not change power or
the boot device (`power_not_permitted`). A name that is not in the file has no console access
(`no_console_access`).

```toml
# /etc/hostkeeper-coordinator/console-operators.toml
[[operator]]
name = "alice"

[[operator]]
name = "bob"
power = false
```

**An address that overrides** (`console-targets-file`; optional; root's, readable by the group, no
secret). Leave it out unless a BMC's address cannot be found
([Each BMC's address](#each-bmcs-address)): a BMC whose host runs no daemon, or a coordinator
reached through the route of the alternative. One `[[target]]` for each such BMC: `mac` is the
BMC MAC of the host's inventory record, and `address` an IP address or a DNS name, with no port (a
BMC's is 623). A BMC the file names is reached at that address alone, so an address that moved is
`address_mismatch` until the file is fixed. A Mac's screen is reached only at the address this file
gives one of its data MACs.

```toml
# /etc/hostkeeper-coordinator/console-targets.toml
[[target]]
mac = "00:00:5e:00:53:02"
address = "192.0.2.12"

[[target]]
mac = "00:00:5e:00:53:03"
address = "bmc-03.lab.example"
```

**The one BMC credential** (`console-bmc-credential-file`; owned by the coordinator's user, mode
0600 or 0400, a secret: the coordinator refuses it when others can read it). One user and
password that every BMC takes; the coordinator holds it, no operator sees it, and it reaches
`ipmitool` in the child's environment only. The user is 1 to 16 bytes of printable ASCII with no
space; the password 1 to 20 bytes of printable ASCII, a space allowed. Make the password long and
random: anyone on the lab LAN who captures one session's handshake can try passwords against it
offline, and the one password opens every BMC.

```toml
# /etc/hostkeeper-coordinator/console-bmc-credential.toml
user = "<bmc-user>"
password = "<long random>"
```

Write it with the editor of your choice into a file made with the right owner and mode first, so
that it is never readable by others, and never through a command line:

```sh
sudo install --mode=0600 --owner=hostkeeper-coordinator --group=hostkeeper-coordinator \
    /dev/null /etc/hostkeeper-coordinator/console-bmc-credential.toml
sudoedit /etc/hostkeeper-coordinator/console-bmc-credential.toml
```

Each BMC must have that user, with the password, the privilege the coordinator asks for
(`console-ipmi-privilege`, `administrator` by default; Serial over LAN alone needs `operator`)
and the SOL payload allowed. On a host that is up, as root, in-band (no credential needed), with
a free user slot `<id>` from `ipmitool user list 1`; `user set password` with no password asks for
it twice at the prompt:

```sh
sudo ipmitool user list 1
sudo ipmitool user set name <id> <bmc-user>
sudo ipmitool user set password <id>
sudo ipmitool channel setaccess 1 <id> callin=on ipmi=on link=off privilege=4
sudo ipmitool sol payload enable 1 <id>
sudo ipmitool user enable <id>
```

A wrong user or password is `bmc_auth_failed`, with no text of the tool in the answer; fix the
file, and the next reload tick takes it.

**Then** add the keys to `serve.toml` (`console-targets-file` only if you wrote one), beside the
defaults that need no key (`console-ipmitool`, `console-ipmi-cipher` 3, `console-ipmi-privilege`
`administrator`, `console-ipmi-timeout-s` 1, `console-ipmi-retries` 2, `console-attach-s` 30,
`console-max-sessions` 8, `console-idle-s` 1800, `console-total-s` 28800,
`console-connect-timeout-s` 5, `console-quiet-s` 10; [package.md](../deploy/coordinator/package.md)
says what each does), and restart the coordinator once:

```toml
console-operators-file = "/etc/hostkeeper-coordinator/console-operators.toml"
console-bmc-credential-file = "/etc/hostkeeper-coordinator/console-bmc-credential.toml"
```

In the container of the image ([README.md](../deploy/coordinator/README.md)), the files are
mounted read-only and named by their paths in the container, as its other files are. The
neighbour table the coordinator reads is its network namespace's: with the container on the
host's network it is the host's, otherwise it holds no BMC.

## `ipmitool` beside the coordinator

The coordinator runs `ipmitool` for every power, boot-device and console call, at the path
`console-ipmitool` names (by default `/usr/bin/ipmitool`). It runs it with a closed argument list,
`-I lanplus` and the cipher suite, privilege, timeout and retries of its flags; the password in
the environment (`-E`), never on the command line.

- **The deb or rpm package** does not carry it. Install the distribution's package on the same
  host: `sudo apt install ipmitool`, or `sudo dnf install ipmitool`. Both put it at
  `/usr/bin/ipmitool`.
- **The image** is distroless (no shell, no package manager) and does not carry it either. Build
  an image of your own that does, from the release image you loaded and verified
  ([README.md](../deploy/coordinator/README.md#the-images)) and the image's own distribution,
  Debian 13, each pinned by its digest; run it in place of the release image (its `image:` line),
  with the same command, mounts and user:

  ```dockerfile
  FROM hostkeeper-coordinator:<version>@sha256:<release image digest> AS release
  FROM debian:trixie-slim@sha256:<digest>
  RUN apt-get update && apt-get install --yes --no-install-recommends ipmitool \
      && rm -rf /var/lib/apt/lists/*
  COPY --from=release /usr/bin/hostkeeper-coordinator /usr/bin/hostkeeper-coordinator
  COPY --from=release /usr/share/hostkeeper-coordinator/ui /usr/share/hostkeeper-coordinator/ui
  USER 65532:65532
  ENTRYPOINT ["/usr/bin/hostkeeper-coordinator"]
  ```

  Check it: `docker run --rm --network none --entrypoint /usr/bin/ipmitool <your image> -V`
  prints `ipmitool version 1.8.19` (or later). The image's own updater replaces only the release
  image; an image you built is updated by building it again from the new release.

A tool that is missing or cannot be run makes every call `bmc_failed`, and the coordinator's log
names the step.

## The BMC and the BIOS

The serial console is the board's second serial port, COM2, which Linux names `ttyS1`, on every
Linux server: the BMC carries COM2 over SOL (the design, section 5.2). Three settings must agree;
the first two are the BMC's and the firmware's, the third the installed system's (next section).

**The BMC's SOL bit rate, 115.2 kbit/s.** The coordinator reads it at every Console open and sets
any rate that differs (`sol set volatile-bit-rate 115.2` and `sol set non-volatile-bit-rate
115.2`), so there is nothing to do by hand. To read it, on the host as root:

```sh
sudo ipmitool sol info 1
```

It shows `Enabled : true`, and `Volatile Bit Rate (kbps)` and `Non-Volatile Bit Rate (kbps)` of
`115.2`. A BMC whose SOL is not enabled: `sudo ipmitool sol set enabled true 1`.

**BIOS console redirection to COM2, at 115200 baud, 8 bits, no parity, 1 stop bit.** Without it
the kernel and the login prompt still reach SOL, but the firmware's own screens (power-on self
test, the network boot menu, setup) do not. Standard IPMI does not carry BIOS settings, and the
boards' Redfish has no BIOS resource, so it can be neither set nor read remotely. It is a one-time
setting per board. **The only check is watching the server boot on the Console:** open the
Console on the host, reboot it (next section, step 5), and look for the firmware's output before
GRUB: screen-control sequences, the network card's boot agent banner, "Press `<DEL>` to run
setup". If nothing comes before the kernel's first line, it is off. To turn it on, open the BIOS
setup (on the BMC's own web console or with a screen at the machine, since SOL cannot show it yet)
and, on a Supermicro X10 board, under Advanced, Serial Port Console Redirection, enable the SOL
port's console redirection with those settings (the menu as Supermicro's manual for the X10 names
it; not yet read on the farm's boards).

## Moving a server to the serial baseline

A server installed from the network with `serial-console = "ttyS1"`
([network-install.md](network-install.md)) has the baseline already: the seed writes the two files
below and runs the three commands of step 3 (the design, section 5.3). A server installed any other
way moves to it by hand once, with this procedure. It is the same for every Ubuntu server; run it
on **one server at a time**.

1. **Take the server out of placement first.** A reboot (step 5) stops every service on it, so
   stop what places work on it and let its jobs drain, and announce the reboot to the people who
   use the server. Change nothing else on a server that is still running jobs.
2. **Find the console settings that would win over the baseline's.** GRUB reads
   `/etc/default/grub` and then each file of `/etc/default/grub.d/` in name order, so a later line
   wins:

   ```sh
   grep -n -E '^[^#]*(console=|GRUB_TERMINAL|GRUB_SERIAL_COMMAND)' /etc/default/grub /etc/default/grub.d/*.cfg
   ls /etc/systemd/system/serial-getty@ttyS1.service.d/
   ```

   Every line the first command prints, but those of `99_hostkeeper.cfg`, is taken out: a file that
   holds nothing but the console (one written by hand earlier) is removed; in a file that holds
   other settings too (a cloud image's `50-cloudimg-settings.cfg`), the `console=` arguments are
   taken out of its kernel line and its `GRUB_TERMINAL` and `GRUB_SERIAL_COMMAND` lines are
   commented out. Take out only the `console=` arguments, not the whole kernel line: a
   `GRUB_CMDLINE_LINUX_DEFAULT` in `grub.d` overrides the one of `/etc/default/grub` (often
   `quiet splash`, which hides the kernel's lines from the Console). A drop-in of the getty other
   than `99_hostkeeper.conf` that sets `ExecStart` is removed. For example:

   ```sh
   sudo rm /etc/default/grub.d/90-serial-console-ttyS1.cfg
   sudo sed -i -E -e '/^GRUB_CMDLINE_LINUX(_DEFAULT)?=/s/ ?console=[^ "]*//g' \
       -e 's/^(GRUB_TERMINAL|GRUB_SERIAL_COMMAND)=/# &/' /etc/default/grub.d/50-cloudimg-settings.cfg
   sudo rm /etc/systemd/system/serial-getty@ttyS1.service.d/10-console-115200.conf
   ```

3. **Write the baseline's two files and run its three commands**, exactly as the seed does
   (a test holds these lines to the seed's own). The GRUB file adds the kernel's serial console
   and GRUB's own serial terminal on COM2 (`--unit=1`); the drop-in holds the login prompt's port
   at 115200 baud (the stock unit's `--keep-baud` moves the port to 9600 at the first Enter, and
   SOL then shows NUL bytes):

   ```sh
   sudo tee /etc/default/grub.d/99_hostkeeper.cfg >/dev/null <<'EOF'
   GRUB_CMDLINE_LINUX="$GRUB_CMDLINE_LINUX console=tty0 console=ttyS1,115200n8"
   GRUB_TERMINAL="console serial"
   GRUB_SERIAL_COMMAND="serial --unit=1 --speed=115200"
   EOF
   sudo install -d /etc/systemd/system/serial-getty@ttyS1.service.d
   sudo tee /etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf >/dev/null <<'EOF'
   [Service]
   ExecStart=
   ExecStart=-/sbin/agetty -o '-p -- \\u' 115200 - $TERM
   EOF
   sudo update-grub
   sudo systemctl daemon-reload
   sudo systemctl enable --now serial-getty@ttyS1.service
   sudo systemctl restart serial-getty@ttyS1.service
   ```

   The last line is the hand step's own: `enable --now` starts a getty that is not running but
   leaves a running one on its old command.
4. **Check before the reboot.** The first command prints the lines of `99_hostkeeper.cfg` alone;
   every kernel line of the generated menu holds `console=tty0 console=ttyS1,115200n8` and no
   other `console=`, and GRUB's serial line is unit 1; the getty runs agetty at 115200 with no
   `--keep-baud`:

   ```sh
   grep -n -E '^[^#]*(console=|GRUB_TERMINAL|GRUB_SERIAL_COMMAND)' /etc/default/grub /etc/default/grub.d/*.cfg
   sudo grep -E '^\s*(linux|serial|terminal_input|terminal_output)\b' /boot/grub/grub.cfg
   systemctl show --property=ExecStart serial-getty@ttyS1.service
   ```

5. **Reboot it from its own shell, and watch the boot on the Console.** Open the host's Console
   (web app, or `hkctl console open` and a WebSocket client) first: the session is the BMC's and
   stays attached through the reboot. Do not reboot a server whose BMC nobody can reach at that
   moment (no Console, no BMC credential): a server that does not come back, or stops in its
   shutdown or in the firmware, can then only be found at the machine. Then `sudo systemctl reboot` on the server. Do not
   power-cycle it through the BMC to speed it up: a shutdown can take minutes while storage
   services stop, and a cycle cuts it. What to see, in order: the firmware's output if BIOS
   redirection is on ([The BMC and the BIOS](#the-bmc-and-the-bios)); the kernel's lines; the
   login prompt.
6. **Check after the reboot**, on the server:

   ```sh
   cat /proc/cmdline
   sudo stty -F /dev/ttyS1 speed
   ```

   The command line holds `console=tty0 console=ttyS1,115200n8` and no other `console=` (the
   last `console=` is the one the kernel's own messages and `/dev/console` go to). Press Enter twice on the
   Console: the login prompt comes back readable each time, and the port's speed is still
   `115200`.
7. **Put the server back into placement**, and check that its services are running again.

GRUB's menu stays hidden (`GRUB_TIMEOUT_STYLE=hidden`, `GRUB_TIMEOUT=0` on the farm's servers), as
the seed leaves it: a visible menu would cost its timeout on every boot. To reach the menu for one
boot, set `GRUB_TIMEOUT_STYLE=menu` and `GRUB_TIMEOUT=5` in `99_hostkeeper.cfg`, run `sudo
update-grub`, reboot, and take the two lines out again after.

## A Serial over LAN session left behind

A BMC carries one SOL session. The coordinator ends the SOL session of each of its streams when the
stream ends, and of every stream when it stops; only a coordinator that died without its drain
(killed, or its host lost power) leaves one active at the BMC. The coordinator never ends a session
it did not start, so every later Console open of that host closes `bmc_sol_busy` (4002) until the
session is ended by hand. First make sure nobody holds it: `hkctl console list` names no session
of the host, and nobody in the lab is using SOL on that BMC. Then, on the host as root, in-band:

```sh
sudo ipmitool raw 0x06 0x4a 0x01
sudo ipmitool sol deactivate
```

The first command reads the SOL payload's state: `01 01 00` is a session active, `01 00 00` none.
With the host down, run `sol deactivate` from the coordinator's host instead, as in
[The network path](#the-network-path), with `sol deactivate` in place of `chassis power status`.

## When something does not work

| What you see | Why, and the fix |
|---|---|
| A blank terminal, and after `console-quiet-s` "No output yet: the host's serial port must be ttyS1." | The installed system's console or login prompt is not on `ttyS1`. [Moving a server to the serial baseline](#moving-a-server-to-the-serial-baseline). |
| A prompt at the first Enter, then nothing or NUL bytes | The stock getty moved the port to 9600. The getty's drop-in, step 3. |
| The kernel and the login, but nothing of the firmware | BIOS console redirection to COM2 is off. [The BMC and the BIOS](#the-bmc-and-the-bios). |
| `device_unreachable` (4001) on every BMC | The coordinator's host is off the lab LAN, or the route of the alternative is down. [The network path](#the-network-path). |
| `address_mismatch` (4009) | The BMC at every address found for the host is another one: a targets file entry that is out of date, or a record whose `bmcMac` is not the BMC's. [Each BMC's address](#each-bmcs-address). |
| `no_address` | No address is known for the BMC: its host never reported one (its daemon is not running as root, or the host has been down since before it first did) and the neighbour table has none. Power the host from the BMC's own page, or name the BMC in the targets file. [Each BMC's address](#each-bmcs-address). |
| `bmc_auth_failed` (4004) | The credential file's user or password is not the BMC's. [The three files](#the-three-files). |
| `bmc_sol_busy` (4002) | Someone holds the BMC's SOL session, or one was left behind. [A Serial over LAN session left behind](#a-serial-over-lan-session-left-behind). |
| `bmc_busy` (4003) | The BMC's 16 sessions are taken; one a client left is freed by the BMC within a minute. |
| `bmc_failed` | `ipmitool` is missing or failed. [`ipmitool` beside the coordinator](#ipmitool-beside-the-coordinator). |
| `no_console_access`, `power_not_permitted` | The operators file. [The three files](#the-three-files). |
