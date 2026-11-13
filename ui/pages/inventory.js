// Inventory: the servers the operator built, one table of four fields each: Server name, BMC MAC,
// Data MAC and Notes. Nothing else of a record is shown or sent (its role, host group, serial,
// platform, window and the rest stay as the coordinator keeps them). The table has a row per
// record, with Edit and Remove, and a last row to add a server. Calls: Enrollment.ListRecords;
// on Add, Enrollment.AddRecord; on Save, Enrollment.UpdateRecord, which changes the record's
// MACs and notes and nothing else of it; on Remove, Enrollment.RemoveRecord, after a confirm.
// The page's `edit` parameter names the record whose row is open for editing, `pageToken` the
// page, and `record` (the Host page's link) marks that record's row. A row whose server has an
// enrolled host says so beside its name (`enrolled`, from ListRecords).

import {
  h, action, allPages, area, errorView, field, reasonProblem, select, table, trimmed,
} from "../view.js";
import {
  CONSOLE_DEVICES, NETWORK_ROLES, consoleWords, gearOf, layoutOf, layoutProblem, placementPanel, rackName,
  roleWords, subjectOf,
} from "../lab.js";

const PAGE_SIZE = 50;
// A server name is a host id of operator.proto: 1 to 63 of `[a-z0-9-]`, not starting or ending with `-`.
const SERVER_NAME = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const MAX_DATA_MACS = 16;
const MAC_PROBLEM = "is not a MAC: twelve hex digits, with or without : - . between them.";
export const NOTES_PROBLEM = "Notes are at most 256 bytes, with no control character but the line break.";
// What an empty field of a row shows, by its key: an example of what goes there.
const PLACEHOLDERS = { server: "Server name", bmcMac: "aa:bb:cc:dd:ee:ff", dataMac: "aa:bb:cc:dd:ee:01", notes: "Notes" };

// A MAC in any common spelling (aa:bb:cc:dd:ee:ff, AA-BB-CC-DD-EE-FF, aabb.ccdd.eeff,
// aabbccddeeff, with spaces around) as the one the coordinator keeps: lower case, colon-separated;
// null when it is not twelve hex digits.
export function normalizeMac(text) {
  const digits = trimmed(text).replace(/[:.\-\s]/g, "");
  if (!/^[0-9A-Fa-f]{12}$/.test(digits)) {
    return null;
  }
  return digits.toLowerCase().replace(/(..)(?=.)/g, "$1:");
}

// The data MACs a field holds: MACs separated by commas, semicolons or line breaks, each
// normalised. A list of the normalised MACs, or `{ bad }` for the first entry that is none.
export function dataMacs(text) {
  const macs = [];
  for (const entry of trimmed(text).split(/[,;\n]+/)) {
    if (trimmed(entry) !== "") {
      const mac = normalizeMac(entry);
      if (mac === null) {
        return { bad: trimmed(entry) };
      }
      macs.push(mac);
    }
  }
  return macs;
}

// Notes with their line breaks written as `\n`, trimmed.
export function notesOf(text) {
  return trimmed(String(text || "").replace(/\r\n?/g, "\n"));
}

// What the inputs of `form` hold, checked each on its own: `{ errors, body }`. `errors` has a
// sentence per field that is wrong (keys server, bmcMac, dataMac, notes); `body` holds the fields
// of the request. A record that keeps a serial (a Mac) may have no data MAC. `named` is false
// when the server name is fixed (an edit).
export function checkForm(form, named, keepsSerial) {
  const errors = {};
  const body = {};
  if (named) {
    const name = trimmed(form.server);
    if (name === "") {
      errors.server = "Name the server.";
    } else if (!SERVER_NAME.test(name)) {
      errors.server = "A server name is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
    }
    body.hostId = name;
  }
  const bmc = trimmed(form.bmcMac);
  body.bmcMac = "";
  if (bmc !== "") {
    const mac = normalizeMac(bmc);
    if (mac === null) {
      errors.bmcMac = `${bmc} ${MAC_PROBLEM}`;
    } else {
      body.bmcMac = mac;
    }
  }
  const data = dataMacs(form.dataMac);
  body.dataMacs = [];
  if (!Array.isArray(data)) {
    errors.dataMac = `${data.bad} ${MAC_PROBLEM}`;
  } else if (data.length === 0 && !keepsSerial) {
    errors.dataMac = "Give the data MAC.";
  } else if (data.length > MAX_DATA_MACS) {
    errors.dataMac = `At most ${MAX_DATA_MACS} data MACs.`;
  } else {
    body.dataMacs = Array.from(new Set(data));
  }
  if (errors.bmcMac === undefined && bmc !== "" && body.dataMacs.includes(body.bmcMac)) {
    errors.bmcMac = "The BMC MAC is also a data MAC.";
  }
  const notes = notesOf(form.notes);
  body.notes = notes;
  if (notes !== "" && reasonProblem(notes.replace(/\n/g, " ")) !== null) {
    errors.notes = NOTES_PROBLEM;
  }
  return { errors, body };
}

// The inputs of a row bound to `form`: `cell(key, label, tag)` is a text input (or a text area)
// for `form[key]` with the place for its error under it, and `show(errors)` writes each field's
// sentence there (the page shows it when the status line next changes the view).
function inputs(prefix, form) {
  const marks = {};
  return {
    cell(key, label, tag) {
      const common = { name: `${prefix}-${key}`, "aria-label": label, spellcheck: "false", value: form[key] || "", placeholder: PLACEHOLDERS[key] };
      const input = h(tag, tag === "textarea"
        ? Object.assign({ rows: 2, maxlength: 300 }, common)
        : Object.assign({ type: "text", autocomplete: "off" }, common));
      input.on = {
        input: (e) => {
          form[key] = e.target.value;
          input.attrs.value = form[key];
        },
      };
      marks[key] = h("span", { class: "error", role: "alert", name: `${prefix}-${key}-error` });
      return [input, marks[key]];
    },
    show(errors) {
      for (const [key, mark] of Object.entries(marks)) {
        mark.children = errors[key] === undefined ? [] : [{ text: errors[key] }];
      }
    },
  };
}

// What a row says of the host enrolled against its record, beside the server name: nothing when
// none is, "enrolled" when the host has the server's name (or an older coordinator does not say which
// host), else "enrolled as <host id>".
function enrolledNote(record) {
  if (record.enrolled !== true) {
    return null;
  }
  const host = record.enrolledHostId || "";
  return h("small", { name: "enrolled-note" }, host === "" || host === record.hostId ? " enrolled" : ` enrolled as ${host}`);
}

function recordRow(ctx, record, chosen) {
  return h("tr", chosen ? { "aria-current": "true" } : {},
    h("td", {}, record.hostId, enrolledNote(record)),
    h("td", {}, record.bmcMac || "-"),
    h("td", {}, (record.dataMacs || []).join(", ") || "-"),
    h("td", {}, record.notes ? h("pre", {}, record.notes) : "-"),
    h("td", { class: "row-actions" },
      action(ctx, "Edit", "edit-record", async () => {
        ctx.navigate("inventory", Object.assign({}, ctx.params, { edit: record.hostId }));
        return undefined;
      }),
      action(ctx, "Remove", "remove-record", async () => {
        if (!ctx.confirm(`Remove ${record.hostId} from the inventory? A host enrolled as it keeps its certificate.`)) {
          return undefined;
        }
        await ctx.api.enrollment.removeRecord({ hostId: record.hostId, reason: "removed on the Inventory page" });
        ctx.refresh();
        return `Removed ${record.hostId}.`;
      }, { class: "danger" })));
}

function editRow(ctx, record) {
  const form = {
    bmcMac: record.bmcMac || "",
    dataMac: (record.dataMacs || []).join(", "),
    notes: record.notes || "",
  };
  const fields = inputs("edit", form);
  const leave = () => {
    const params = Object.assign({}, ctx.params);
    delete params.edit;
    ctx.navigate("inventory", params);
  };
  return h("tr", { "aria-current": "true" },
    h("td", {}, record.hostId),
    h("td", {}, fields.cell("bmcMac", "BMC MAC", "input")),
    h("td", {}, fields.cell("dataMac", "Data MAC", "input")),
    h("td", {}, fields.cell("notes", "Notes", "textarea")),
    h("td", { class: "row-actions" },
      action(ctx, "Save", "save-record", async () => {
        const { errors, body } = checkForm(form, false, Boolean(record.serial));
        fields.show(errors);
        if (Object.keys(errors).length > 0) {
          return "Correct the marked fields.";
        }
        await ctx.api.enrollment.updateRecord({
          hostId: record.hostId, dataMacs: body.dataMacs, bmcMac: body.bmcMac, notes: body.notes,
        });
        leave();
        return `Saved ${record.hostId}.`;
      }, { class: "primary" }),
      action(ctx, "Cancel", "cancel-edit", async () => {
        leave();
        return undefined;
      })));
}

function addRow(ctx) {
  const form = { server: "", bmcMac: "", dataMac: "", notes: "" };
  const fields = inputs("add", form);
  return h("tr", { class: "add-row" },
    h("td", {}, fields.cell("server", "Server name", "input")),
    h("td", {}, fields.cell("bmcMac", "BMC MAC", "input")),
    h("td", {}, fields.cell("dataMac", "Data MAC", "input")),
    h("td", {}, fields.cell("notes", "Notes", "textarea")),
    h("td", { class: "row-actions" }, action(ctx, "Add", "add-record", async () => {
      const { errors, body } = checkForm(form, true, false);
      fields.show(errors);
      if (Object.keys(errors).length > 0) {
        return "Correct the marked fields.";
      }
      await ctx.api.enrollment.addRecord(body);
      ctx.refresh();
      return `Added ${body.hostId}.`;
    }, { class: "primary" })));
}

async function renderServers(ctx) {
  const param = (name) => (ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "");
  const pageToken = param("pageToken");
  let listed;
  try {
    listed = await ctx.api.enrollment.listRecords({ limit: PAGE_SIZE, pageToken });
  } catch (error) {
    return errorView("Inventory", error);
  }
  const records = listed.records || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const rows = records.map((record) => (record.hostId === param("edit")
    ? editRow(ctx, record)
    : recordRow(ctx, record, record.hostId === param("record"))));
  return h("section", { class: "page inventory" },
    h("h2", {}, "Inventory"),
    h("p", { class: "note" }, "The servers you built: each with its name, BMC MAC, data MAC and notes."),
    table(["Server name", "BMC MAC", "Data MAC", "Notes", ""], rows.concat([addRow(ctx)])),
    h("div", { class: "buttons" },
      pageToken === "" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("inventory", {}) },
      }, "First page"),
      next === "" ? null : h("button", {
        type: "button", name: "next-page",
        on: { click: () => ctx.navigate("inventory", { pageToken: next }) },
      }, "Next page")));
}

// Inventory, kind Switch: the lab's network devices (docs/design/network-gear.md). A switch has a Name, a
// Network (BMC or Data), a Console (the host its cable ends at, picked from the hosts, and the device on
// it, picked from a short list, or none), an optional Management MAC and Notes, and a place like a
// server's (Height, 1U by default, Row, Rack, U; "Unplaced" first). One table of the switches and, under
// it, one form that adds a switch or edits the one the `edit` parameter names. Calls: Layout.Get and
// Hosts.List; Layout.PutDevice, then Layout.Place or Unplace; Layout.RemoveDevice after a confirm.
// A switch's name: 1 to 32 of letters, digits, space, `.`, `_` and `-`, not edged by a space.
const NAME = /^[A-Za-z0-9 ._-]{1,32}$/;

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

async function renderSwitches(ctx) {
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
    return h("section", { class: "page inventory switches" },
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

// The page: the choice of kind (Server | Switch, the `kind` parameter; Server when absent) under the
// title, then the page of the kind chosen.
export async function render(ctx) {
  const kind = ctx.params && ctx.params.kind === "switch" ? "switch" : "server";
  const page = kind === "switch" ? await renderSwitches(ctx) : await renderServers(ctx);
  const choose = (name, text, value) => h("button", {
    type: "button", name, class: kind === value ? "seg-on" : "", "aria-pressed": kind === value ? "true" : "false",
    on: { click: () => ctx.navigate("inventory", value === "switch" ? { kind: "switch" } : {}) },
  }, text);
  const control = h("div", { class: "kinds" },
    h("div", { class: "segmented", role: "group", "aria-label": "kind" },
      choose("kind-server", "Server", "server"), choose("kind-switch", "Switch", "switch")));
  if (Array.isArray(page.children)) {
    page.children.splice(1, 0, control);
  }
  return page;
}
