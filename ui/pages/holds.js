// Holds: every hold that stands, with its reason, scope, setter and time; set and release one
// for a host, a role or the whole fleet. Calls: Services.List; on a button, Services.Hold or
// Services.Unhold. Both need a reason: a hold with an empty one is never sent.
//
// The page list has no entry for this page any more: its route opens the package page
// (`pages/package.js`), which builds on the parts this module exports.

import {
  h, action, errorView, field, fmtUnix, list, reasonProblem, scopeLabel, select, table, trimmed,
} from "../view.js";

// The HoldScope the form names, or a sentence saying what is missing.
export function holdScope(form) {
  if (form.scope === "fleet") {
    return { fleet: {} };
  }
  const value = trimmed(form.value);
  if (value === "") {
    return form.scope === "role" ? "Name the role." : "Name the host.";
  }
  return form.scope === "role" ? { role: value } : { host: value };
}

export async function render(ctx) {
  let listed;
  try {
    listed = await ctx.api.services.list({});
  } catch (error) {
    return errorView("Holds", error);
  }
  const services = list(listed, "services");
  const holds = listed.holds || [];
  const form = { service: "", scope: "host", value: "", reason: "" };
  const release = { reason: "" };
  const rows = holds.map((hold) => h("tr", {},
    h("td", {}, hold.service),
    h("td", {}, scopeLabel(hold.scope)),
    h("td", {}, hold.reason || "-"),
    h("td", {}, hold.setBy || "-"),
    h("td", {}, fmtUnix(hold.setUnix)),
    h("td", {}, action(ctx, "Release", "unhold", async () => {
      const reason = trimmed(release.reason);
      if (reason === "") {
        return "Give a reason for the release first.";
      }
      if (reasonProblem(reason) !== null) {
        return reasonProblem(reason);
      }
      await ctx.api.services.unhold(
        { scope: hold.scope, service: hold.service, reason });
      ctx.refresh();
      return `Released the hold of ${hold.service} (${scopeLabel(hold.scope)}).`;
    }))));
  return h("section", { class: "page" },
    h("h2", {}, "Holds"),
    h("p", { class: "note" },
      "A held service is ordered nothing where it is held: no update, install, rollback, uninstall or force without include held."),
    h("h3", {}, `Holds that stand (${holds.length})`),
    holds.length === 0 ? h("p", {}, "No hold stands.") : table(["Service", "Scope", "Reason", "Set by", "Set at", ""], rows),
    holds.length === 0 ? null : field("Reason for a release", "unhold-reason", release, "reason", { maxlength: 256 }),
    h("fieldset", {},
      h("legend", {}, "Hold a service"),
      select("Service", "hold-service", form, "service",
        [["", "choose"]].concat(services.map((s) => [s.service, s.service]))),
      select("Scope", "hold-scope", form, "scope", [["host", "a host"], ["role", "a role"], ["fleet", "the whole fleet"]]),
      field("Host or role", "hold-value", form, "value"),
      field("Reason (required)", "hold-reason", form, "reason", { maxlength: 256 }),
      action(ctx, "Hold", "hold", async () => {
        const service = trimmed(form.service);
        const reason = trimmed(form.reason);
        if (service === "") {
          return "Choose the service.";
        }
        if (reason === "") {
          return "A hold needs a reason.";
        }
        if (reasonProblem(reason) !== null) {
          return reasonProblem(reason);
        }
        const scope = holdScope(form);
        if (typeof scope === "string") {
          return scope;
        }
        await ctx.api.services.hold({ scope, service, reason });
        ctx.refresh();
        return `Held ${service} (${scopeLabel(scope)}).`;
      })));
}
