# Network gear in the inventory and the racks

Status: design, 2026-11-13, for komira-ai/hostkeeper. Built by the PRs named at the end. Owner direction: "for the first rack, it's a 24u
with 6 4u servers. That's the spoolr nodes. We also have a BMC switch and a data switch. I think we have to model that as well? And put
them in the rack as well? But again that's something we can add to inventory because there is a console port on it."

## 1. Problem

The row view draws a rack from servers only. The first real rack is a 24 U cabinet holding six 4 U spoolr nodes and two switches: one
for the BMC network, one for the data network. The switches have no record anywhere, so the rack drawn is not the rack in the lab, and a
switch's console port (a serial cable from one of the hosts) is written nowhere. A switch is not a server: it enrolls nothing, has no
host id, no data MAC pair, no state hostkeeper hears. It must not be forced into the server entry, which stays exactly Server name,
BMC MAC, one data MAC, Notes.

## 2. Solution in one paragraph

Inventory gets a Kind: Server | Switch. A Server is what it is today and nothing in it changes. A Switch is a new record, a *network
device*, with Name, Role (BMC network | Data network), Console, Management MAC (optional) and Notes. It places in a rack like any
inventory item (rack, U, height; a switch is typically 1 U) through the same `Layout.Place` call and the same subject
`inventory:<id>`. The row view draws it as a switch faceplate (a port strip), with no health light because hostkeeper does not know a
switch's state. A click opens a small switch page: its fields and its placement.

## 3. Data model (the coordinator)

A network device is part of the lab's layout model, not of the enrollment inventory:

```
NetworkDevice {
  id            net.<n>          made by the coordinator, never changed; n counts up from 1
  name          1 to 32 of letters, digits, space . _ -   (the cluster-name rule); no two devices share a name
  role          BMC_NETWORK | DATA_NETWORK
  console_host  host id of the host the console cable ends at, or empty
  console_device the device on that host (/dev/ttyUSB0), or empty; both set or both empty ("none")
  management_mac optional, lower-case colon-separated
  notes         0 to 256 bytes, the inventory's notes rule
}
```

Why in the layout and not in the inventory store: the inventory record is read strictly by the release before (an unknown field is
refused), and every change to it is shaped by enrollment (windows, matching, groups) that a switch has none of. The layout file is read
the way the accounts file is (an unknown key is skipped), and Postgres gets one more table. Both are additive (B8). The cost: a
release before this one that rewrites the layout file drops `devices`; only a rollback followed by a layout edit does that, and
Postgres keeps its table.

Placement: the existing placement, subject `{INVENTORY, "net.<n>"}`. The id of a device has a `.`, which no host id (a DNS label) has,
so a device never collides with a server entry, and no change to the placement model, its table or its constraint is needed. A device
removed takes its placement with it in the same change, so no placement of a removed device stays.

Limits: at most 256 devices. A U is held by one placement, so a switch and a server never share a U (existing `overlap`).

Storage: file store `<state-file>.layout` gains an optional `devices` object; Postgres migration `0016_lab_devices.sql` (`may-run`):
`lab_devices (id text primary key, record jsonb)` with the id pattern as a check. Backup, export and counts follow the layout's.

## 4. API (operator proto, `layout.proto`, additions only)

- `message NetworkDevice` and `enum NetworkRole` (`NETWORK_ROLE_UNSPECIFIED`, `BMC_NETWORK`, `DATA_NETWORK`).
- `GetLayoutResponse.devices = 5` (`repeated NetworkDevice`), by name.
- `rpc PutDevice(PutLayoutDeviceRequest) returns (PutLayoutDeviceResponse)`: empty `id` adds, else replaces every field of that device.
- `rpc RemoveDevice(RemoveLayoutDeviceRequest) returns (RemoveLayoutDeviceResponse)`: removes the device and its placement.
- `LayoutRefusal.UNKNOWN_DEVICE = 14` (NOT_FOUND); a name taken is `name_taken` (already in the enum, unused until now), a bad field is `invalid_request`,
  more than 256 is `capacity`.
- Audit actions `layout_device_put`, `layout_device_remove`, each with the optional reason, as every layout write.
- Not changed: every existing message, field, refusal; `Enrollment.*Record` (a server entry); `Place`/`Unplace` (a device is a subject).

`hkctl` and the UI client (`ui/gen/`) pick the two calls from the generated tables.

## 5. UI

- **Inventory**: a Kind switch, Server | Switch. Server shows today's table, unchanged. Switch shows a table of switches (Name, Role,
  Console, Management MAC, Notes, Where) with Edit and Remove and an add row. Role is a pick list. Console is two picks: the host (enrolled
  hosts, from `Hosts.List`, or "none") and the device (a short list of the usual: `/dev/ttyUSB0` to `/dev/ttyUSB3`, `/dev/ttyS0` to
  `/dev/ttyS3`, `/dev/ttyACM0`, `/dev/ttyACM1`), no free text. Placement is the same four choices as a server's (Height, Row, Rack, U;
  1 U by default; "Unplaced" first), made in the same step as the add.
- **Row view**: a switch is a faceplate distinct from a server: a flat dark plate with a strip of RJ45 port cells (two rows on a 2 U),
  a small role tag (BMC or Data) in the accent, its name, and a console jack mark when a console is set. No health light, no state
  word. Hover says name, role and console. A click opens the switch page.
- **Switch page** (`switch`, route not in the menu, crumb `Lab > Row > Rack > Switch`): a header with the name and role, a small
  elevation of the rack with its U lit, a table of its fields (Console as "ttyUSB0 on node-03"; a link to that host when enrolled), and
  Edit (to Inventory, Switch kind, that row open) and Remove.
- Edit layout on Home moves a switch like a server (pick up, put on a free U).

## 6. What the console field enables later (not built)

Console names where the serial cable ends: a host and a device. The coordinator could later open a serial console to a switch
through that host's hostkeeper (an `ssh`-free path in the style of the console broker design), with the same audit and reason rules as
a server's BMC console. This change builds the record only: nothing opens a device, nothing probes it, and the coordinator does not
check that the host or the device exists (as it does not for a placement's subject).

## 7. Open questions and the defaults chosen

1. Switch records live in the layout, not the inventory store. Default: layout (additive and rollback-safe on Postgres). The
   alternative is a new inventory store; it costs a second store for no behaviour gain.
2. Console is two fields (host, device), not one text. Default: two, validated together; the device pattern is `/dev/` and 1 to 48
   of letters, digits, `.`, `_`, `-`, `/` with no `..`.
3. Remove of a placed switch: removes its placement too. Default: yes (a switch is no server to keep a place across a wipe).
4. Heights: a switch takes 1 U to 4 U, 1 U default. Default: the same four choices as a server.
5. Management MAC: optional, one only, never matched against anything (no enrollment match, no PXE). Default: stored and shown.
6. Does Home count a switch in "U in use"? Default: yes (it holds a U); it is not in any server count or state strip.
7. Should a server's console cable be recorded the same way? Default: no; the server entry stays four fields.

## 8. Tests (named in the PRs)

Coordinator: the layout rules for devices (names, role, console pair, MAC, notes, capacity, id), the put and remove changes with
their placement, both stores (files and Postgres) round trip, an older layout file without `devices` loads empty, backup and export carry
the devices. Proto: the additions test (an older reader keeps the new bytes, this build reads an old `GetLayoutResponse`). UI: the
inventory Kind switch, the switch form checks, the faceplate and the switch page.

## 9. Delivery

Two PRs, stacked: (1) the API (built, komira-ai/hostkeeper#373): proto, coordinator, stores, migration `0016`, generated client, docs; it lands alone and
changes nothing a user sees; (2) the UI on top of it (built): Inventory Kind (`ui/pages/inventory.js`), the faceplate (`ui/pages/home.js`, `ui/styles/gear.css`), the switch page (`ui/pages/switch.js`); tests in `crates/hostkeeper-ui-tests/tests/network_gear_page.rs`.

Tests of PR 1: `crates/hostkeeper-coordinator/src/layout/device_tests.rs`, `operator_layout_tests.rs` (devices), the Postgres and
export tests (`rich_layout` now holds a placed device), `crates/hostkeeper-proto/tests/additions_network_devices.rs` (rule B8, both
directions).
