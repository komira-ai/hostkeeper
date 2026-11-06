// Rollout: per service, the desired version against the converged hosts, the canary and bake, the
// halt, the bad versions and the holder of the update slot; and the operator's overrides.
// Calls: Releases.Status, then Services.List for each service's roles, assignments and hosts; on a
// button, one of Releases.MarkBad, Releases.UnmarkBad, Releases.Halt, Releases.ClearHalt,
// Releases.Converge, Releases.Force, Releases.SetDesired, Services.Assign, Services.Unassign and
// Services.SetMinVersion, and Hosts.List to count a force's hosts. The desired version is set here or by CI.

import {
  h, action, area, checkbox, describeError, enumLabel, errorView, field, list, reasonProblem, select, table,
  trimmed, words,
} from "../view.js";

function runsService(view) {
  return view !== undefined && typeof view.installedVersion === "string";
}

// How many hosts a force reaches, from the request it would send (`target`, a ForceRequest:
// `service`, one of `fleet`, `role` and `hosts`, and `includeHeld`) and a fresh `Hosts.List`.
// The fleet and a role count the hosts that report the service installed; a list counts the
// listed hosts the coordinator knows, each once. A host that holds the pair is counted only
// with `includeHeld`. No target counts 0.
export function forceTargetCount(target, hosts) {
  const listed = target.hosts ? new Set(target.hosts.nodeIds || []) : null;
  let count = 0;
  for (const host of hosts) {
    const view = list(host, "services").find((s) => s.service === target.service);
    if (target.fleet) {
      if (!runsService(view)) {
        continue;
      }
    } else if (typeof target.role === "string") {
      if (host.role !== target.role || !runsService(view)) {
        continue;
      }
    } else if (listed === null || !listed.has(host.nodeId)) {
      continue;
    }
    const held = view !== undefined && (view.holds || []).length > 0;
    if (held && target.includeHeld !== true) {
      continue;
    }
    count += 1;
  }
  return count;
}

// Counts the hosts of `request` over a fresh Hosts.List. `{ok: true, request, count}`, or
// `{ok: false, error}` when the list could not be read: then no force may be sent.
export async function prepareForce(api, request) {
  try {
    const listed = await api.call("hostkeeper.operator.v1.Hosts/List", {});
    return { ok: true, request, count: forceTargetCount(request, listed.hosts || []) };
  } catch (error) {
    return { ok: false, error };
  }
}

// Whether the operator typed exactly the number of hosts the force reaches.
export function typedCountMatches(prepared, typed) {
  return prepared !== null && prepared.ok === true && typeof typed === "string"
    && /^[0-9]+$/.test(typed.trim()) && typed.trim() === String(prepared.count);
}

// Sends the prepared force if the typed number matches, and returns its reply; null, with nothing
// sent, otherwise.
export async function sendForce(api, prepared, typed) {
  if (!typedCountMatches(prepared, typed)) {
    return null;
  }
  return api.call("hostkeeper.operator.v1.Releases/Force", prepared.request);
}

function targetLabel(request) {
  if (request.fleet) {
    return "the whole fleet";
  }
  if (typeof request.role === "string") {
    return `role ${request.role}`;
  }
  return `hosts ${(request.hosts.nodeIds || []).join(", ")}`;
}

// The ForceRequest the form describes, or a sentence saying what is missing.
export function forceRequest(form) {
  const service = trimmed(form.service);
  const version = trimmed(form.version);
  const reason = trimmed(form.reason);
  if (service === "" || version === "" || reason === "") {
    return "Name the service, the version and a reason.";
  }
  const problem = reasonProblem(reason);
  if (problem !== null) {
    return problem;
  }
  const request = { service, version, reason, includeHeld: form.includeHeld === true };
  if (form.targetKind === "role") {
    if (trimmed(form.role) === "") {
      return "Name the role.";
    }
    request.role = trimmed(form.role);
  } else if (form.targetKind === "hosts") {
    const nodeIds = words(form.hosts);
    if (nodeIds.length === 0) {
      return "List the hosts.";
    }
    request.hosts = { nodeIds };
  } else {
    request.fleet = { confirmService: service };
  }
  return request;
}

function forceReply(reply) {
  const held = reply.held || [];
  const skipped = (reply.skipped || []).map((s) => `${s.nodeId} (${s.reason})`);
  return `Force sent: ${reply.targeted || 0} hosts ordered`
    + (held.length > 0 ? `; held, not ordered: ${held.join(", ")}` : "")
    + (skipped.length > 0 ? `; skipped: ${skipped.join(", ")}` : "")
    + ".";
}

// The force form moves through phases: `idle`; `counting`, while Hosts.List counts the hosts;
// `confirm`, while the dialog waits for the typed number; and `sending`, from the click on Force
// until its answer. Each count is an attempt with its own number: a count that finishes after it
// was cancelled or started again changes nothing. Cancel is offered while counting and
// confirming, never once the force is sent: a sent force cannot be called back.
function forceSection(ctx, services, state, redraw) {
  const form = state.force;
  const reset = () => {
    state.attempt += 1;
    state.phase = "idle";
    state.prepared = null;
    state.typed = "";
  };
  const cancel = action(ctx, "Cancel", "force-cancel", async () => {
    reset();
    redraw();
    return "Force cancelled: nothing was sent.";
  });
  const send = async () => {
    const prepared = state.prepared;
    if (state.phase !== "confirm" || !typedCountMatches(prepared, state.typed)) {
      return prepared !== null && prepared.ok
        ? `Not sent: type ${prepared.count}, the number of hosts, to confirm.`
        : "Not sent: the hosts have not been counted.";
    }
    state.phase = "sending";
    redraw();
    let reply;
    try {
      reply = await sendForce(ctx.api, prepared, state.typed);
    } catch (error) {
      if (error.code === "NETWORK" || error.code === "BAD_REPLY") {
        reset();
        redraw();
        return `The force may have been sent (${describeError(error)}). Look at this page and the audit`
          + " log before forcing again; a new force needs a new count.";
      }
      state.phase = "confirm";
      state.typed = "";
      redraw();
      return `Not forced: ${describeError(error)}. Type ${prepared.count} again to retry.`;
    }
    reset();
    redraw();
    ctx.refresh();
    return forceReply(reply);
  };
  const confirmation = () => {
    if (state.phase === "counting") {
      return h("div", { class: "confirm", role: "status" }, h("p", {}, "Counting the hosts..."), cancel);
    }
    if (state.phase === "sending") {
      const r = state.prepared.request;
      return h("div", { class: "confirm", role: "status" },
        h("p", {}, `Sending the force of ${r.service} to ${r.version} on ${state.prepared.count} hosts.`
          + " It cannot be cancelled."),
        h("button", { type: "button", name: "force-send", disabled: true }, "Force"));
    }
    if (state.phase !== "confirm") {
      return null;
    }
    const prepared = state.prepared;
    if (!prepared.ok) {
      return h("div", { class: "confirm", role: "alert" },
        h("p", {}, `The hosts could not be counted (${describeError(prepared.error)}), so the force cannot be`
          + " confirmed."),
        h("button", { type: "button", name: "force-send", disabled: true }, "Force"),
        cancel);
    }
    const r = prepared.request;
    return h("div", { class: "confirm", role: "alert" },
      h("p", {},
        `Force ${r.service} to ${r.version} on ${prepared.count} hosts (${targetLabel(r)}`
        + `${r.includeHeld ? ", held hosts included" : ""}). Running work on them is cut: the units stop without a drain.`),
      field(`Type ${prepared.count} to confirm`, "force-count", state, "typed", { inputmode: "numeric" }),
      action(ctx, "Force", "force-send", send),
      cancel);
  };
  return h("section", { class: "force" },
    h("h3", {}, "Force a version"),
    h("p", {}, "Switches the service on every targeted host at once, with no hook, past the update slot, the canary and the bake."),
    select("Service", "force-service", form, "service",
      [["", "choose"]].concat(services.map((s) => [s.service, s.service]))),
    field("Version", "force-version", form, "version"),
    field("Reason", "force-reason", form, "reason", { maxlength: 256 }),
    select("Target", "force-target", form, "targetKind",
      [["fleet", "the whole fleet"], ["role", "a role"], ["hosts", "listed hosts"]]),
    field("Role (for a role)", "force-role", form, "role"),
    field("Hosts (for listed hosts, separated by commas)", "force-hosts", form, "hosts"),
    checkbox("Include held hosts, for this force only (no hold is removed)", "force-include-held", form, "includeHeld"),
    action(ctx, "Count the hosts", "force-prepare", async () => {
      if (state.phase === "sending") {
        return "A force is being sent: wait for its answer.";
      }
      const request = forceRequest(form);
      if (typeof request === "string") {
        return request;
      }
      reset();
      const attempt = state.attempt;
      state.phase = "counting";
      redraw();
      const prepared = await prepareForce(ctx.api, request);
      if (state.attempt !== attempt) {
        return undefined;
      }
      state.prepared = prepared;
      state.phase = "confirm";
      redraw();
      return undefined;
    }),
    confirmation());
}

function slotHolders(svc) {
  return (svc.hosts || []).filter((host) => host.view && host.view.holdsSlot === true).map((host) => host.nodeId);
}

function byGate(svc, reason) {
  return (svc.hosts || []).filter((host) => host.view && host.view.gateReason === reason).map((host) => host.nodeId);
}

// The limits of a desired state that an operator's set keeps: CI's per-role maxUpdating and minUp.
function limitsOf(desired) {
  const limits = {};
  for (const key of ["maxUpdating", "minUp"]) {
    if (desired && desired[key] !== undefined) {
      limits[key] = desired[key];
    }
  }
  return limits;
}

function setDesired(ctx, svc, form) {
  const service = svc.service;
  return action(ctx, "Set desired", "set-desired", async () => {
    const version = trimmed(form.version);
    if (version === "") {
      return "Name the version.";
    }
    if (!ctx.confirm(`Set the desired version of ${service} to ${version}? Its hosts are moved there a role at a time.`)) {
      return "Desired version not changed.";
    }
    const request = { service, desired: Object.assign({ version }, limitsOf(svc.desired)) };
    // The release index and its digest go together or not at all; the API checks the pair. The
    // index goes as typed, not trimmed: its digest is of its exact bytes, a final newline included.
    if (trimmed(form.indexDigest) !== "") {
      request.releaseIndexDigest = trimmed(form.indexDigest);
    }
    if (trimmed(form.index) !== "") {
      request.releaseIndex = form.index;
    }
    const reply = await ctx.api.call("hostkeeper.operator.v1.Releases/SetDesired", request);
    ctx.refresh();
    const dropped = list(reply, "droppedRoles");
    return `Set the desired version of ${service} to ${version}.`
      + (dropped.length > 0 ? ` Limits dropped for roles: ${dropped.join(", ")}.` : "");
  });
}

function assignButton(ctx, service, form) {
  return action(ctx, "Assign to the role", "assign", async () => {
    const role = trimmed(form.role);
    if (role === "") {
      return "Name the role.";
    }
    await ctx.api.call("hostkeeper.operator.v1.Services/Assign", { service, role });
    ctx.refresh();
    return `Assigned ${service} to role ${role}.`;
  });
}

function unassignButton(ctx, service, form) {
  return action(ctx, "Unassign from the role", "unassign", async () => {
    const role = trimmed(form.role);
    if (role === "") {
      return "Name the role.";
    }
    const uninstall = form.uninstall === true;
    if (uninstall && !ctx.confirm(`Unassign ${service} from role ${role} and uninstall it from each host that loses its last assignment?`)) {
      return "Not unassigned.";
    }
    await ctx.api.call("hostkeeper.operator.v1.Services/Unassign", { service, role, uninstall });
    ctx.refresh();
    return uninstall
      ? `Unassigned ${service} from role ${role}, uninstalling it where no assignment is left.`
      : `Unassigned ${service} from role ${role}; its hosts keep running it, unmanaged.`;
  });
}

// Sets the minimum version of `service` for the role the form names. The API refuses a minimum
// above the desired version; the refusal is shown as it comes.
function minVersionButton(ctx, service, form) {
  return action(ctx, "Set the role's minimum version", "set-min-version", async () => {
    const role = trimmed(form.role);
    const version = trimmed(form.minVersion);
    if (role === "" || version === "") {
      return "Name the role and the minimum version.";
    }
    if (!ctx.confirm(`Set the minimum version of ${service} for role ${role} to ${version}? No host of the role is ordered below it, rollbacks included.`)) {
      return "Minimum version not changed.";
    }
    await ctx.api.call("hostkeeper.operator.v1.Services/SetMinVersion",
      { service, minVersion: { role, version } });
    ctx.refresh();
    return `Set the minimum version of ${service} for role ${role} to ${version}.`;
  });
}

// The role, version, digest and release index grammar is the API's to check: the form only
// refuses a blank version and role.
function assignFieldset(ctx, svc) {
  const service = svc.service;
  const form = { version: "", indexDigest: "", index: "", role: "", uninstall: false, minVersion: "" };
  return h("fieldset", {},
    h("legend", {}, `Assign and set the desired version of ${service}`),
    field("Version", "desired-version", form, "version"),
    field("Release index digest (64 hex characters)", "desired-index-digest", form, "indexDigest"),
    area("Release index (the text of index.json)", "desired-index", form, "index"),
    h("div", { class: "buttons" }, setDesired(ctx, svc, form)),
    field("Role", "assign-role", form, "role"),
    checkbox("Uninstall from each host that loses its last assignment", "unassign-uninstall", form, "uninstall"),
    h("div", { class: "buttons" }, assignButton(ctx, service, form), unassignButton(ctx, service, form)),
    field("Minimum version for the role", "min-version", form, "minVersion"),
    h("div", { class: "buttons" }, minVersionButton(ctx, service, form)));
}

// A minimum version of Services.List, by its scope: `role web 1.2.0`.
function minVersionLabel(min) {
  const scope = typeof min["host"] === "string" ? `host ${min["host"]}` : `role ${min.role}`;
  return `${scope} ${min.version}`;
}

// The rows of the assignments of `service` from Services.List; `entries` are its services. A
// service the list does not name has none.
function assignmentRows(entries, service) {
  const entry = entries.find((e) => e.service === service) || {};
  const assignments = list(entry, "assignments").map((given) => {
    const scope = typeof given["host"] === "string" ? `host ${given["host"]}` : `role ${given.role}`;
    return given.removed === true ? `removed from ${scope}` : scope;
  });
  const assigned = list(entry, "assigned");
  return [
    ["Assigned roles", list(entry, "roles").join(", ") || "none"],
    ["Operator assignments", assignments.join(", ") || "-"],
    ["Hosts assigned", String(assigned.length > 0 ? assigned.length : list(entry, "hosts").length)],
    ["Uninstalling", list(entry, "uninstalling").join(", ") || "-"],
    ["Minimum versions", list(entry, "minVersions").map(minVersionLabel).join(", ") || "-"],
  ];
}

// `listing` is `{entries}` of Services.List, or `{error}` when it failed.
function serviceSection(ctx, svc, listing) {
  const service = svc.service;
  const marks = svc.marks || {};
  const convergence = svc.convergence || {};
  const form = { reason: "", version: "" };
  const { halt } = marks;
  const halted = Boolean(halt) || convergence.state === "CONVERGENCE_STATE_HALTED";
  const withReason = (run) => async () => {
    if (trimmed(form.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(form.reason)) || run(trimmed(form.reason));
  };
  const canary = byGate(svc, "GATE_REASON_CANARY_WAIT");
  const baking = byGate(svc, "GATE_REASON_BAKING");
  const holders = slotHolders(svc);
  const roles = (convergence.roles || []).map((role) => h("tr", {},
    h("td", {}, role.role), h("td", {}, String(role.atDesired || 0)), h("td", {}, String(role.updating || 0)),
    h("td", {}, String(role.failed || 0)), h("td", {}, String(role.down || 0))));
  return h("section", { class: "service" },
    h("h3", {}, service),
    table(["", ""], [
      ["Desired", svc.desired && svc.desired.version ? svc.desired.version : "none set"],
      ...(listing.error ? [] : assignmentRows(listing.entries, service)),
      ["State", enumLabel(convergence.state, "CONVERGENCE_STATE_")],
      ["Up", `${svc.up || 0} hosts`],
      ["Update slot", holders.length > 0 ? `held by ${holders.join(", ")}` : "free"],
      ["Waiting for the canary", canary.length > 0 ? canary.join(", ") : "-"],
      ["Baking", baking.length > 0 ? baking.join(", ") : "-"],
      ["Halt", halt ? `by ${halt.nodeId || "-"}, check ${halt.check || "-"}, target ${halt["target"] || "-"}`
        : (halted ? "halted" : "-")],
      ["Last good", typeof marks.lastGood === "string" ? marks.lastGood : "-"],
      ["Bad versions", (marks.bad || []).join(", ") || "-"],
      ["Bad, left by a force", (marks.badByForce || []).join(", ") || "-"],
      ["Force standing", convergence.force ? `${convergence.force.version} (${convergence.force.reason || "-"})` : "-"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))),
    listing.error ? h("p", { class: "error", role: "alert" }, describeError(listing.error)) : null,
    table(["Role", "At desired", "Updating", "Failed", "Down"], roles),
    assignFieldset(ctx, svc),
    h("fieldset", {},
      h("legend", {}, `Act on ${service}`),
      field("Reason", "reason", form, "reason", { maxlength: 256 }),
      field("Version to mark bad", "bad-version", form, "version"),
      h("div", { class: "buttons" },
        action(ctx, "Mark bad", "mark-bad", withReason(async (reason) => {
          const version = trimmed(form.version);
          if (version === "") {
            return "Name the version to mark bad.";
          }
          await ctx.api.call("hostkeeper.operator.v1.Releases/MarkBad", { service, version, reason });
          ctx.refresh();
          return `Marked ${service} ${version} bad.`;
        })),
        (marks.bad || []).map((version) => action(ctx, `Unmark ${version}`, "unmark-bad", withReason(async (reason) => {
          await ctx.api.call("hostkeeper.operator.v1.Releases/UnmarkBad", { service, version, reason });
          ctx.refresh();
          return `Unmarked ${service} ${version}.`;
        }))),
        halted
          ? action(ctx, "Clear the halt", "clear-halt", withReason(async (reason) => {
            await ctx.api.call("hostkeeper.operator.v1.Releases/ClearHalt", { service, reason });
            ctx.refresh();
            return `Cleared the halt of ${service}.`;
          }))
          : action(ctx, "Halt", "halt", withReason(async (reason) => {
            if (!ctx.confirm(`Halt ${service}? No host of it is ordered until the halt is cleared.`)) {
              return "Not halted.";
            }
            await ctx.api.call("hostkeeper.operator.v1.Releases/Halt", { service, reason });
            ctx.refresh();
            return `Halted ${service}.`;
          })),
        action(ctx, "Check convergence", "converge", async () => {
          const reply = await ctx.api.call("hostkeeper.operator.v1.Releases/Converge", { service });
          const c = reply.convergence || {};
          return `${service}: ${enumLabel(c.state, "CONVERGENCE_STATE_")}, desired ${c.desired || "none"}`
            + (c.quiet === true ? ", quiet start" : "") + ".";
        }))));
}

export async function render(ctx) {
  let status;
  try {
    status = await ctx.api.call("hostkeeper.operator.v1.Releases/Status", {});
  } catch (error) {
    return errorView("Rollout", error);
  }
  const services = list(status, "services");
  let listing;
  try {
    listing = { entries: list(await ctx.api.call("hostkeeper.operator.v1.Services/List", {}), "services") };
  } catch (error) {
    listing = { error };
  }
  const state = {
    force: { service: "", version: "", reason: "", targetKind: "fleet", role: "", hosts: "", includeHeld: false },
    phase: "idle",
    attempt: 0,
    prepared: null,
    typed: "",
  };
  const page = () => h("section", { class: "page" },
    h("h2", {}, "Rollout"),
    h("p", { class: "note" }, "CI or an operator sets the desired version: an operator sets it here."),
    status.quiet === true ? h("p", { class: "note" }, "The coordinator is in its quiet start: it orders nothing yet.") : null,
    services.map((svc) => serviceSection(ctx, svc, listing)),
    forceSection(ctx, services, state, () => ctx.show(page())));
  return page();
}
