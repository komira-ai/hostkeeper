// The lab: the model the Home page and the pages that place a server share. The coordinator keeps
// the lab's layout (`Layout.Get`: the cluster's name, its numbered rows, the numbered racks of each
// row with their height in U, and the placements of servers in racks); this module reads it, joins
// it with the hosts and the inventory entries, and answers the questions the views ask: which
// server stands where, in what state, which U of a rack are free for a server of a given height,
// and in words, why the coordinator refused a change.
//
// A server is the physical machine. Its place belongs to its inventory entry (so it survives a
// wipe and a new enrollment); a host that no entry matches carries its own. Everything here is a
// pure function of the replies it is given.

import { ApiError } from "./api.js";
import { h, list } from "./view.js";

const KIND_HOST = "LAYOUT_SUBJECT_KIND_HOST";
const KIND_INVENTORY = "LAYOUT_SUBJECT_KIND_INVENTORY";

// The heights a rack is made at, in U (the first is the default), and the heights a server takes.
export const RACK_HEIGHTS = [42, 48, 45, 36, 32, 27, 24, 21, 18, 12, 6];
export const SERVER_HEIGHTS = [1, 2, 3, 4];
export const DEFAULT_RACK_HEIGHT = 42;

// The states a server shows, with the words of its pill and its tone.
export const STATES = [
  ["healthy", "ok"],
  ["updating", "info"],
  ["failed", "bad"],
  ["offline", "bad"],
  ["pending", "warn"],
];

// The layout a `Layout.Get` reply holds, with the lists in order and the numbers as numbers:
// `{name, rows, racks, placements, devices}`; each placement is `{kind, id, key, rack, u, height}`
// where `key` is `host:<id>` or `inventory:<id>` and `u` its lowest U; each device (a switch) is
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
    return { kind, id, key: `${kind}:${id}`, rack: p.rack, u: Number(p.u), height: Number(p.heightU) };
  });
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

// "Row 2", "Rack 3" and "Row 2 / Rack 3" for the ids of a layout; an id it does not hold is shown as is.
export function rowName(layout, rowId) {
  const row = layout.rows.find((r) => r.id === rowId);
  return row === undefined ? rowId : `Row ${row.number}`;
}
export function rackName(layout, rackId) {
  const rack = layout.racks.find((r) => r.id === rackId);
  return rack === undefined ? rackId : `${rowName(layout, rack.row)} / Rack ${rack.number}`;
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
    .sort((a, b) => a.placement.u - b.placement.u);
}

// The U (lowest U) at which a server `height` U high fits in `rackId`, each of its U free of other
// servers; the server `except` (a key) is not counted, so it may be moved over its own U.
export function freeStarts(layout, rackId, height, except) {
  const rack = layout.racks.find((r) => r.id === rackId);
  if (rack === undefined || !(height >= 1)) {
    return [];
  }
  const taken = new Set();
  for (const p of layout.placements) {
    if (p.rack === rackId && p.key !== except) {
      for (let u = p.u; u < p.u + p.height; u += 1) {
        taken.add(u);
      }
    }
  }
  const starts = [];
  for (let u = 1; u + height - 1 <= rack.height; u += 1) {
    let free = true;
    for (let k = 0; k < height && free; k += 1) {
      free = !taken.has(u + k);
    }
    if (free) {
      starts.push(u);
    }
  }
  return starts;
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
  overlap: "Another server already stands in one of those U.",
  out_of_rack: "That does not fit in the rack: it would reach past U 1 or the top U.",
  unknown_rack: "That rack no longer exists.",
  unknown_row: "That row no longer exists.",
  unknown_placement: "That server is not placed.",
  in_use: "It still holds something: remove what stands in it first.",
  capacity: "The lab holds no more of those.",
  name_taken: "Another switch already has that name.",
  unknown_device: "That switch no longer exists.",
  invalid_request: "The coordinator did not accept that value.",
  placement_overlap: "Another server already stands in one of those U.",
  placement_out_of_rack: "That does not fit in the rack: it would reach past U 1 or the top U.",
  placement_invalid: "Give the rack, the height and the U together, or none of them.",
};
export function layoutProblem(error) {
  if (error instanceof ApiError && Object.hasOwn(PROBLEMS, error.reason)) {
    return PROBLEMS[error.reason];
  }
  return error instanceof ApiError ? `${error.code} ${error.reason}`.trim() : "the app failed unexpectedly";
}

// A select whose options are `[value, label]` pairs, chosen by `state[key]`; unlike the bound
// `select` of the shared controls it takes its own name and re-draws nothing.
function chooser(name, state, key, options, redraw) {
  const choices = options.map(([value, text]) => h("option", { value: String(value), selected: String(state[key]) === String(value) }, text));
  const control = h("select", { name }, choices);
  control.on = {
    change: (e) => {
      state[key] = e.target.value;
      for (const choice of choices) {
        choice.attrs.selected = choice.attrs.value === String(state[key]);
      }
      if (redraw) {
        redraw();
      }
    },
  };
  return control;
}

// The controls that place a server: its height (`name=place-height`), the row, the rack of that
// row and the U of the rack at which that height is free (`place-row`, `place-rack`, `place-u`),
// each a choice from the lists the layout holds, never typed. `state` keeps the choices
// (`height`, `row`, `rack`, `u`, strings), `redraw` shows the page again after one changes, and
// `except` is the key of the server being moved (its own U count as free). With `optional`, the
// row's first choice is "Unplaced": the server is placed nowhere, and the rack and the U have no
// choice. The choices of U are only those that fit. Returns the elements, `request()`: `{rack,
// heightU, u}` for the choices made, or null for none (a rack or a free U is missing, or
// "Unplaced"), and `note`, a sentence when a choice cannot be made.
export function placementChoices(layout, state, except, redraw, optional) {
  const height = Number(state.height || 1);
  const rows = layout.rows;
  const none = optional === true && state.row === "";
  if (!none && !rows.some((r) => r.id === state.row)) {
    state.row = rows.length > 0 ? rows[0].id : "";
  }
  const racks = none ? [] : racksOf(layout, state.row);
  if (!racks.some((r) => r.id === state.rack)) {
    state.rack = racks.length > 0 ? racks[0].id : "";
  }
  const starts = none ? [] : freeStarts(layout, state.rack, height, except);
  if (!starts.includes(Number(state.u))) {
    state.u = starts.length > 0 ? String(starts[0]) : "";
  }
  const rowChoices = rows.map((r) => [r.id, `Row ${r.number}`]);
  const elements = [
    h("label", {}, "Height", chooser("place-height", state, "height",
      SERVER_HEIGHTS.map((n) => [n, `${n}U`]), redraw)),
    h("label", {}, "Row", chooser("place-row", state, "row",
      optional === true ? [["", "Unplaced"], ...rowChoices] : rowChoices, redraw)),
    h("label", {}, "Rack", chooser("place-rack", state, "rack",
      racks.map((r) => [r.id, `Rack ${r.number}`]), redraw)),
    h("label", {}, "U position", chooser("place-u", state, "u",
      starts.map((u) => [u, `U${u}`]), null)),
  ];
  let note = "";
  if (!none && rows.length === 0) {
    note = "There is no rack yet: add rows and racks on Home, in Edit layout.";
  } else if (!none && racks.length === 0) {
    note = "That row has no rack yet.";
  } else if (!none && starts.length === 0) {
    note = "No free slot of that height in this rack.";
  }
  return {
    elements,
    note,
    request: () => (state.rack === "" || state.u === "" || none
      ? null
      : { rack: state.rack, heightU: Number(state.height || 1), u: Number(state.u) }),
  };
}

// A panel of the placement controls that keeps itself current: when a choice changes, the choices
// that depend on it are made again in place and `show()` draws the page's tree again (the same
// tree, so nothing typed elsewhere on the page is lost). `panel.choices.request()` is the placement
// the choices name, or null.
export function placementPanel(layout, state, except, optional, show) {
  const panel = h("div", { class: "buttons" });
  const fill = () => {
    const choices = placementChoices(layout, state, except, () => {
      fill();
      show();
    }, optional);
    panel.choices = choices;
    panel.children = choices.note === ""
      ? choices.elements
      : [...choices.elements, h("p", { class: "note" }, choices.note)];
  };
  fill();
  return panel;
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
    trail.push([`Rack ${rack.number}`, "rack", "home", { rack: rack.id }]);
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
