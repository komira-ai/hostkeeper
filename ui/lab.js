// The lab: the model the Home page and the pages that place a server share. The coordinator keeps
// the lab's layout (`Layout.Get`: the cluster's name, its numbered rows, the numbered racks of each
// row with their height in U, the sleds in the racks and the items standing on the slots of the
// sleds); this module reads it, joins it with the hosts and the inventory entries, and answers the
// questions the views ask: which server stands where (its address), in what state, which U of a
// rack are free for a sled of a given height, which slots are free, and in words, why the
// coordinator refused a change. A sled is what goes in a rack (docs/design/rack-sleds.md): it has
// the U and the height, and 1 to 8 slots; a server or a switch has no height of its own.
//
// A server is the physical machine. Its place belongs to its inventory entry (so it survives a
// wipe and a new enrollment); a host that no entry matches carries its own. Everything here is a
// pure function of the replies it is given.

import { ApiError } from "./api.js";
import { h, list } from "./view.js";

const KIND_HOST = "LAYOUT_SUBJECT_KIND_HOST";
const KIND_INVENTORY = "LAYOUT_SUBJECT_KIND_INVENTORY";

// The heights a rack is made at, in U (the first is the default), the heights a sled is made at
// (1 to 60 U) and the slots it has (1 to 8): lists to pick from, never typed.
export const RACK_HEIGHTS = [42, 48, 45, 36, 32, 27, 24, 21, 18, 12, 6];
export const SLED_HEIGHTS = Array.from({ length: 60 }, (_, i) => i + 1);
export const SLED_SLOTS = [1, 2, 3, 4, 5, 6, 7, 8];
export const DEFAULT_RACK_HEIGHT = 42;

// The states a server shows, with the words of its pill and its tone.
export const STATES = [
  ["healthy", "ok"],
  ["updating", "info"],
  ["failed", "bad"],
  ["offline", "muted"],
  ["pending", "warn"],
];

// The layout a `Layout.Get` reply holds, with the lists in order and the numbers as numbers:
// `{name, rows, racks, sleds, placements, devices}`; each sled is `{id, rack, number, u, height,
// label, address, slots}` (`u` its lowest U, `number` its place counted from the bottom of its
// rack, `address` as the coordinator computed it: `C0-R0-R0-S2`); each placement is `{kind, id,
// key, rack, u, height, sled, slot, address}` where `key` is `host:<id>` or `inventory:<id>`, `sled`
// the sled it stands on, `slot` its slot (from 0), `address` `C0-R0-R0-S2-S1`, and `rack`, `u` and
// `height` the sled's, repeated; each device (a switch) is
// `{id, key, name, role, consoleHost, consoleDevice, managementMac, notes}`, its `key` the key of
// its placement (a device is placed as an inventory entry, under the id `net.<n>`) and `role`
// `bmc` or `data`.
export function layoutOf(reply) {
  const rows = list(reply, "rows").map((r) => ({ id: r.id, number: Number(r.number) }));
  const racks = list(reply, "racks").map((r) => ({
    id: r.id, row: r.row, number: Number(r.number), height: Number(r.heightU),
  }));
  const placements = list(reply, "placements").map((p) => {
    const kind = p.subject && p.subject.kind === KIND_INVENTORY ? "inventory" : "host";
    const id = p.subject ? p.subject.id : "";
    return {
      kind, id, key: `${kind}:${id}`, rack: p.rack, u: Number(p.u), height: Number(p.heightU),
      sled: p.sled || "", slot: Number(p.slot || 0), address: p.address || "",
    };
  });
  const sleds = list(reply, "sleds").map((s) => ({
    id: s.id, rack: s.rack, number: Number(s.number || 0), u: Number(s.u), height: Number(s.heightU),
    label: s.label || "", address: s.address || "", slots: Number(s.slots || 1),
  }));
  const devices = list(reply, "devices").map((d) => ({
    id: d.id,
    key: `inventory:${d.id}`,
    name: d.name || "",
    role: d.role === "NETWORK_ROLE_BMC" ? "bmc" : "data",
    consoleHost: d.consoleHost || "",
    consoleDevice: d.consoleDevice || "",
    managementMac: d.managementMac || "",
    notes: d.notes || "",
  }));
  return {
    name: typeof reply?.name === "string" ? reply.name : "",
    rows: rows.sort((a, b) => a.number - b.number),
    racks: racks.sort((a, b) => a.number - b.number),
    sleds: sleds.sort((a, b) => a.u - b.u),
    placements,
    devices,
  };
}

// The network devices of the lab, each joined with its place: `{key, kind: "inventory", id, name,
// device, placement}` (the server shape, so a rack draws both from one list), `placement` null when
// it stands in no rack. They are no server: they have no state, host or group.
export function gearOf(layout) {
  const placed = new Map(layout.placements.map((p) => [p.key, p]));
  return layout.devices.map((device) => ({
    key: device.key, kind: "inventory", id: device.id, name: device.name, device,
    placement: placed.get(device.key) || null,
  }));
}

// What a device's role is called, and the roles of the pick list: `[value, words]`.
export const NETWORK_ROLES = [["NETWORK_ROLE_BMC", "BMC network"], ["NETWORK_ROLE_DATA", "Data network"]];
export function roleWords(role) {
  return role === "bmc" ? "BMC network" : "Data network";
}

// The console ports to pick from (never typed), and what a device's console is in words:
// "ttyUSB0 on node-3", or "none".
export const CONSOLE_DEVICES = [
  "/dev/ttyUSB0", "/dev/ttyUSB1", "/dev/ttyUSB2", "/dev/ttyUSB3",
  "/dev/ttyS0", "/dev/ttyS1", "/dev/ttyS2", "/dev/ttyS3", "/dev/ttyACM0", "/dev/ttyACM1",
];
export function consoleWords(device) {
  return device.consoleHost === "" ? "none" : `${device.consoleDevice.replace(/^\/dev\//, "")} on ${device.consoleHost}`;
}

// The request subject of a server: `{kind, id}` as the API spells it.
export function subjectOf(server) {
  return { kind: server.kind === "inventory" ? KIND_INVENTORY : KIND_HOST, id: server.id };
}

// The number a row or a rack is called by: its number less one, since the lab counts from 0 (the
// address `C0-R0-R0-S0-S0`). The ids stay as they are.
export function shownNumber(number) {
  return Number(number) - 1;
}

// "Row 1", "Rack 2" and "Row 1 / Rack 2" for the ids of a layout; an id it does not hold is shown as is.
export function rowName(layout, rowId) {
  const row = layout.rows.find((r) => r.id === rowId);
  return row === undefined ? rowId : `Row ${shownNumber(row.number)}`;
}
export function rackName(layout, rackId) {
  const rack = layout.racks.find((r) => r.id === rackId);
  return rack === undefined ? rackId : `${rowName(layout, rack.row)} / Rack ${shownNumber(rack.number)}`;
}

// A host's state: `pending` for one the coordinator never heard from, `offline` when it is not
// live, `failed` when a service of it reports itself unhealthy or its last update failed, `updating`
// while it holds the update slot of any service, else `healthy`. Only a live host that reports no
// fault is ever `healthy`.
export function hostState(host) {
  if (host.lastHeartbeatUnix === undefined || Number(host.lastHeartbeatUnix) === 0) {
    return "pending";
  }
  if (host.live !== true) {
    return "offline";
  }
  const services = list(host, "services");
  if (services.some((s) => s.health === "HEALTH_UNHEALTHY" || (s.lastUpdate && s.lastUpdate.outcome === "OUTCOME_FAILED"))) {
    return "failed";
  }
  return services.some((s) => s.holdsSlot === true) ? "updating" : "healthy";
}

// The worst of the states of some servers, as the one word a rack or a row shows: `failed`, then
// `offline`, `pending`, `updating`, else `healthy`; `empty` when there is none.
export function worstState(by) {
  for (const state of ["failed", "offline", "pending", "updating"]) {
    if (by[state] > 0) {
      return state;
    }
  }
  return Object.values(by).some((n) => n > 0) ? "healthy" : "empty";
}

// The position a strip of racks shows: given the strip's `scrollLeft`, `scrollWidth` and
// `clientWidth`, the width of one rack with its gap (`pitch`) and the number of racks, the number
// (from 1) of the first rack in view, and whether the strip is at its left or right end.
export function stripPosition({ scrollLeft, scrollWidth, clientWidth, pitch, count }) {
  const first = pitch > 0 ? Math.min(count, Math.round(scrollLeft / pitch) + 1) : 1;
  return {
    first: Math.max(1, first),
    atStart: scrollLeft <= 1,
    atEnd: scrollLeft + clientWidth >= scrollWidth - 1,
  };
}

// The version a host runs of the service named hostkeeper, else of its first service; "" for none.
function versionOf(host) {
  const services = list(host, "services");
  const own = services.find((s) => s.service === "hostkeeper") || services[0];
  return own !== undefined && typeof own.installedVersion === "string" ? own.installedVersion : "";
}

// Every server the lab knows, joined with its place: one for each enrolled host (its entry's
// place when an entry matches it, else its own), one for each inventory entry no host is enrolled
// against (pending), and one for each placement whose host or entry is gone (offline, `gone`).
// A server is `{key, kind, id, name, state, group, version, host, placement}`; `host` is the node
// id when a host is enrolled, `placement` the placement or null.
export function serversOf(layout, hosts, records) {
  const placed = new Map(layout.placements.map((p) => [p.key, p]));
  const seen = new Set();
  const out = [];
  const take = (server) => {
    seen.add(server.key);
    out.push({ ...server, placement: placed.get(server.key) || null });
  };
  const entries = new Set(records.map((r) => r.hostId));
  for (const host of hosts) {
    const entry = host.inventoryRecordId || (entries.has(host.nodeId) ? host.nodeId : "");
    take({
      key: entry === "" ? `host:${host.nodeId}` : `inventory:${entry}`,
      kind: entry === "" ? "host" : "inventory",
      id: entry === "" ? host.nodeId : entry,
      name: host.nodeId,
      state: hostState(host),
      group: host.hostGroup || "",
      version: versionOf(host),
      host: host.nodeId,
    });
  }
  for (const record of records) {
    if (!seen.has(`inventory:${record.hostId}`)) {
      take({
        key: `inventory:${record.hostId}`, kind: "inventory", id: record.hostId, name: record.hostId,
        state: "pending", group: record.hostGroup || "", version: "", host: null,
      });
    }
  }
  const gear = new Set(layout.devices.map((d) => d.key));
  for (const p of layout.placements) {
    if (!seen.has(p.key) && !gear.has(p.key)) {
      take({
        key: p.key, kind: p.kind, id: p.id, name: p.id, state: "offline", group: "", version: "",
        host: p.kind === "host" ? p.id : null, gone: true,
      });
    }
  }
  return out;
}

// The servers standing in `rackId`, lowest U first.
export function inRack(servers, rackId) {
  return servers
    .filter((s) => s.placement !== null && s.placement.rack === rackId)
    .sort((a, b) => a.placement.u - b.placement.u || a.placement.slot - b.placement.slot);
}

// The sleds standing in `rackId`, lowest U first.
export function sledsIn(layout, rackId) {
  return layout.sleds.filter((s) => s.rack === rackId).sort((a, b) => a.u - b.u);
}

// The U in use in a rack: the sum of its sleds' heights (only sleds take U).
export function usedU(layout, rackId) {
  return sledsIn(layout, rackId).reduce((n, s) => n + s.height, 0);
}

// Whether a sled `height` U high with its lowest U at `u` fits in `rackId`: inside the rack and over
// no U of another sled; the sled `except` (an id) is not counted, so it may be moved over its own U.
export function fitsAt(layout, rackId, height, u, except) {
  const rack = layout.racks.find((r) => r.id === rackId);
  if (rack === undefined || !(height >= 1) || !(u >= 1) || u + height - 1 > rack.height) {
    return false;
  }
  return sledsIn(layout, rackId).every((s) => s.id === except || s.u + s.height <= u || s.u >= u + height);
}

// The U (lowest U) at which a sled `height` U high fits in `rackId`.
export function freeSledStarts(layout, rackId, height, except) {
  const rack = layout.racks.find((r) => r.id === rackId);
  const starts = [];
  if (rack === undefined || !(height >= 1)) {
    return starts;
  }
  for (let u = 1; u + height - 1 <= rack.height; u += 1) {
    if (fitsAt(layout, rackId, height, u, except)) {
      starts.push(u);
    }
  }
  return starts;
}

// Where a sled `height` U high dropped on the free U `u` of `rackId` would stand: the lowest start
// whose U include `u` and at which it fits, or null when there is none.
export function dropStart(layout, rackId, height, u, except) {
  for (let start = u - height + 1; start <= u; start += 1) {
    if (fitsAt(layout, rackId, height, start, except)) {
      return start;
    }
  }
  return null;
}

// The numbers of an address, `C0-R0-R1-S2-S0` -> [0, 0, 1, 2, 0], to order addresses by place.
function addressKey(address) {
  return (address.match(/\d+/g) || []).map(Number);
}
export function byAddress(a, b) {
  const x = addressKey(a.address);
  const y = addressKey(b.address);
  for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
    const d = (x[i] === undefined ? -1 : x[i]) - (y[i] === undefined ? -1 : y[i]);
    if (d !== 0) {
      return d;
    }
  }
  return 0;
}

// The slots no item stands on, `[{sled, slot, address}]` by address; the item `except` (a key) is
// not counted, so the slot it stands on is free for it.
export function freeSlots(layout, except) {
  const taken = new Set(layout.placements.filter((p) => p.key !== except).map((p) => `${p.sled}:${p.slot}`));
  const out = [];
  for (const sled of layout.sleds) {
    for (let slot = 0; slot < sled.slots; slot += 1) {
      if (!taken.has(`${sled.id}:${slot}`)) {
        out.push({ sled: sled.id, slot, address: `${sled.address}-S${slot}` });
      }
    }
  }
  return out.sort(byAddress);
}

// The items standing on a sled, by slot.
export function occupants(layout, sledId) {
  return layout.placements.filter((p) => p.sled === sledId).sort((a, b) => a.slot - b.slot);
}

// The address of the item with `key`, "" when it stands nowhere.
export function addressOf(layout, key) {
  const held = layout.placements.find((p) => p.key === key);
  return held === undefined ? "" : held.address;
}

// What the choice of a slot is made from: `[value, address]` for each free slot, `value` being
// `<sled>:<slot>`; with `unplaced`, "Unplaced" ("") first. `slotOf(value)` reads a value back as
// `{sled, slot}`, or null for "" or anything else.
export function slotOptions(layout, except, unplaced) {
  const options = freeSlots(layout, except).map((s) => [`${s.sled}:${s.slot}`, s.address]);
  return unplaced === true ? [["", "Unplaced"], ...options] : options;
}
export function slotOf(value) {
  const match = /^(.+):(\d+)$/.exec(String(value || ""));
  return match === null ? null : { sled: match[1], slot: Number(match[2]) };
}

// A sled in words: its address, its label when it has one, and where it stands.
export function sledWords(layout, sled) {
  const top = sled.u + sled.height - 1;
  return `${sled.address}${sled.label === "" ? "" : ` ${sled.label}`}, ${rackName(layout, sled.rack)}, U${sled.u}${top > sled.u ? `-${top}` : ""}`;
}

// The racks of `rowId`, by number.
export function racksOf(layout, rowId) {
  return layout.racks.filter((r) => r.row === rowId);
}

// How many servers are in each state, as `{healthy, updating, failed, offline, pending}`, counting the
// placed ones only when `placedOnly`.
export function counts(servers, placedOnly) {
  const out = { healthy: 0, updating: 0, failed: 0, offline: 0, pending: 0 };
  for (const server of servers) {
    if (!placedOnly || server.placement !== null) {
      out[server.state] += 1;
    }
  }
  return out;
}

// The words for a refusal of `Layout` or of an approval's placement: what the operator reads
// instead of a bare token. Any other error is shown by its code and token.
const PROBLEMS = {
  overlap: "That place is taken: another sled stands in one of those U, or another item on that slot.",
  out_of_rack: "That does not fit in the rack: it would reach past U 1 or the top U.",
  unknown_rack: "That rack no longer exists.",
  unknown_sled: "That sled no longer exists.",
  unknown_row: "That row no longer exists.",
  unknown_placement: "That server is not placed.",
  in_use: "It still holds something: take what stands on it away first.",
  capacity: "That sled has no free slot.",
  name_taken: "Another switch already has that name.",
  unknown_device: "That switch no longer exists.",
  invalid_request: "The coordinator did not accept that value.",
  placement_overlap: "That slot is taken.",
  placement_out_of_rack: "That does not fit in the rack: it would reach past U 1 or the top U.",
  placement_invalid: "Give the sled and the slot together, or none of them.",
};
export function layoutProblem(error) {
  if (error instanceof ApiError && Object.hasOwn(PROBLEMS, error.reason)) {
    return PROBLEMS[error.reason];
  }
  return error instanceof ApiError ? `${error.code} ${error.reason}`.trim() : "the app failed unexpectedly";
}

// The breadcrumbs of a lab view as buttons: `Lab` (named after the cluster when it has a name),
// then the row, the rack and the host as far as given. `at` is `{row, rack, host}` (ids and a node
// id); each crumb but the last goes there through `go(view, params)`.
export function crumbs(layout, at, go) {
  const rack = at.rack ? layout.racks.find((r) => r.id === at.rack) : undefined;
  const rowId = at.row || (rack ? rack.row : "");
  const trail = [["Lab", "lab", "home", {}]];
  if (rowId !== "") {
    trail.push([rowName(layout, rowId), "row", "home", { row: rowId }]);
  }
  if (rack !== undefined) {
    trail.push([`Rack ${shownNumber(rack.number)}`, "rack", "home", { rack: rack.id }]);
  }
  if (at.host) {
    trail.push([at.host, "host", "host", { nodeId: at.host }]);
  }
  if (at.leaf) {
    trail.push([at.leaf, "leaf", "", {}]);
  }
  return h("nav", { class: "crumbs", "aria-label": "breadcrumbs" },
    trail.map(([label, name, view, params], i) => (i === trail.length - 1
      ? h("span", { class: "here", "aria-current": "page" }, label)
      : h("button", { type: "button", class: "linkish", name: `crumb-${name}`, on: { click: () => go(view, params) } }, label))));
}
