// Inventory: the servers the operator recorded when building them (host id, role, data MACs, BMC
// MAC, serial, notes), so that a pending enrollment request can be matched to its record.
// Calls: Enrollment.ListRecords; on a button, Enrollment.AddRecord or Enrollment.RemoveRecord.
// A record approves nothing: Enrollment still approves each request by its key hash.

import {
  h, action, errorView, field, fmtUnix, reasonProblem, table, trimmed, words,
} from "../view.js";

const PAGE_SIZE = 50;
// The grammars of operator.proto: a host id is a node id (1 to 63 of `[a-z0-9-]`, not starting
// or ending with `-`), a role 1 to 32 of `[a-z0-9-]` with a letter first, a MAC six pairs of hex
// digits separated all by `:` or all by `-`, or twelve hex digits, and a serial 1 to 64 of
// `A-Za-z0-9._-`.
const HOST_ID = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const ROLE = /^[a-z][a-z0-9-]{0,31}$/;
const MAC = /^([0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}|[0-9A-Fa-f]{2}(-[0-9A-Fa-f]{2}){5}|[0-9A-Fa-f]{12})$/;
const SERIAL = /^[A-Za-z0-9._-]{1,64}$/;
const MAX_DATA_MACS = 16;

// The AddRecordRequest the form names, or a sentence saying what is missing or wrong, each input
// checked on its own against its grammar. The coordinator writes each MAC in one spelling.
export function recordRequest(form) {
  const hostId = trimmed(form.hostId);
  const role = trimmed(form.role);
  const dataMacs = words(form.dataMacs);
  const bmcMac = trimmed(form.bmcMac);
  const serial = trimmed(form.serial);
  const notes = trimmed(form.notes);
  if (hostId === "") {
    return "Name the host id.";
  }
  if (!HOST_ID.test(hostId)) {
    return "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
  }
  if (role !== "" && !ROLE.test(role)) {
    return "A role is 1 to 32 of a-z, 0-9 and -, a letter first.";
  }
  const badMac = dataMacs.concat(bmcMac === "" ? [] : [bmcMac]).find((m) => !MAC.test(m));
  if (badMac !== undefined) {
    return `${badMac} is not a MAC: six pairs of hex digits separated by : or by -.`;
  }
  if (dataMacs.length > MAX_DATA_MACS) {
    return `A record holds at most ${MAX_DATA_MACS} data MACs.`;
  }
  if (serial !== "" && !SERIAL.test(serial)) {
    return "A serial is 1 to 64 of A-Z, a-z, 0-9, ., _ and -.";
  }
  if (dataMacs.length === 0 && serial === "") {
    return "Give a data MAC, or a serial for a Mac.";
  }
  if (notes !== "" && reasonProblem(notes) !== null) {
    return reasonProblem(notes).replace("A reason", "A note");
  }
  return { hostId, role, dataMacs, bmcMac, serial, notes };
}

function recordRow(ctx, record, removal) {
  return h("tr", {},
    h("td", {}, record.hostId),
    h("td", {}, record.role || "-"),
    h("td", {}, (record.dataMacs || []).join(", ") || "-"),
    h("td", {}, record.bmcMac || "-"),
    h("td", {}, record.serial || "-"),
    h("td", {}, record.notes || "-"),
    h("td", {}, record.addedBy || "-"),
    h("td", {}, fmtUnix(record.addedUnix)),
    h("td", {}, action(ctx, "Remove", "remove-record", async () => {
      const reason = trimmed(removal.reason);
      if (reason === "") {
        return "Give a reason for the removal first.";
      }
      if (reasonProblem(reason) !== null) {
        return reasonProblem(reason);
      }
      await ctx.api.call("hostkeeper.operator.v1.Enrollment/RemoveRecord", { hostId: record.hostId, reason });
      ctx.refresh();
      return `Removed the record of ${record.hostId}.`;
    })));
}

export async function render(ctx) {
  const pageToken = ctx.params && typeof ctx.params.pageToken === "string" ? ctx.params.pageToken : "";
  let listed;
  try {
    listed = await ctx.api.call("hostkeeper.operator.v1.Enrollment/ListRecords", { limit: PAGE_SIZE, pageToken });
  } catch (error) {
    return errorView("Inventory", error);
  }
  const records = listed.records || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const form = { hostId: "", role: "", dataMacs: "", bmcMac: "", serial: "", notes: "" };
  const removal = { reason: "" };
  return h("section", { class: "page" },
    h("h2", {}, "Inventory"),
    h("p", { class: "note" },
      "Record each server's data MAC and BMC MAC when you build it. A pending request that matches "
      + "one record shows its host id and role on the Enrollment page; it is still approved by its key hash."),
    records.length === 0
      ? h("p", {}, "No server is recorded.")
      : table(["Host id", "Role", "Data MACs", "BMC MAC", "Serial", "Notes", "Added by", "Added at", ""],
        records.map((record) => recordRow(ctx, record, removal))),
    records.length === 0 ? null : field("Reason for a removal", "remove-reason", removal, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      pageToken === "" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("inventory", {}) },
      }, "First page"),
      next === "" ? null : h("button", {
        type: "button", name: "next-page", on: { click: () => ctx.navigate("inventory", { pageToken: next }) },
      }, "Next page")),
    h("fieldset", {},
      h("legend", {}, "Record a server"),
      field("Host id", "record-host-id", form, "hostId"),
      field("Role (may be empty)", "record-role", form, "role"),
      field("Data MACs (separated by commas)", "record-data-macs", form, "dataMacs"),
      field("BMC MAC (may be empty)", "record-bmc-mac", form, "bmcMac"),
      field("Serial (a Mac's is what it is matched by)", "record-serial", form, "serial"),
      field("Notes", "record-notes", form, "notes", { maxlength: 256 }),
      action(ctx, "Record", "add-record", async () => {
        const body = recordRequest(form);
        if (typeof body === "string") {
          return body;
        }
        const reply = await ctx.api.call("hostkeeper.operator.v1.Enrollment/AddRecord", body);
        ctx.refresh();
        return `Recorded ${reply.record ? reply.record.hostId : body.hostId}.`;
      })));
}
