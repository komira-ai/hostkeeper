// Inventory: the servers the operator recorded when building them (host id, role, data MACs, BMC
// MAC, serial, platform, notes), so that a pending enrollment request can be matched to its record, and
// whether a host is enrolled against each. Calls: Enrollment.ListRecords, all records or, with
// the filter, only the enrolled or only the not enrolled ones; on a button, Enrollment.AddRecord
// or Enrollment.RemoveRecord; on a closed record that names a role, Enrollment.OpenRecord. A
// record that names a role is the approval of its host: the coordinator approves a request that
// matches it exactly while its window is open, once (the State column: open, closed or used, and
// for a used record the request and the time). The page's `record` parameter (the Host page's link) marks that record's row.

import {
  h, action, enumLabel, errorView, field, fmtUnix, reasonProblem, select, table, trimmed, words,
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

const WINDOW_TIME = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2}))?(?:Z| ?UTC)?$/;
const WINDOW_PROBLEM = "A window is a number of hours (2 or 1.5), or an end time in UTC (2027-01-15 08:00 or 2027-01-15T08:00:00Z).";

// The window a form input names, as the fields of an AddRecordRequest or OpenRecordRequest: none
// for an empty input, `approveForSecs` for a number of hours, `approveUntilUnix` for an end time
// in UTC (both as the strings canonical JSON writes a uint64 in), or a sentence saying the input is neither.
export function windowFields(input) {
  const text = trimmed(input);
  if (text === "") {
    return {};
  }
  if (/^\d+(\.\d+)?$/.test(text)) {
    const secs = Math.round(Number(text) * 3600);
    return secs > 0 ? { approveForSecs: String(secs) } : WINDOW_PROBLEM;
  }
  const parts = WINDOW_TIME.exec(text);
  if (parts === null) {
    return WINDOW_PROBLEM;
  }
  const [year, month, day, hour, minute, second] = parts.slice(1).map((n) => Number(n || 0));
  const at = new Date(Date.UTC(year, month - 1, day, hour, minute, second));
  const same = at.getUTCMonth() === month - 1 && at.getUTCDate() === day && at.getUTCHours() === hour &&
    at.getUTCMinutes() === minute && at.getUTCSeconds() === second;
  return same ? { approveUntilUnix: String(Math.floor(at.getTime() / 1000)) } : WINDOW_PROBLEM;
}

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
  const window = windowFields(form.window);
  if (typeof window === "string") {
    return window;
  }
  if (role === "" && Object.keys(window).length > 0) {
    return "A window needs a role: a record with no role approves nothing.";
  }
  const body = Object.assign({ hostId, role, dataMacs, bmcMac, serial, notes }, window);
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

function recordRow(ctx, record, removal, opening, chosen) {
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
    h("td", {}, fmtUnix(record.approveUntilUnix)),
    h("td", {}, enumLabel(record.state, "RECORD_STATE_")),
    h("td", {}, record.usedByRequestId || "-"),
    h("td", {}, fmtUnix(record.usedUnix)),
    h("td", {}, record.state === "RECORD_STATE_CLOSED" && record.role
      ? action(ctx, "Open window", "open-record", async () => {
        const window = windowFields(opening.window);
        if (typeof window === "string") {
          return window;
        }
        const reason = trimmed(opening.reason);
        if (reason !== "" && reasonProblem(reason) !== null) {
          return reasonProblem(reason);
        }
        const reply = await ctx.api.enrollment.openRecord(
          Object.assign({ hostId: record.hostId, reason }, window));
        ctx.refresh();
        const until = reply.record ? reply.record.approveUntilUnix : undefined;
        return `Opened the record of ${record.hostId} until ${fmtUnix(until)}.`;
      })
      : null),
    h("td", {}, action(ctx, "Remove", "remove-record", async () => {
      const reason = trimmed(removal.reason);
      if (reason === "") {
        return "Give a reason for the removal first.";
      }
      if (reasonProblem(reason) !== null) {
        return reasonProblem(reason);
      }
      await ctx.api.enrollment.removeRecord({ hostId: record.hostId, reason });
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
    listed = await ctx.api.enrollment.listRecords(
      Object.assign({ limit: PAGE_SIZE, pageToken }, filter));
  } catch (error) {
    return errorView("Inventory", error);
  }
  const records = listed.records || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const form = { hostId: "", role: "", dataMacs: "", bmcMac: "", serial: "", platform: "", notes: "", window: "" };
  const opening = { window: "", reason: "" };
  const removal = { reason: "" };
  const filterForm = { enrolled: shown };
  const nothing = shown === "" ? "No server is recorded." : `No ${shown === "true" ? "" : "not "}enrolled server is recorded.`;
  return h("section", { class: "page" },
    h("h2", {}, "Inventory"),
    h("p", { class: "note" },
      "Record each server's data MAC and BMC MAC when you build it. A record that names a role approves "
      + "the one request that matches it exactly while its window is open, once; any other match shows its host id and "
      + "role on the Enrollment page and waits for an operator. "
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
      : table(["Host id", "Role", "Data MACs", "BMC MAC", "Serial", "Platform", "Notes", "Added by", "Added at", "Enrolled", "Window ends", "State", "Used by", "Used at", "", ""],
        records.map((record) => recordRow(ctx, record, removal, opening, record.hostId === param("record")))),
    records.length === 0 ? null : field("Reason for a removal", "remove-reason", removal, "reason", { maxlength: 256 }),
    records.length === 0 ? null : field("Window to open (hours or an end time; empty for the coordinator's default)",
      "open-window", opening, "window"),
    records.length === 0 ? null : field("Reason for opening (may be empty)", "open-reason", opening, "reason", { maxlength: 256 }),
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
      field("Window (hours or an end time in UTC; empty for the coordinator's default; needs a role)", "record-window", form, "window"),
      action(ctx, "Record", "add-record", async () => {
        const body = recordRequest(form);
        if (typeof body === "string") {
          return body;
        }
        const reply = await ctx.api.enrollment.addRecord(body);
        ctx.refresh();
        return `Recorded ${reply.record ? reply.record.hostId : body.hostId}.`;
      })));
}
