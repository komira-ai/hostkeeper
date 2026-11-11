// Audit: the coordinator's log of changes, newest last, each with its time and the name it was
// made under (the operator's token, or `coordinator`, `ci`, `mdm-nonce`), filtered by actor, by
// that name, by action, service and host, a page at a time from a sequence number or the newest
// page. Calls: Audit.List, nothing else.

import { h, action, enumLabel, errorView, field, fmtUnix, select, table, trimmed } from "../view.js";

const PAGE_SIZE = 100;
const FILTERS = ["actor", "byName", "action", "service", "nodeId"];

// The ListAuditRequest for `params`: every filter that is set, the page's start, and whether it
// is the newest page.
export function auditRequest(params) {
  const request = { sinceSeq: String(params.sinceSeq || "0"), limit: PAGE_SIZE };
  for (const key of FILTERS) {
    const value = trimmed(params[key]);
    if (value !== "") {
      request[key] = value;
    }
  }
  if (params.newest === "1") {
    request.newest = true;
  }
  return request;
}

export async function render(ctx) {
  const params = ctx.params || {};
  const request = auditRequest(params);
  let listed;
  try {
    listed = await ctx.api.audit.list(request);
  } catch (error) {
    return errorView("Audit", error);
  }
  const entries = listed.entries || [];
  const form = {};
  for (const key of FILTERS) {
    form[key] = request[key] || "";
  }
  const filters = () => {
    const out = {};
    for (const key of FILTERS) {
      if (trimmed(form[key]) !== "") {
        out[key] = trimmed(form[key]);
      }
    }
    return out;
  };
  const rows = entries.map(({ seq, atUnix, by, byName, action: did, service, nodeId, reason }) => h("tr", {},
    h("td", {}, String(seq || "-")),
    h("td", {}, fmtUnix(atUnix)),
    h("td", {}, enumLabel(by, "ACTOR_")),
    h("td", {}, byName || "-"),
    h("td", {}, did || "-"),
    h("td", {}, service || "-"),
    h("td", {}, nodeId || "-"),
    h("td", {}, reason || "-")));
  const next = String(listed.nextSeq || request.sinceSeq);
  return h("section", { class: "page" },
    h("h2", {}, "Audit"),
    h("fieldset", {},
      h("legend", {}, "Filter"),
      select("Actor", "actor", form, "actor",
        [["", "anyone"], ["operator", "an operator"], ["ci", "CI"], ["coordinator", "the coordinator"]]),
      field("Who", "by-name", form, "byName"),
      field("Action", "action", form, "action"),
      field("Service", "service", form, "service"),
      field("Host", "node-id", form, "nodeId"),
      action(ctx, "Show", "show", async () => {
        ctx.navigate("audit", filters());
        return undefined;
      })),
    entries.length === 0 ? h("p", {}, "No entry.") : table(["Seq", "Time", "By", "Who", "Action", "Service", "Host", "Reason"], rows),
    h("div", { class: "buttons" },
      request.sinceSeq === "0" && !request.newest ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("audit", filters()) },
      }, "From the oldest"),
      request.newest ? null : h("button", {
        type: "button", name: "newest-page",
        on: { click: () => ctx.navigate("audit", Object.assign(filters(), { newest: "1" })) },
      }, "Newest"),
      entries.length === 0 ? null : h("button", {
        type: "button", name: "next-page",
        on: { click: () => ctx.navigate("audit", Object.assign(filters(), { sinceSeq: next })) },
      }, "Newer")));
}
