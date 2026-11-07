// Inventory: the servers the operator recorded when building them (host id, role, data MACs, BMC
// MAC, serial, platform, notes), so that a pending enrollment request can be matched to its
// record, and whether a host is enrolled against each. Calls: Enrollment.ListRecords, all records
// or, with the filter, only the enrolled or only the not enrolled ones; on a button,
// Enrollment.AddRecord or Enrollment.RemoveRecord. A record approves nothing: Enrollment still approves each request
// by its key hash. The page's `record` parameter (the Host page's link) marks that record's row.

import {
  h, action, errorView, field, fmtUnix, reasonProblem, select, table, trimmed, words,
} from "../view.js";

const PAGE_SIZE = 50;
// The grammars of operator.proto: a host id is a node id (1 to 63 of `[a-z0-9-]`, not starting
// or ending with `-`), a role 1 to 32 of `[a-z0-9-]` with a letter first, not ending with `-` and
// with no `--`, a MAC six pairs of hex
// digits separated all by `:` or all by `-`, or twelve hex digits, and a serial 1 to 64 of
// `A-Za-z0-9._-`.
const HOST_ID = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const ROLE = /^(?!.*--)[a-z]([a-z0-9-]{0,30}[a-z0-9])?$/;
const MAC = /^([0-9A-Fa-f]{2}(:[0-9A-Fa-f]{2}){5}|[0-9A-Fa-f]{2}(-[0-9A-Fa-f]{2}){5}|[0-9A-Fa-f]{12})$/;
const SERIAL = /^[A-Za-z0-9._-]{1,64}$/;
const MAX_DATA_MACS = 16;
// The platforms a record may name for a network install, as the gateway's JSON spells the enum
// (the form's choice, then the name sent), and the short names the records list shows.
const PLATFORMS = [["", "none"], ["linux-x86_64", "linux-x86_64"], ["linux-arm64", "linux-arm64"]];
const PLATFORM_NAMES = { PLATFORM_LINUX_X86_64: "linux-x86_64", PLATFORM_LINUX_ARM64: "linux-arm64" };
const PLATFORM_ENUMS = { "linux-x86_64": "PLATFORM_LINUX_X86_64", "linux-arm64": "PLATFORM_LINUX_ARM64" };

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
    return "A role is 1 to 32 of a-z, 0-9 and -, a letter first, not ending with - and with no --.";
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
  const body = { hostId, role, dataMacs, bmcMac, serial, notes };
  if (form.platform) {
    body.platform = PLATFORM_ENUMS[form.platform];
  }
  return body;
}

// The `enrolled` of a ListRecordsRequest for the page's `enrolled` parameter: true for "true",
// false for "false", and absent (every record) for anything else.
export function enrolledFilter(param) {
  if (param === "true") {
    return { enrolled: true };
  }
  if (param === "false") {
    return { enrolled: false };
  }
  return {};
}

function recordRow(ctx, record, removal, chosen) {
  return h("tr", chosen ? { "aria-current": "true" } : {},
    h("td", {}, record.hostId),
    h("td", {}, record.role || "-"),
    h("td", {}, (record.dataMacs || []).join(", ") || "-"),
    h("td", {}, record.bmcMac || "-"),
    h("td", {}, record.serial || "-"),
    h("td", {}, PLATFORM_NAMES[record.platform] || "-"),
    h("td", {}, record.notes || "-"),
    h("td", {}, record.addedBy || "-"),
    h("td", {}, fmtUnix(record.addedUnix)),
    h("td", {}, record.enrolled === true ? "yes" : "no"),
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
  const param = (name) => (ctx.params && typeof ctx.params[name] === "string" ? ctx.params[name] : "");
  const pageToken = param("pageToken");
  const filter = enrolledFilter(param("enrolled"));
  const shown = filter.enrolled === undefined ? "" : String(filter.enrolled);
  // The parameters that keep the filter across pages.
  const kept = shown === "" ? {} : { enrolled: shown };
  let listed;
  try {
    listed = await ctx.api.call("hostkeeper.operator.v1.Enrollment/ListRecords",
      Object.assign({ limit: PAGE_SIZE, pageToken }, filter));
  } catch (error) {
    return errorView("Inventory", error);
  }
  const records = listed.records || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const form = { hostId: "", role: "", dataMacs: "", bmcMac: "", serial: "", platform: "", notes: "" };
  const removal = { reason: "" };
  const filterForm = { enrolled: shown };
  const nothing = shown === "" ? "No server is recorded." : `No ${shown === "true" ? "" : "not "}enrolled server is recorded.`;
  return h("section", { class: "page" },
    h("h2", {}, "Inventory"),
    h("p", { class: "note" },
      "Record each server's data MAC and BMC MAC when you build it. A pending request that matches "
      + "one record shows its host id and role on the Enrollment page; it is still approved by its key hash. "
      + "A record is enrolled when an enrolled host's claims match it and no other record."),
    h("fieldset", {},
      h("legend", {}, "Filter"),
      select("Enrolled", "enrolled-filter", filterForm, "enrolled",
        [["", "every record"], ["true", "enrolled"], ["false", "not enrolled"]]),
      action(ctx, "Show", "show", async () => {
        ctx.navigate("inventory", filterForm.enrolled === "" ? {} : { enrolled: filterForm.enrolled });
        return undefined;
      })),
    records.length === 0
      ? h("p", {}, nothing)
      : table(["Host id", "Role", "Data MACs", "BMC MAC", "Serial", "Platform", "Notes", "Added by", "Added at", "Enrolled", ""],
        records.map((record) => recordRow(ctx, record, removal, record.hostId === param("record")))),
    records.length === 0 ? null : field("Reason for a removal", "remove-reason", removal, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" },
      pageToken === "" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("inventory", kept) },
      }, "First page"),
      next === "" ? null : h("button", {
        type: "button", name: "next-page",
        on: { click: () => ctx.navigate("inventory", Object.assign({ pageToken: next }, kept)) },
      }, "Next page")),
    h("fieldset", {},
      h("legend", {}, "Record a server"),
      field("Host id", "record-host-id", form, "hostId"),
      field("Role (may be empty)", "record-role", form, "role"),
      field("Data MACs (separated by commas)", "record-data-macs", form, "dataMacs"),
      field("BMC MAC (may be empty)", "record-bmc-mac", form, "bmcMac"),
      field("Serial (a Mac's is what it is matched by)", "record-serial", form, "serial"),
      select("Platform (for a network install)", "record-platform", form, "platform", PLATFORMS),
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
