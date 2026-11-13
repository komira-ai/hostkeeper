// Inventory, kind Switch: the lab's network devices, the switches in the racks (docs/design/network-gear.md).
// A switch has a Name, a Role (BMC network or Data network), a Console (what its console port is
// reachable as: the host its cable ends at, picked from the hosts, and the device on it, picked from a
// short list, or none), an optional Management MAC and Notes. It is placed like a server: a Height
// (1U by default), a Row, a Rack and a U position, each a choice from the lab's layout, and
// "Unplaced" first. Nothing is typed where a list fits.
//
// One table of the switches (Open, Edit and Remove on each) and, under it, one form that adds a
// switch, or edits the one the page's `edit` parameter names. Calls: Layout.Get and Hosts.List to
// read; Layout.PutDevice to add or save, then Layout.Place or Layout.Unplace for its place, and
// Layout.RemoveDevice on Remove, after a confirm. A refusal is told in words. The kind switch
// (Server | Switch) is drawn by `inventory_kinds.js`.

import {
  h, action, allPages, area, errorView, field, reasonProblem, select, table, trimmed,
} from "../view.js";
import {
  CONSOLE_DEVICES, NETWORK_ROLES, consoleWords, gearOf, layoutOf, layoutProblem, placementPanel, rackName,
  roleWords, subjectOf,
} from "../lab.js";
import { NOTES_PROBLEM, normalizeMac, notesOf } from "./inventory.js";

// A switch's name: 1 to 32 of letters, digits, space, `.`, `_` and `-`, not edged by a space.
const NAME = /^[A-Za-z0-9 ._-]{1,32}$/;
const MAC_PROBLEM = "is not a MAC: twelve hex digits, with or without : - . between them.";

// What the fields of `form` hold, each checked on its own: `{ errors, body }`. `errors` has a
// sentence for each field that is wrong (keys name, role, console, mac, notes); `body` has the
// fields of the request (`name`, `role`, `consoleHost`, `consoleDevice`, `managementMac`, `notes`).
export function checkSwitch(form) {
  const errors = {};
  const name = trimmed(form.name);
  if (name === "") {
    errors.name = "Name the switch.";
  } else if (!NAME.test(name)) {
    errors.name = "A name is 1 to 32 of letters, digits, space, . _ and -.";
  }
  if (!NETWORK_ROLES.some(([value]) => value === form.role)) {
    errors.role = "Choose the network it carries.";
  }
  const host = trimmed(form.consoleHost);
  const device = trimmed(form.consoleDevice);
  const known = (path) => CONSOLE_DEVICES.includes(path) || path === form.keepDevice;
  if (host !== "" && !known(device)) {
    errors.console = "Choose the device the console cable is plugged into.";
  }
  const mac = trimmed(form.mac);
  let managementMac = "";
  if (mac !== "") {
    const normal = normalizeMac(mac);
    if (normal === null) {
      errors.mac = `${mac} ${MAC_PROBLEM}`;
    } else {
      managementMac = normal;
    }
  }
  const notes = notesOf(form.notes);
  if (notes !== "" && reasonProblem(notes.replace(/\n/g, " ")) !== null) {
    errors.notes = NOTES_PROBLEM;
  }
  return {
    errors,
    body: {
      name, role: form.role, consoleHost: host, consoleDevice: host === "" ? "" : device, managementMac, notes,
    },
  };
}

// The placement state a form starts from: the device's own place when it has one, else "Unplaced".
function placementState(layout, held) {
  if (held === undefined) {
    return { height: "1", row: "", rack: "", u: "" };
  }
  const rack = layout.racks.find((r) => r.id === held.rack);
  return { height: String(held.height), row: rack === undefined ? "" : rack.row, rack: held.rack, u: String(held.u) };
}

// Where a device stands, in words.
export function whereWords(layout, held) {
  return held === undefined ? "Unplaced" : `${rackName(layout, held.rack)}, U${held.u}${held.height > 1 ? `-${held.u + held.height - 1}` : ""}`;
}

export async function render(ctx) {
  const param = (name) => (ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "");
  let layout;
  try {
    layout = layoutOf(await ctx.api.layout.get({}));
  } catch (error) {
    return errorView("Inventory", error);
  }
  let nodes = [];
  let nodesFailure = null;
  try {
    nodes = (await allPages((token) => ctx.api.hosts.list(token === "" ? {} : { pageToken: token }), "hosts"))
      .map((host) => host.nodeId);
  } catch (error) {
    nodesFailure = error;
  }
  const gear = gearOf(layout);
  const editing = gear.find((item) => item.id === param("edit"));
  const device = editing === undefined ? undefined : editing.device;
  const form = device === undefined
    ? { name: "", role: "NETWORK_ROLE_DATA", consoleHost: "", consoleDevice: CONSOLE_DEVICES[0], mac: "", notes: "", keepDevice: "" }
    : {
      name: device.name, role: device.role === "bmc" ? "NETWORK_ROLE_BMC" : "NETWORK_ROLE_DATA",
      consoleHost: device.consoleHost, consoleDevice: device.consoleDevice || CONSOLE_DEVICES[0],
      mac: device.managementMac, notes: device.notes, keepDevice: device.consoleDevice,
    };
  const held = editing === undefined ? undefined : layout.placements.find((p) => p.key === editing.key);
  const where = placementState(layout, held);
  const errorMarks = {};
  // A control with the place for its error under it, which `save` fills.
  const marked = (control, key) => {
    errorMarks[key] = h("span", { class: "error", role: "alert", name: `switch-${key}-error` });
    control.children.push(errorMarks[key]);
    return control;
  };

  const repaint = () => ctx.show(assemble());
  const leave = () => {
    const params = Object.assign({}, ctx.params);
    delete params.edit;
    ctx.navigate("inventory", params);
  };

  const panel = placementPanel(layout, where, editing === undefined ? undefined : editing.key, true, () => repaint());

  const save = async () => {
    const { errors, body } = checkSwitch(form);
    for (const [key, node] of Object.entries(errorMarks)) {
      node.children = errors[key] === undefined ? [] : [{ text: errors[key] }];
    }
    if (Object.keys(errors).length > 0) {
      return "Correct the marked fields.";
    }
    const request = panel.choices.request();
    let reply;
    try {
      reply = await ctx.api.layout.putDevice(Object.assign({ id: editing === undefined ? "" : editing.id, reason: "saved on the Inventory page" }, body));
    } catch (error) {
      return layoutProblem(error);
    }
    const id = reply.device.id;
    let said = editing === undefined ? `Added ${body.name}.` : `Saved ${body.name}.`;
    try {
      if (request !== null) {
        await ctx.api.layout.place(Object.assign({ subject: subjectOf({ kind: "inventory", id }), reason: `placed ${body.name}` }, request));
      } else if (where.row === "" && held !== undefined) {
        await ctx.api.layout.unplace({ subject: subjectOf({ kind: "inventory", id }), reason: `unplaced ${body.name}` });
      }
    } catch (error) {
      said = `${said} It could not be placed: ${layoutProblem(error)}`;
    }
    if (editing !== undefined) {
      leave();
    } else {
      ctx.refresh();
    }
    return said;
  };

  const consoleHosts = Array.from(new Set(nodes.concat(form.consoleHost === "" ? [] : [form.consoleHost])))
    .map((node) => [node, node]);
  const consoleDevices = Array.from(new Set(CONSOLE_DEVICES.concat(form.keepDevice === "" ? [] : [form.keepDevice])))
    .map((path) => [path, path]);

  const rows = gear.map((item) => {
    const open = ctx.params && item.id === param("edit");
    return h("tr", open ? { "aria-current": "true" } : {},
      h("td", {}, item.name),
      h("td", {}, roleWords(item.device.role)),
      h("td", {}, consoleWords(item.device)),
      h("td", {}, item.device.managementMac || "-"),
      h("td", {}, item.device.notes ? h("pre", {}, item.device.notes) : "-"),
      h("td", {}, whereWords(layout, layout.placements.find((p) => p.key === item.key))),
      h("td", { class: "row-actions" },
        action(ctx, "Open", "open-switch", async () => {
          ctx.navigate("switch", { id: item.id });
          return undefined;
        }),
        action(ctx, "Edit", "edit-switch", async () => {
          ctx.navigate("inventory", Object.assign({}, ctx.params, { kind: "switch", edit: item.id }));
          return undefined;
        }),
        action(ctx, "Remove", "remove-switch", async () => {
          if (!ctx.confirm(`Remove ${item.name} from the inventory? It leaves its rack too.`)) {
            return undefined;
          }
          try {
            await ctx.api.layout.removeDevice({ id: item.id, reason: "removed on the Inventory page" });
          } catch (error) {
            return layoutProblem(error);
          }
          ctx.refresh();
          return `Removed ${item.name}.`;
        }, { class: "danger" })));
  });

  const assemble = () => {
    return h("section", { class: "page inventory" },
      h("h2", {}, "Inventory"),
      h("p", { class: "note" }, "The switches in the racks: each with its network, console, management MAC and place."),
      gear.length === 0
        ? h("p", { class: "empty" }, "No switch yet. Add the BMC switch and the data switch below.")
        : table(["Name", "Network", "Console", "Management MAC", "Notes", "Where", ""], rows),
      h("div", { class: "switch-form" },
        h("h3", {}, device === undefined ? "Add a switch" : `Edit ${device.name}`),
        marked(field("Name", "switch-name", form, "name", { maxlength: 32, placeholder: "bmc-switch" }), "name"),
        marked(select("Network", "switch-role", form, "role", NETWORK_ROLES), "role"),
        select("Console host", "switch-console-host", form, "consoleHost", [["", "none"]].concat(consoleHosts)),
        marked(select("Console device", "switch-console-device", form, "consoleDevice", consoleDevices), "console"),
        marked(field("Management MAC (optional)", "switch-mac", form, "mac", { placeholder: "aa:bb:cc:dd:ee:ff" }), "mac"),
        marked(area("Notes", "switch-notes", form, "notes", { rows: 2, maxlength: 300, placeholder: "Notes" }), "notes"),
        h("div", { class: "buttons wide" }, panel),
        nodesFailure === null ? null : h("p", { class: "note wide" }, "The hosts could not be listed, so a console host cannot be chosen now."),
        h("div", { class: "buttons" },
          action(ctx, device === undefined ? "Add" : "Save", "save-switch", save, { class: "primary" }),
          device === undefined ? null : action(ctx, "Cancel", "cancel-switch", async () => {
            leave();
            return undefined;
          }))));
  };
  return assemble();
}
