// Audit: the coordinator's log of changes, newest last, filtered by actor, action, service and
// host, a page at a time from a sequence number. Calls: Audit.List, nothing else. The request has
// no filter by token name, so the page offers none.

import { h, action, enumLabel, errorView, field, select, table, trimmed } from "../view.js";

const PAGE_SIZE = 100;
const FILTERS = ["actor", "action", "service", "nodeId"];

// The ListAuditRequest for `params`: every filter that is set, and the page's start.
export function auditRequest(params) {
  const request = { sinceSeq: String(params.sinceSeq || "0"), limit: PAGE_SIZE };
  for (const key of FILTERS) {
    const value = trimmed(params[key]);
    if (value !== "") {
      request[key] = value;
    }
  }
  return request;
}

export async function render(ctx) {
  const params = ctx.params || {};
  const request = auditRequest(params);
  let listed;
  try {
    listed = await ctx.api.call("hostkeeper.operator.v1.Audit/List", request);
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
  const rows = entries.map(({ seq, by, action: did, service, nodeId, reason }) => h("tr", {},
    h("td", {}, String(seq || "-")),
    h("td", {}, enumLabel(by, "ACTOR_")),
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
      field("Action", "action", form, "action"),
      field("Service", "service", form, "service"),
      field("Host", "node-id", form, "nodeId"),
      action(ctx, "Show", "show", async () => {
        ctx.navigate("audit", filters());
        return undefined;
      })),
    entries.length === 0 ? h("p", {}, "No entry.") : table(["Seq", "By", "Action", "Service", "Host", "Reason"], rows),
    h("div", { class: "buttons" },
      request.sinceSeq === "0" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("audit", filters()) },
      }, "From the oldest"),
      entries.length === 0 ? null : h("button", {
        type: "button", name: "next-page",
        on: { click: () => ctx.navigate("audit", Object.assign(filters(), { sinceSeq: next })) },
      }, "Newer")));
}
