// Enrollment: the hosts waiting for an operator, with what they claim, the inventory record each
// matches, Approve and Reject (no reason is typed for either); or, with the parameter `unenrolled`, the requests that expired, whose
// hosts are unenrolled, each with why, and no Approve. Calls: Enrollment.List (with
// `unenrolled: true` for the second view); HostGroups.List (every page, for the pending view
// only, when a request is listed) for the Host group choice; on a button, Enrollment.Approve,
// which sends back the key hash the list gave for the request, or Enrollment.Reject, after a confirm, which takes the
// request out of the queue and refuses its key from then on. Claims authorise nothing: they help an operator
// recognise a machine. An approval asks for one choice besides the host id: the host group, a
// dropdown of the coordinator's host groups but the built-in `all` (which every host is in) and a
// blank (the host is enrolled in no specific group, and runs the packages of all alone until it is
// put in one on the Host groups page). The record's group is chosen when
// the coordinator holds it. There is no role, no services and no reason to type. A match against
// one record (the Inventory page) fills in the host id and shows the record's state and window (a request that matches one record exactly while
// its window is open is approved by the coordinator itself and is not listed); several matching
// records fill in nothing. A request (not a
// renewal) that matches one record is approved as that record's host id: an empty host id is sent
// empty for the coordinator to fill in, and another host id needs Replace. The answer names the
// host id approved.
//
// A matched Linux request is shown with the claims that are hard to copy beside the record's
// values: its DMI serial, its BMC MAC and its product UUID (the record holds no UUID yet, so it is
// "not recorded"). A value the record does not hold is "not recorded" and a claim left out "not
// claimed", neither shown as agreement; a BMC MAC the record holds and the request does not claim
// is a warning. Beside the match line, the page says that "matched" is not proof. A request that
// another pending request contests (`contestedBy`: they match one record) says so, naming the
// others, and its approval is refused in the page, as the coordinator refuses it
// (`record_contested`), until the operator rejects all but one.

import {
  h, action, allPages, checkbox, describeError, enumLabel, errorView, field, fmtUnix,
  select, table, trimmed,
} from "../view.js";

const PAGE_SIZE = 50;
const UNENROLLED = "ENROLLMENT_REQUEST_STATE_UNENROLLED";
// The grammar of operator.proto: a host id is a node id (1 to 63 of `[a-z0-9-]`, not starting or
// ending with `-`).
const HOST_ID = /^(?=.{1,63}$)[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

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

// The request ids of the other pending requests that contest `request`'s record.
function contestedBy(request) {
  return Array.isArray(request.contestedBy) ? request.contestedBy : [];
}

// The ApproveRequest for `request` (a PendingEnrollment) from what the operator typed and chose,
// or a sentence saying what is missing or wrong, the host id checked against its grammar. The
// host group is the one chosen, or "" for none. A contested request is not approved, whatever was
// typed.
// The key hash is the one the list gave, sent back unchanged; `replace` is true only when its box
// is ticked. For a request that matches one record, an empty host id is sent empty (the
// coordinator approves it as the record's), and another host id than the record's needs Replace.
export function approval(request, form) {
  const hostId = trimmed(form.hostId);
  const hostGroup = trimmed(form.hostGroup);
  const record = recordHostId(request);
  const replace = form.replace === true;
  if (contestedBy(request).length > 0) {
    return "This request is contested: reject the other requests that match its record first.";
  }
  if (hostId === "" && record === "") {
    return "Name the host id.";
  }
  if (hostId !== "" && !HOST_ID.test(hostId)) {
    return "A host id is 1 to 63 of a-z, 0-9 and -, not starting or ending with -.";
  }
  if (record !== "" && hostId !== "" && hostId !== record && !replace) {
    return `This request matches the record of ${record}: approve it as ${record}, or tick Replace to approve it as ${hostId}.`;
  }
  return {
    requestId: request.requestId,
    keySha256: request.keySha256,
    hostId,
    hostGroup,
    replace,
  };
}

// The host group of the one record `request` matches: the record's group, or from a coordinator
// before groups its role; "" for none.
function recordGroup(request) {
  return request.recordHostGroup || request.recordRole || "";
}

// What the request's match against the inventory says, as one line.
export function matchLine(request) {
  switch (request.inventoryMatch) {
    case "INVENTORY_MATCH_MATCHED":
      return `Matches the record of ${request.recordHostId}`
        + (recordGroup(request) ? ` (host group ${recordGroup(request)}).` : " (no host group).");
    case "INVENTORY_MATCH_AMBIGUOUS":
      return `Matches several records: ${(request.candidateHostIds || []).join(", ")}. None is chosen.`;
    case "INVENTORY_MATCH_UNMATCHED":
      return "Matches no inventory record.";
    default:
      return "Not matched against the inventory.";
  }
}

// What the approval screen says beside a match line of one record. Its full stop is a string of
// its own: the source scan of tests/sources.rs refuses the text of a call of the console API.
export const NOT_PROOF = "Matched means this request claims a data MAC of the record and does not "
  + "contradict its BMC MAC. The machine wrote the claims itself, and anyone who read the label or "
  + "the network can write the same. Approve only the key hash shown on this machine's console" + ".";

// A text claim or record value, trimmed; "" for none.
function text(value) {
  return typeof value === "string" ? value.trim() : "";
}

// The window and state of the one record `request` matches, as a line; "" when it matches none or
// the coordinator says nothing of it.
export function recordWindowLine(request) {
  const word = enumLabel(request.recordState, "RECORD_STATE_");
  if (request.inventoryMatch !== "INVENTORY_MATCH_MATCHED" || word === "-") {
    return "";
  }
  const until = fmtUnix(request.recordApproveUntilUnix);
  return `The record is ${word}` + (until === "-" ? " (it has no window)." : `: its window ends ${until}.`);
}

// Whether `request` is a Linux request (it claims no Mac serial) that matches one record: by a
// data MAC, and not contradicting the record's BMC MAC.
function linuxMatch(request) {
  return request.inventoryMatch === "INVENTORY_MATCH_MATCHED" && text((request["claims"] || {}).macSerial) === "";
}

// A MAC in one spelling for comparing: lower case, without separators.
function macKey(text) {
  return text.toLowerCase().replace(/[:-]/g, "");
}

// How a claim compares with the record's value: "same", "differs", "not recorded" or "not
// claimed" (both missing: "not claimed"). `same` compares two values given.
function check(claimed, recorded, same) {
  if (claimed === "") {
    return "not claimed";
  }
  if (recorded === "") {
    return "not recorded";
  }
  return same(claimed, recorded) ? "same" : "differs";
}

// The claims of a matched Linux `request` that are hard to copy, each beside the record's value
// and how they compare; and a warning when the record holds a BMC MAC the request does not claim.
// Nothing for any other request: a Mac request is matched by its serial, which the match line
// already names the record of.
export function hardClaims(request) {
  if (!linuxMatch(request)) {
    return null;
  }
  const c = request["claims"] || {};
  const rows = [
    ["DMI serial", text(c.dmiSerial), text(request.recordSerial),
      (a, b) => a.toLowerCase() === b.toLowerCase()],
    ["BMC MAC", text(c.bmcMac), text(request.recordBmcMac), (a, b) => macKey(a) === macKey(b)],
    ["Product UUID", text(c.productUuid), "", (a, b) => a.toLowerCase() === b.toLowerCase()],
  ].map(([name, claimed, recorded, same]) => [name, claimed, recorded, check(claimed, recorded, same)]);
  const bmcUnclaimed = rows[1][3] === "not claimed" && rows[1][2] !== "";
  return h("div", {},
    table(["Hard to copy", "This request", "The record", ""], rows.map(([name, claimed, recorded, how]) =>
      h("tr", how === "same" ? {} : { class: "warning" },
        h("th", {}, name),
        h("td", {}, claimed || "not claimed"),
        h("td", {}, recorded || "not recorded"),
        h("td", {}, how)))),
    bmcUnclaimed
      ? h("p", { class: "warning", role: "alert" },
        `The record holds the BMC MAC ${rows[1][2]}, and this request does not claim one: `
        + "it matched on a data MAC alone.")
      : null);
}

// What a contested `request` says: the others that match its record, and what to do.
export function contestedLine(request) {
  const others = contestedBy(request);
  return `Contested: ${others.length === 1 ? "another pending request matches" : `${others.length} other pending requests match`} `
    + `the same record: ${others.join(", ")}. Approval is refused until all but one are rejected. `
    + "Find this machine's key hash on its console, and reject the requests that are not it.";
}

// The choices of the Host group control: blank, then each group of `groups` by name but `all`,
// which every host is in already.
function groupChoices(groups) {
  return [["", ""], ...groups.map((held) => {
    const name = (held["group"] || {}).name || "";
    return [name, name];
  }).filter(([name]) => name !== "" && name !== "all")];
}

function pending(ctx, request, groups) {
  const matched = request.inventoryMatch === "INVENTORY_MATCH_MATCHED";
  const choices = groupChoices(groups.held);
  const recorded = matched ? recordGroup(request) : "";
  const form = {
    hostId: request.renewalOf || recordHostId(request),
    hostGroup: choices.some(([name]) => name === recorded) ? recorded : "",
    replace: false,
  };
  return h("section", { class: "service" },
    h("h3", {}, `Request ${request.requestId}`),
    request.duplicateClaim === true
      ? h("p", { class: "error", role: "alert" },
        "Duplicate claim: another host presents the same machine id or key, or a MAC of an enrolled host. "
        + "Approving it needs Replace.")
      : null,
    contestedBy(request).length > 0
      ? h("p", { class: "error", role: "alert" }, contestedLine(request))
      : null,
    h("p", { class: request.inventoryMatch === "INVENTORY_MATCH_AMBIGUOUS" ? "warning" : "note" },
      matchLine(request)),
    recordWindowLine(request) === "" ? null : h("p", { class: "note" }, recordWindowLine(request)),
    linuxMatch(request) ? h("p", { class: "warning" }, NOT_PROOF) : null,
    hardClaims(request),
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
      select("Host group", "host-group", form, "hostGroup", choices),
      groups.failure === null ? null : h("p", { class: "warning", role: "alert" },
        `Host groups cannot be listed (${describeError(groups.failure)}): the host can be approved with no group.`),
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
        const reply = await ctx.api.enrollment.approve(body);
        ctx.refresh();
        const placed = body.hostGroup === ""
          ? "with no host group (it runs the packages of all until it is put in one)"
          : `into host group ${body.hostGroup}`;
        return `Approved ${reply.hostId || named} ${placed}: the host gets its certificate at its next poll.`;
      })),
    rejectSection(ctx, request));
}

// Reject: the request leaves the queue and its key is refused, so it asks first. The page asks for
// no reason (the API takes one, optional).
function rejectSection(ctx, request) {
  return h("fieldset", {},
    h("legend", {}, "Reject"),
    action(ctx, "Reject", "reject", async () => {
      if (!ctx.confirm(`Reject request ${request.requestId}? Its key is refused from then on.`)) {
        return "Not rejected.";
      }
      await ctx.api.enrollment.reject({ requestId: request.requestId });
      ctx.refresh();
      return `Rejected request ${request.requestId}.`;
    }));
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
    listed = await ctx.api.enrollment.list(body);
  } catch (error) {
    return errorView("Enrollment", error);
  }
  const requests = listed.requests || [];
  const next = typeof listed.nextPageToken === "string" ? listed.nextPageToken : "";
  const here = expired ? { unenrolled: "yes" } : {};
  // A coordinator that does not expire requests ignores `unenrolled` and lists its pending ones:
  // only a request it says is unenrolled is shown as one.
  const shown = expired ? requests.filter((request) => request.state === UNENROLLED) : requests;
  // The host groups to choose from, for the approval of a pending request: listed only when one is
  // shown; a List that fails leaves the choice at none.
  const groups = { held: [], failure: null };
  if (!expired && shown.length > 0) {
    try {
      groups.held = await allPages((token) => ctx.api.hostGroups.list(token === "" ? {} : { pageToken: token }), "hostGroups");
    } catch (error) {
      groups.failure = error;
    }
  }
  const empty = expired
    ? h("p", {}, requests.length === 0 ? "No request expired." : "This coordinator does not expire requests.")
    : h("p", {}, "No host is waiting.");
  return h("section", { class: "page" },
    h("h2", {}, expired ? "Enrollment: expired requests" : "Enrollment"),
    h("p", { class: "note" }, expired
      ? "A request no operator approves within the coordinator's request TTL expires, and its host is unenrolled."
      : "Claims are what a host says it is; they prove nothing. Approve names the key hash shown."),
    shown.length === 0 ? empty : shown.map((request) => (expired ? unenrolled(request) : pending(ctx, request, groups))),
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
