// Enrollment: the hosts waiting for an operator, with what they claim, the inventory record each
// matches, and Approve; or, with the parameter `unenrolled`, the requests that expired, whose hosts
// are unenrolled, each with why, and no Approve. Calls: Enrollment.List (with `unenrolled: true`
// for the second view); on a button, Enrollment.Approve, which sends back the key hash the list
// gave for the request. Claims authorise nothing: they help an operator
// recognise a machine. A match against one record (the Inventory page) fills in the host id and
// role; it approves nothing, and several matching records fill in nothing. A request (not a
// renewal) that matches one record is approved as that record's host id: an empty host id is sent
// empty for the coordinator to fill in, and another host id needs Replace. The answer names the
// host id approved.

import {
  h, action, checkbox, errorView, field, fmtUnix, reasonProblem, table, trimmed, words,
} from "../view.js";

const PAGE_SIZE = 50;
const UNENROLLED = "ENROLLMENT_REQUEST_STATE_UNENROLLED";
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
    ["BMC MAC", c.bmcMac],
  ].filter(([, v]) => typeof v === "string" && v !== "");
  return facts.length === 0
    ? h("p", {}, "No claims.")
    : table(["Claim", ""], facts.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v))));
}

// The host id of the one record `request` (a PendingEnrollment) matches, when it is not a
// renewal: the host id its approval is made as; "" otherwise.
function recordHostId(request) {
  const matched = request.inventoryMatch === "INVENTORY_MATCH_MATCHED" && !request.renewalOf;
  return matched ? request.recordHostId || "" : "";
}

// The ApproveRequest for `request` (a PendingEnrollment) from what the operator typed, or a
// sentence saying what is missing or wrong, each input checked on its own against its grammar.
// The key hash is the one the list gave, sent back unchanged; `replace` is true only when its box
// is ticked. For a request that matches one record, an empty host id is sent empty (the
// coordinator approves it as the record's), and another host id than the record's needs Replace.
export function approval(request, form) {
  const hostId = trimmed(form.hostId);
  const role = trimmed(form.role);
  const services = words(form.serviceList);
  const reason = trimmed(form.reason);
  const record = recordHostId(request);
  const replace = form.replace === true;
  if (hostId === "" && record === "") {
    return "Name the host id.";
  }
  if (hostId !== "" && !HOST_ID.test(hostId)) {
    return "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
  }
  if (record !== "" && hostId !== "" && hostId !== record && !replace) {
    return `This request matches the record of ${record}: approve it as ${record}, or tick Replace to approve it as ${hostId}.`;
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
    replace,
    reason,
  };
}

// What the request's match against the inventory says, as one line.
export function matchLine(request) {
  switch (request.inventoryMatch) {
    case "INVENTORY_MATCH_MATCHED":
      return `Matches the record of ${request.recordHostId}`
        + (request.recordRole ? ` (role ${request.recordRole}).` : " (no role).");
    case "INVENTORY_MATCH_AMBIGUOUS":
      return `Matches several records: ${(request.candidateHostIds || []).join(", ")}. None is chosen.`;
    case "INVENTORY_MATCH_UNMATCHED":
      return "Matches no inventory record.";
    default:
      return "Not matched against the inventory.";
  }
}

function pending(ctx, request) {
  const matched = request.inventoryMatch === "INVENTORY_MATCH_MATCHED";
  const form = {
    hostId: request.renewalOf || recordHostId(request),
    role: matched ? request.recordRole || "" : "",
    serviceList: "",
    reason: "",
    replace: false,
  };
  return h("section", { class: "service" },
    h("h3", {}, `Request ${request.requestId}`),
    request.duplicateClaim === true
      ? h("p", { class: "error", role: "alert" },
        "Duplicate claim: another host presents the same machine id or key, or a MAC of an enrolled host. "
        + "Approving it needs Replace.")
      : null,
    h("p", { class: request.inventoryMatch === "INVENTORY_MATCH_AMBIGUOUS" ? "warning" : "note" },
      matchLine(request)),
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
        const named = body.hostId || recordHostId(request);
        if (body.replace && !ctx.confirm(
          `Approve request ${body.requestId} as ${named} and revoke the certificate of any host that holds ${named}?`)) {
          return "Not approved.";
        }
        const reply = await ctx.api.call("hostkeeper.operator.v1.Enrollment/Approve", body);
        ctx.refresh();
        return `Approved ${reply.hostId || named}: the host gets its certificate at its next poll.`;
      })));
}

// A request TTL of `secs` seconds, in the largest whole unit.
export function ttlWords(secs) {
  const n = Number(secs);
  if (!Number.isSafeInteger(n) || n <= 0) {
    return "its time";
  }
  const [count, unit] = n % 3600 === 0 ? [n / 3600, "hour"]
    : n % 60 === 0 ? [n / 60, "minute"]
      : [n, "second"];
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

// Why the host of `request` (an unenrolled PendingEnrollment) is unenrolled, in a sentence.
export function unenrolledLine(request) {
  return request.unenrolledReason === "request_expired"
    ? `Unenrolled: the request expired. No operator approved it within ${ttlWords(request.requestTtlSecs)}. `
      + "It cannot be approved; the host is listed pending again when it posts its key again."
    : `Unenrolled: ${request.unenrolledReason || "no reason given"}.`;
}

function unenrolled(request) {
  return h("section", { class: "service" },
    h("h3", {}, `Request ${request.requestId}`),
    h("p", { class: "warning" }, unenrolledLine(request)),
    table(["", ""], [
      ["Key SHA-256", request.keySha256 || "-"],
      ["Image", request.image || "-"],
      ["First seen", fmtUnix(request.firstSeenUnix)],
      ["Expired", fmtUnix(request.unenrolledUnix)],
      ["Renewal of", request.renewalOf || "-"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    claimTable(request["claims"] || {}));
}

export async function render(ctx) {
  const params = ctx.params || {};
  const pageToken = typeof params.pageToken === "string" ? params.pageToken : "";
  const expired = params.unenrolled === "yes";
  const body = expired ? { limit: PAGE_SIZE, pageToken, unenrolled: true } : { limit: PAGE_SIZE, pageToken };
  let listed;
  try {
    listed = await ctx.api.call("hostkeeper.operator.v1.Enrollment/List", body);
  } catch (error) {
    return errorView("Enrollment", error);
  }
  const requests = listed.requests || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const here = expired ? { unenrolled: "yes" } : {};
  // A coordinator that does not expire requests ignores `unenrolled` and lists its pending ones:
  // only a request it says is unenrolled is shown as one.
  const shown = expired ? requests.filter((request) => request.state === UNENROLLED) : requests;
  const empty = expired
    ? h("p", {}, requests.length === 0 ? "No request expired." : "This coordinator does not expire requests.")
    : h("p", {}, "No host is waiting.");
  return h("section", { class: "page" },
    h("h2", {}, expired ? "Enrollment: expired requests" : "Enrollment"),
    h("p", { class: "note" }, expired
      ? "A request no operator approves within the coordinator's request TTL expires, and its host is unenrolled."
      : "Claims are what a host says it is; they prove nothing. Approve names the key hash shown."),
    shown.length === 0 ? empty : shown.map((request) => (expired ? unenrolled(request) : pending(ctx, request))),
    h("div", { class: "buttons" },
      pageToken === "" ? null : h("button", {
        type: "button", name: "first-page", on: { click: () => ctx.navigate("enrollment", here) },
      }, "First page"),
      next === "" ? null : h("button", {
        type: "button", name: "next-page", on: { click: () => ctx.navigate("enrollment", { ...here, pageToken: next }) },
      }, "Next page"),
      h("button", {
        type: "button",
        name: expired ? "show-pending" : "show-unenrolled",
        on: { click: () => ctx.navigate("enrollment", expired ? {} : { unenrolled: "yes" }) },
      }, expired ? "Pending requests" : "Expired requests")));
}
