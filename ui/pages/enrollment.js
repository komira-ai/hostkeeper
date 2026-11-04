// Enrollment: the hosts waiting for an operator, with what they claim, and Approve.
// Calls: Enrollment.List; on a button, Enrollment.Approve, which sends back the key hash the
// list gave for the request. Claims authorise nothing: they help an operator recognise a machine.

import {
  h, action, checkbox, errorView, field, fmtUnix, reasonProblem, table, trimmed, words,
} from "../view.js";

const PAGE_SIZE = 50;
// The grammars of operator.proto: a host id is a node id (1 to 63 of `[a-z0-9-]`, not starting
// or ending with `-`), a role 1 to 32 of `[a-z0-9-]` with a letter first, a service name 1 to 64
// of `[a-z0-9-]`.
const HOST_ID = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const ROLE = /^[a-z][a-z0-9-]{0,31}$/;
const SERVICE = /^[a-z0-9-]{1,64}$/;

function claimTable(c) {
  const facts = [
    ["Machine id", c.machineId],
    ["DMI serial", c.dmiSerial],
    ["Product UUID", c.productUuid],
    ["Addresses", (c.addresses || []).join(", ")],
    ["Mac serial", c.macSerial],
    ["Platform UUID", c.platformUuid],
  ].filter(([, v]) => typeof v === "string" && v !== "");
  return facts.length === 0
    ? h("p", {}, "No claims.")
    : table(["Claim", ""], facts.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v))));
}

// The ApproveRequest for `request` (a PendingEnrollment) from what the operator typed, or a
// sentence saying what is missing or wrong, each input checked on its own against its grammar.
// The key hash is the one the list gave, sent back unchanged; `replace` is true only when its box
// is ticked.
export function approval(request, form) {
  const hostId = trimmed(form.hostId);
  const role = trimmed(form.role);
  const services = words(form.serviceList);
  const reason = trimmed(form.reason);
  if (hostId === "") {
    return "Name the host id.";
  }
  if (!HOST_ID.test(hostId)) {
    return "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
  }
  if (role === "") {
    return "Name the role.";
  }
  if (!ROLE.test(role)) {
    return "A role is 1 to 32 of a-z, 0-9 and -, a letter first.";
  }
  const badService = services.find((s) => !SERVICE.test(s));
  if (badService !== undefined) {
    return `${badService} is not a service name: 1 to 64 of a-z, 0-9 and -.`;
  }
  if (reason === "") {
    return "Give a reason for the approval.";
  }
  if (reasonProblem(reason) !== null) {
    return reasonProblem(reason);
  }
  return {
    requestId: request.requestId,
    keySha256: request.keySha256,
    hostId,
    role,
    services,
    replace: form.replace === true,
    reason,
  };
}

function pending(ctx, request) {
  const form = {
    hostId: request.renewalOf || "", role: "", serviceList: "", reason: "", replace: false,
  };
  return h("section", { class: "service" },
    h("h3", {}, `Request ${request.requestId}`),
    request.duplicateClaim === true
      ? h("p", { class: "error", role: "alert" },
        "Duplicate claim: another host presents the same machine id or key. Approving it needs Replace.")
      : null,
    table(["", ""], [
      ["Key SHA-256", request.keySha256 || "-"],
      ["Image", request.image || "-"],
      ["First seen", fmtUnix(request.firstSeenUnix)],
      ["Renewal of", request.renewalOf || "-"],
      ["Mac nonce presented", request.macNoncePresented === true ? "yes" : "no"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    claimTable(request["claims"] || {}),
    h("fieldset", {},
      h("legend", {}, "Approve"),
      field("Host id", "host-id", form, "hostId"),
      field("Role", "role", form, "role"),
      field("Services (separated by commas, may be empty)", "services", form, "serviceList"),
      field("Reason", "reason", form, "reason", { maxlength: 256 }),
      checkbox("Replace", "replace", form, "replace"),
      h("p", { class: "warning" },
        "Replace takes the host id from the host that holds it and revokes that host's certificate. "
        + "It is also needed to approve a duplicate claim or a denied host id. Leave it off unless you mean that."),
      action(ctx, "Approve", "approve", async () => {
        const body = approval(request, form);
        if (typeof body === "string") {
          return body;
        }
        if (body.replace && !ctx.confirm(
          `Approve request ${body.requestId} as ${body.hostId} and revoke the certificate of any host that holds ${body.hostId}?`)) {
          return "Not approved.";
        }
        const reply = await ctx.api.call("hostkeeper.operator.v1.Enrollment/Approve", body);
        ctx.refresh();
        return `Approved ${reply.hostId || body.hostId}: the host gets its certificate at its next poll.`;
      })));
}

export async function render(ctx) {
  const pageToken = ctx.params && typeof ctx.params.pageToken === "string" ? ctx.params.pageToken : "";
  let listed;
  try {
    listed = await ctx.api.call("hostkeeper.operator.v1.Enrollment/List", { limit: PAGE_SIZE, pageToken });
  } catch (error) {
    return errorView("Enrollment", error);
  }
  const requests = listed.requests || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  return h("section", { class: "page" },
    h("h2", {}, "Enrollment"),
    h("p", { class: "note" }, "Claims are what a host says it is; they prove nothing. Approve names the key hash shown."),
    requests.length === 0 ? h("p", {}, "No host is waiting.") : requests.map((request) => pending(ctx, request)),
    h("div", { class: "buttons" },
      pageToken === "" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("enrollment", {}) },
      }, "First page"),
      next === "" ? null : h("button", {
        type: "button", name: "next-page", on: { click: () => ctx.navigate("enrollment", { pageToken: next }) },
      }, "Next page")));
}
