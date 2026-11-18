// The "Service identity" section of the Host page: whether the coordinator signs certificates for
// the services of the host that ask for an identity, and the services' identities as the host's
// last heartbeat reported them.
// The Host page makes the calls and hands them in as `calls` (calls belong to pages/): `get()`,
// HostIdentity.Get of the host, and `approve(keySha256)`, HostIdentity.Approve, which the
// "Approve identity" button makes after a confirm, with the key hash Get answered. A refused Get (`unknown_host` for a host the
// coordinator has no enrollment of, `unimplemented` of a coordinator that predates the service) is
// said in the section and nothing else of the page is affected.
//
// The section shows the assurance (`operator`: an operator approved the host's key by hand; `auto`:
// the coordinator approved it by an inventory record), the key's SHA-256, whether certificates are
// signed for the host (allowed), the approval (who, when) and whether an operator retired the host
// (denied). The button is offered for an `auto` host that is neither allowed nor denied.

import { action, describeError, enumLabel, fmtUnix, h, list } from "./view.js";

function facts(rows) {
  return h("div", { class: "table-wrap cols card" }, h("table", { class: "kv" },
    h("tbody", {}, rows.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v))))));
}

// The services' identities the host's last heartbeat reported: the service, when its certificate
// ends and its last error. `heartbeat` is the host's `heartbeat` (absent when none was heard).
function reported(heartbeat) {
  const rows = list(heartbeat || {}, "serviceIdentities");
  if (rows.length === 0) {
    return h("p", { class: "quiet" }, "The host reports no service identity.");
  }
  return h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, ["Service", "Certificate ends", "Last error"].map((t) => h("th", {}, t)))),
    h("tbody", {}, rows.map((row) => h("tr", {},
      h("td", {}, row.service || "-"),
      h("td", {}, fmtUnix(row.notAfterUnix)),
      h("td", {}, typeof row.lastError === "string" && row.lastError !== "" ? row.lastError : "-"))))));
}

export async function identitySection(ctx, nodeId, heartbeat, calls) {
  let identity;
  try {
    identity = await calls.get();
  } catch (error) {
    return h("section", { class: "identity" }, h("h3", {}, "Service identity"),
      h("p", { class: "warning", role: "alert" }, `The service identity cannot be read: ${describeError(error)}`),
      reported(heartbeat));
  }
  const approval = identity.approval;
  const assurance = enumLabel(identity.assurance, "HOST_ASSURANCE_");
  const offer = assurance === "auto" && identity.allowed !== true && identity.denied !== true;
  return h("section", { class: "identity" }, h("h3", {}, "Service identity"),
    facts([
      ["Assurance", assurance],
      ["Key SHA-256", identity.keySha256 || "-"],
      ["Certificates signed for its services", identity.allowed === true ? "yes" : "no"],
      ["Approved by", approval ? `${approval.approver || "-"} at ${fmtUnix(approval.approvedUnix)}` : "-"],
      ["Denied", identity.denied === true ? "yes: an operator retired the host" : "no"],
    ]),
    offer
      ? h("div", { class: "buttons" }, action(ctx, "Approve identity", "approve-identity", async () => {
        const keySha256 = identity.keySha256;
        if (!ctx.confirm(`Approve the identity of ${nodeId}? The coordinator will sign certificates for its `
          + `services that ask for an identity, for its current key ${keySha256}.`)) {
          return "Identity not approved.";
        }
        await calls.approve(keySha256);
        ctx.refresh();
        return `Approved the identity of ${nodeId}: the coordinator signs certificates for its services.`;
      }, { class: "primary" }))
      : null,
    reported(heartbeat));
}
