// Inventory: the servers the operator built, one table of four fields each: Server name, BMC MAC,
// Data MAC and Notes. Nothing else of a record is shown or sent (its role, host group, serial,
// platform, window and the rest stay as the coordinator keeps them). The table has a row per
// record, with Edit and Remove, and a last row to add a server. Calls: Enrollment.ListRecords;
// on Add, Enrollment.AddRecord; on Save, Enrollment.UpdateRecord, which changes the record's
// MACs and notes and nothing else of it; on Remove, Enrollment.RemoveRecord, after a confirm.
// The page's `edit` parameter names the record whose row is open for editing, `pageToken` the
// page, and `record` (the Host page's link) marks that record's row. A row whose server has an
// enrolled host says so beside its name (`enrolled`, from ListRecords).

import { h, action, errorView, reasonProblem, table, trimmed } from "../view.js";

const PAGE_SIZE = 50;
// A server name is a host id of operator.proto: 1 to 63 of `[a-z0-9-]`, not starting or ending with `-`.
const SERVER_NAME = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const MAX_DATA_MACS = 16;
const MAC_PROBLEM = "is not a MAC: twelve hex digits, with or without : - . between them.";
const NOTES_PROBLEM = "Notes are at most 256 bytes, with no control character but the line break.";

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
function notesOf(text) {
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
      const common = { name: `${prefix}-${key}`, "aria-label": label, spellcheck: "false", value: form[key] || "" };
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
    h("td", {},
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
      })));
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
    h("td", {},
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
      }),
      action(ctx, "Cancel", "cancel-edit", async () => {
        leave();
        return undefined;
      })));
}

function addRow(ctx) {
  const form = { server: "", bmcMac: "", dataMac: "", notes: "" };
  const fields = inputs("add", form);
  return h("tr", {},
    h("td", {}, fields.cell("server", "Server name", "input")),
    h("td", {}, fields.cell("bmcMac", "BMC MAC", "input")),
    h("td", {}, fields.cell("dataMac", "Data MAC", "input")),
    h("td", {}, fields.cell("notes", "Notes", "textarea")),
    h("td", {}, action(ctx, "Add", "add-record", async () => {
      const { errors, body } = checkForm(form, true, false);
      fields.show(errors);
      if (Object.keys(errors).length > 0) {
        return "Correct the marked fields.";
      }
      await ctx.api.enrollment.addRecord(body);
      ctx.refresh();
      return `Added ${body.hostId}.`;
    })));
}

export async function render(ctx) {
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
  return h("section", { class: "page" },
    h("h2", {}, "Inventory"),
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
