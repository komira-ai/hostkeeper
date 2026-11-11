// Rollout: per service, the desired version against the converged hosts, the update plan (its
// stages, the stage it is in, why it waits, and each stage's hosts with their states), the bake,
// the halt, the bad versions and the holders of the update slots; and the operator's overrides.
// Calls: Releases.Status, then Services.List for each service's roles, assignments and hosts; on a
// button, one of Releases.MarkBad, Releases.UnmarkBad, Releases.Halt, Releases.ClearHalt,
// Releases.Converge, Releases.Force, Releases.SetDesired, Rollouts.Advance, Rollouts.Pause,
// Rollouts.Resume, Services.Assign, Services.Unassign and Services.SetMinVersion, and Hosts.List
// to count a force's hosts. The page ends with the host groups section (pages/hostgroups.js). The desired version is set here or by CI.

import { section as hostGroupsSection } from "./hostgroups.js";
import {
  h, action, allPages, area, checkbox, describeError, enumLabel, errorView, field, list, reasonProblem, select,
  table, trimmed, words,
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

// Counts the hosts of `request` over a fresh Hosts.List, every page of it. `{ok: true, request,
// count}`, or `{ok: false, error}` when a page could not be read: then no force may be sent.
export async function prepareForce(api, request) {
  try {
    const hosts = await allPages((pageToken) => api.hosts.list(
      pageToken === "" ? {} : { pageToken }), "hosts");
    return { ok: true, request, count: forceTargetCount(request, hosts) };
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
  return api.releases.force(prepared.request);
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
    h("p", {}, "Switches the service on every targeted host at once, with no hook, past the update slots, the plan and the bake."),
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

function planStages(plan) {
  return plan ? list(plan, "stages") : [];
}

function isManual(stage) {
  return stage.advance === "STAGE_ADVANCE_MANUAL";
}

// How a stage is named in the plan row: the hosts it brings the service to, then in brackets its
// percent (a canary has none) and that an operator enters it.
function stageSize(stage) {
  const marks = [typeof stage.percent === "number" ? `${stage.percent}%` : "canary"];
  if (isManual(stage)) {
    marks.push("manual");
  }
  return `${stage.hosts || 0} (${marks.join(", ")})`;
}

// "1 (1%), 4 (50%, manual), 8 (100%) hosts of 8 eligible"; "none" when the service has no plan.
export function planSummary(plan) {
  const stages = planStages(plan);
  if (stages.length === 0) {
    return "none";
  }
  return `${stages.map(stageSize).join(", ")} hosts of ${plan.eligible || 0} eligible`;
}

// "2 of 3"; "0 of 3" before a manual first stage is entered.
export function stageProgress(plan) {
  const stages = planStages(plan);
  if (stages.length === 0) {
    return "-";
  }
  return `${plan.current || 0} of ${stages.length}${plan.finished === true ? ", every host rolled" : ""}`;
}

// The stage that a wait is about: the stage waiting for its Advance is the next one, the other
// waits that name a stage are the current one. The waits of the whole service (halt, pause, quiet
// start, other rollouts) and no wait name none.
function waitStage(plan) {
  const current = plan.current || 0;
  switch (plan.waiting) {
    case "PLAN_WAIT_MANUAL_ADVANCE":
      return current + 1;
    case "PLAN_WAIT_FAILED_HOST":
    case "PLAN_WAIT_BELOW_MIN_UP":
    case "PLAN_WAIT_UPDATING":
    case "PLAN_WAIT_DEFERRING":
    case "PLAN_WAIT_BAKING":
      return current;
    default:
      return null;
  }
}

// A service the plan waits on, with why its rollout is live and on which host when the
// coordinator says (`waitingOnWhy`; a coordinator before it names the service alone).
function liveOn(plan, service) {
  const why = (plan.waitingOnWhy || []).find((w) => w.service === service);
  if (!why) {
    return service;
  }
  const host = why.nodeId ? ` on ${why.nodeId}` : "";
  return `${service} (${enumLabel(why.reason, "LIVE_REASON_")}${host})`;
}

// Why the plan orders no new host: the wait with its stage or, for another service's rollout,
// the services it waits on, each with why it is live and on which host (none from a coordinator
// that does not name them), and the pause with who and why.
export function planWaiting(plan) {
  if (!plan) {
    return "-";
  }
  const parts = [];
  if (plan.waiting !== "PLAN_WAIT_PAUSED") {
    const stage = waitStage(plan);
    const on = (plan.waitingOn || []).map((service) => liveOn(plan, service)).join(", ");
    parts.push(
      enumLabel(plan.waiting, "PLAN_WAIT_") +
        (stage === null ? "" : `, stage ${stage}`) +
        (on === "" ? "" : ` on ${on}`),
    );
  }
  if (plan.paused === true) {
    parts.push(`paused by ${plan.pausedBy || "-"}: ${plan.pauseReason || "-"}`);
  }
  return parts.join("; ");
}

// The stages with their hosts, one row to a host; the stage's own cells are on its first row.
function planTable(plan) {
  const body = [];
  for (const stage of planStages(plan)) {
    const members = list(stage, "members");
    const mark = (stage.number || 0) === (plan.current || 0) ? " (current)" : "";
    const own = [`${stage.number || 0}${mark}`, typeof stage.percent === "number" ? `${stage.percent}%` : "canary",
      String(stage.hosts || 0), enumLabel(stage.advance, "STAGE_ADVANCE_")];
    const rows = members.length > 0 ? members : [null];
    rows.forEach((member, at) => {
      const first = at === 0 ? own : ["", "", "", ""];
      const host = member === null ? ["-", "-", "-"]
        : [member.nodeId, member.role || "-",
          enumLabel(member.state, "PLAN_HOST_STATE_") + (member.deferring === true ? " (deferring)" : "")];
      body.push(h("tr", {}, first.concat(host).map((text) => h("td", {}, text))));
    });
  }
  return table(["Stage", "Percent", "Hosts", "Enters", "Host", "Role", "State"], body);
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
    if (!ctx.confirm(`Set the desired version of ${service} to ${version}? Its hosts are moved there stage by stage, by the service's update plan.`)) {
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
    const reply = await ctx.api.releases.setDesired(request);
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
    await ctx.api.services.assign({ service, role });
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
    await ctx.api.services.unassign({ service, role, uninstall });
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
    await ctx.api.services.setMinVersion(
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

// The buttons of the plan: Advance while it waits for an operator to enter its next stage, Pause
// while it runs and Resume while it is paused. Each needs a reason and asks first.
function planButtons(ctx, service, plan, withReason) {
  const buttons = [];
  if (plan && plan.waiting === "PLAN_WAIT_MANUAL_ADVANCE") {
    const next = waitStage(plan);
    buttons.push(action(ctx, "Advance", "advance", withReason(async (reason) => {
      if (!ctx.confirm(`Advance ${service} into stage ${next} of ${planStages(plan).length}? Its hosts are ordered at their next heartbeats.`)) {
        return "Not advanced.";
      }
      const reply = await ctx.api.rollouts.advance({ service, reason });
      ctx.refresh();
      return reply.plan ? `Advanced ${service} into stage ${stageProgress(reply.plan)}.` : `Advanced ${service}.`;
    })));
  }
  if (plan && plan.paused === true) {
    buttons.push(action(ctx, "Resume", "resume", withReason(async (reason) => {
      if (!ctx.confirm(`Resume the plan of ${service}? Its hosts are ordered again from where it stands.`)) {
        return "Not resumed.";
      }
      await ctx.api.rollouts.resume({ service, reason });
      ctx.refresh();
      return `Resumed the plan of ${service}.`;
    })));
  } else {
    buttons.push(action(ctx, "Pause", "pause", withReason(async (reason) => {
      if (!ctx.confirm(`Pause the plan of ${service}? No host that has not started is ordered; the updates under way finish.`)) {
        return "Not paused.";
      }
      await ctx.api.rollouts.pause({ service, reason });
      ctx.refresh();
      return `Paused the plan of ${service}.`;
    })));
  }
  return buttons;
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
  const plan = svc.plan;
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
      ["Update slots", holders.length > 0 ? `held by ${holders.join(", ")}` : "none held"],
      ["Plan", planSummary(plan)],
      ["Stage", stageProgress(plan)],
      ["Waiting", planWaiting(plan)],
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
    planStages(plan).length > 0 ? planTable(plan) : null,
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
          await ctx.api.releases.markBad({ service, version, reason });
          ctx.refresh();
          return `Marked ${service} ${version} bad.`;
        })),
        (marks.bad || []).map((version) => action(ctx, `Unmark ${version}`, "unmark-bad", withReason(async (reason) => {
          await ctx.api.releases.unmarkBad({ service, version, reason });
          ctx.refresh();
          return `Unmarked ${service} ${version}.`;
        }))),
        halted
          ? action(ctx, "Clear the halt", "clear-halt", withReason(async (reason) => {
            await ctx.api.releases.clearHalt({ service, reason });
            ctx.refresh();
            return `Cleared the halt of ${service}.`;
          }))
          : action(ctx, "Halt", "halt", withReason(async (reason) => {
            if (!ctx.confirm(`Halt ${service}? No host of it is ordered until the halt is cleared.`)) {
              return "Not halted.";
            }
            await ctx.api.releases.halt({ service, reason });
            ctx.refresh();
            return `Halted ${service}.`;
          })),
        planButtons(ctx, service, plan, withReason),
        action(ctx, "Check convergence", "converge", async () => {
          const reply = await ctx.api.releases.converge({ service });
          const c = reply.convergence || {};
          return `${service}: ${enumLabel(c.state, "CONVERGENCE_STATE_")}, desired ${c.desired || "none"}`
            + (c.quiet === true ? ", quiet start" : "") + ".";
        }))));
}

export async function render(ctx) {
  let status;
  try {
    status = await ctx.api.releases.status({});
  } catch (error) {
    return errorView("Rollout", error);
  }
  const services = list(status, "services");
  let listing;
  try {
    listing = { entries: list(await ctx.api.services.list({}), "services") };
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
  const groups = await hostGroupsSection(ctx, () => ctx.show(page()));
  const page = () => h("section", { class: "page" },
    h("h2", {}, "Rollout"),
    h("p", { class: "note" }, "CI or an operator sets the desired version: an operator sets it here."),
    status.quiet === true ? h("p", { class: "note" }, "The coordinator is in its quiet start: it orders nothing yet.") : null,
    services.map((svc) => serviceSection(ctx, svc, listing)),
    forceSection(ctx, services, state, () => ctx.show(page())),
    groups.view());
  return page();
}
