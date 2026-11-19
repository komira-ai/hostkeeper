// Host: one host's services, holds, marks and last update, the host-inventory record its
// enrollment matched (linked to the Inventory page) with that record's data MACs, BMC MAC and
// serial, when it enrolled, its certificate and its Sigstore trusted root as its last heartbeat
// said them, and what an operator may do to it. The page reads the time from `ctx.now()` (unix
// seconds), once per rendering, to say how soon the certificate ends.
// Calls: Hosts.Get, then Services.List for the services assigned to the host (it is called even
// when Hosts.Get fails, and each failure is shown as is), then, when a service is assigned to the
// host, Audit.List of the host's entries, page by page, to mark a service whose last uninstall was
// cancelled; on
// a button, one of Hosts.Exclude, Hosts.Include, Hosts.Abort, Hosts.SetLabels, Services.Hold,
// Services.Unhold and, after a confirm, Hosts.Retire. HostGroups.List (every page) gives the groups
// the "Host group" dropdown offers, and HostGroups.Move, after a confirm, moves the host into the
// group chosen, or into none when the blank is chosen; a host in no group is Unassigned and is ordered no package. Retire is offered when Hosts.Get fails too,
// for an enrolled host the coordinator has not heard from since it started (a dead one).
// The "Power and boot" tab makes no call when the page is drawn; on a button, after a confirm,
// Console.Power (status, on, soft shutdown, off, cycle or reset of the host's BMC) or
// Console.SetBootDevice (disk, network or BIOS setup, for the next boot or every boot), each with
// a required reason and with no refresh: the host's state changes slowly.
// The Remote SSH tab is `sshpanel.js`: Ssh.HostKeys is called when the page is read, and on a button
// Ssh.PinHostKeys (after a confirm); its terminal is a socket from `ctx.openSocket`.
// Alerts.List is called last when the page is read, for the alerts that name the host; when it
// fails, the page says so and shows the reasons the host's own report gives. On a button of a
// reason, after a confirm, Releases.ClearHalt (with the reason typed) or Hosts.Retry.
//
// The page is a status header (name, status LED and word, role, group, what it runs, the last
// heartbeat, and a small elevation of the rack it stands in), then, when the host is not healthy,
// why: every reason (`health.js`) in plain words, with when it began and the one action that fixes
// it (Clear halt, Retry update), and for a js-agent taint what hostkeeper holds of it; then tabs (Overview, Services,
// Certificate, Labels, Placement, Host group, Remote SSH, Power and boot; every panel is in the page, the stylesheet shows the
// chosen one, and the choice is kept across a refresh of the same host), then the danger zone,
// which holds Retire. The status says "degraded" for a live host with any reason: a service of it
// unhealthy, halted or whose last update failed, or an alert that names it; the lab's state alone
// would call it healthy.

import {
  h, action, allPages, checkbox, describeError, enumLabel, errorView, field, fmtUnix, list, pill, reasonProblem,
  scopeLabel, select, table, trimmed,
} from "../view.js";
import { isDegraded, reasonsOf } from "../health.js";
import { crumbs, hostState, layoutOf, layoutProblem, rackName, slotOf, slotOptions, subjectOf } from "../lab.js";
import { loadSsh, sshPanel } from "../sshpanel.js";

// Rows of a facts table: a name and its value, no heading strip.
function kv(rows, extra) {
  return h("div", { class: `table-wrap ${extra || ""}`.trim() }, h("table", { class: "kv" },
    h("tbody", {}, rows.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v))))));
}

// The words of an enum value without its underscores (`up_to_date` reads `up to date`).
function plain(text) {
  return typeof text === "string" ? text.replace(/_/g, " ") : text;
}

// The status a host header shows: the lab's state, but a live host with a reason (`reasons`, of
// `health.js`) or whose service reports unhealthy in its last heartbeat is `degraded`, never
// `healthy` or `updating`. `[word, tone]`.
export function statusOf(host, reasons = []) {
  const state = hostState(host);
  const heart = host.heartbeat && list(host.heartbeat, "services").some((s) => s.health === "HEALTH_UNHEALTHY");
  if (state === "failed" || (state === "healthy" && heart) || isDegraded(host, reasons)) {
    return ["degraded", "bad"];
  }
  return [state, { healthy: "ok", updating: "info", offline: "bad", pending: "muted" }[state]];
}

// A label key or value, as SetLabels takes it: 1 to 32 bytes of `[a-z0-9-]`.
const LABEL = /^[a-z0-9-]{1,32}$/;
// The most labels a host may have.
const MAX_LABELS = 8;

// `key=value` pairs separated by commas or spaces, as SetLabels takes them: `{labels}`, or
// `{problem}`, a sentence saying why not. Each key and value must follow the label grammar (so
// no key reaches the object's prototype), no key may be given twice, and there may be at most 8.
// An empty text is the empty set, which removes every label.
export function parseLabels(text) {
  const labels = Object.create(null);
  let count = 0;
  for (const pair of trimmed(text).split(/[\s,]+/).filter((p) => p !== "")) {
    const at = pair.indexOf("=");
    const key = pair.slice(0, at);
    const value = pair.slice(at + 1);
    if (at < 0 || !LABEL.test(key) || !LABEL.test(value)) {
      return { problem: `Labels are key=value pairs, each key and value 1 to 32 of a-z, 0-9 and -: not ${pair}.` };
    }
    if (key in labels) {
      return { problem: `The label ${key} is given twice.` };
    }
    labels[key] = value;
    count += 1;
  }
  if (count > MAX_LABELS) {
    return { problem: `A host has at most ${MAX_LABELS} labels, not ${count}.` };
  }
  return { labels };
}

// The reason sent with every move: the form asks for none.
const DEFAULT_MOVE_REASON = "moved in the console";

// The Host group form: a dropdown of the groups held, the host's own selected (the
// blank, "Unassigned", for a host in no group), and Move. `groups` is the
// names the coordinator holds, or null with `groupsFailure` when it could not be listed.
function hostGroupSection(ctx, nodeId, host, groups, groupsFailure) {
  const current = typeof host.hostGroup === "string" && host.hostGroup !== "" ? host.hostGroup : "";
  const names = (groups || []).slice();
  if (current !== "" && !names.includes(current)) {
    names.push(current);
  }
  const form = { hostGroup: current };
  return h("fieldset", { class: "card" },
    h("legend", {}, "Host group"),
    h("p", { class: "note" },
      "A host is in one host group or is Unassigned. A host is ordered the packages of its group, which always lists hostkeeper; an Unassigned host is ordered no package and keeps what it runs."),
    groupsFailure === null ? null : h("p", { class: "error", role: "alert" }, `Host groups: ${describeError(groupsFailure)}`),
    select("Host group", "host-group", form, "hostGroup",
      [["", "Unassigned"], ...names.map((name) => [name, name])]),
    action(ctx, "Move", "move-host", async () => {
      const target = trimmed(form.hostGroup);
      if (target === current) {
        return current === "" ? `${nodeId} is Unassigned.` : `${nodeId} is already in host group ${current}.`;
      }
      const question = target === ""
        ? `Remove ${nodeId} from host group ${current}? It is then Unassigned: it is ordered no package and keeps what it runs.`
        : `Move ${nodeId} to host group ${target}? It is ordered that group's packages at its next heartbeat.`;
      if (!ctx.confirm(question)) {
        return "Not moved.";
      }
      const reply = await ctx.api.hostGroups.move({ nodeId, hostGroup: target, reason: DEFAULT_MOVE_REASON });
      ctx.refresh();
      return `Moved ${nodeId} from ${reply.fromHostGroup || "Unassigned"} to ${target === "" ? "Unassigned" : target}.`;
    }, { class: "primary" }));
}

// Less than this many seconds of a certificate left is a warning: 48 hours.
export const CERT_WARN_SECS = 48 * 3600;

// The state of a certificate whose last valid second is `notAfter` (unix seconds, a string or a
// number as Hosts.Get answers it; absent when the host said none) when the clock reads `now`
// (unix seconds): "unknown" without an end, "expired" once `now` is past the end, "soon" while
// less than CERT_WARN_SECS are left, and "ok" otherwise.
export function certState(notAfter, now) {
  const end = Number(notAfter);
  if (notAfter === undefined || notAfter === null || notAfter === "" || !Number.isFinite(end) || end <= 0) {
    return "unknown";
  }
  if (now > end) {
    return "expired";
  }
  return end - now < CERT_WARN_SECS ? "soon" : "ok";
}

// The certificate of `host` as its last heartbeat said it, at `now`: its end, the last renewal
// error, the count of heartbeats whose clock was off, and a warning when the end is near or past
// or a renewal error is reported.
function certificate(host, now) {
  const state = certState(host.certNotAfterUnix, now);
  const error = typeof host.lastRenewError === "string" && host.lastRenewError !== "" ? host.lastRenewError : null;
  const ends = fmtUnix(host.certNotAfterUnix);
  const warnings = [];
  if (state === "expired") {
    warnings.push(`The certificate expired at ${ends}.`);
  } else if (state === "soon") {
    warnings.push(`The certificate ends in less than 48 hours, at ${ends}.`);
  }
  if (error !== null) {
    warnings.push(`The last renewal failed: ${error}`);
  }
  const skewed = host.clockSkewBeats === undefined ? "0" : String(host.clockSkewBeats);
  return h("section", { class: "certificate" },
    h("h3", {}, "Certificate"),
    kv([
      ["Ends", state === "unknown" ? "not reported" : ends],
      ["Last renewal error", error || "-"],
      ["Heartbeats with the clock off", skewed],
    ]),
    warnings.map((text) => h("p", { class: "warning", role: "alert" }, text)));
}

// Seconds in a day, the unit a trusted root's age is shown in.
const DAY_SECS = 86400;

// The state of a trusted root whose reason Hosts.Get answers as `reason` (`trustedRootReason`,
// the enum's name; absent when unspecified): "stale", "missing" or "unparseable" when it needs
// attention, "ok" when it needs none, and "unknown" when the host's last heartbeat said nothing
// of it (absent, unspecified, or a value this page does not know).
export function rootState(reason) {
  switch (reason) {
    case "TRUSTED_ROOT_REASON_NONE":
      return "ok";
    case "TRUSTED_ROOT_REASON_STALE":
      return "stale";
    case "TRUSTED_ROOT_REASON_MISSING":
      return "missing";
    case "TRUSTED_ROOT_REASON_UNPARSEABLE":
      return "unparseable";
    default:
      return "unknown";
  }
}

// What the page says of each state of a trusted root, and the warning of each that needs
// attention.
const ROOT_SAYS = {
  ok: ["needs no attention", null],
  stale: ["stale", "The Sigstore trusted root is stale: package checks go on with it, and refuse every package "
    + "signed after a key rotation it does not hold. Update hostkeeper on the host, or copy a current root to it."],
  missing: ["missing", "The host has no Sigstore trusted root: every package check on it fails."],
  unparseable: ["unparseable", "The host's Sigstore trusted root cannot be read: every package check on it fails."],
  unknown: ["not reported", null],
};

// The age of a trusted root as Hosts.Get answers it (`trustedRootAgeSecs`, a string or a number;
// absent when the host said none): whole days and the seconds, or "not reported".
export function rootAge(secs) {
  const age = Number(secs);
  if (secs === undefined || secs === null || secs === "" || !Number.isFinite(age) || age < 0) {
    return "not reported";
  }
  return `${Math.floor(age / DAY_SECS)} days (${age} s)`;
}

// The trusted root of `host` as its last heartbeat said it: its state and age, and a warning when
// it needs attention.
function trustedRoot(host) {
  const [said, warning] = ROOT_SAYS[rootState(host.trustedRootReason)];
  return h("section", { class: "trusted-root" },
    h("h3", {}, "Trusted root"),
    kv([
      ["State", said],
      ["Age", rootAge(host.trustedRootAgeSecs)],
    ]),
    warning === null ? [] : [h("p", { class: "warning", role: "alert" }, warning)]);
}

function lastUpdate(result) {
  if (!result) {
    return h("p", {}, "No update reported yet.");
  }
  const facts = [
    ["Target", result["target"] || "-"],
    ["Verdict", enumLabel(result.outcome, "OUTCOME_") + (result.forced === true ? " (forced)" : "")],
    ["Phase", enumLabel(result.phase, "FAILURE_PHASE_")],
    ["Class", enumLabel(result.class, "FAILURE_CLASS_")],
    ["Reason", result.reason || "-"],
    ["Detail", result.detail || "-"],
    ["Try", result.orderTry === undefined ? "-" : String(result.orderTry)],
    ["Order", result.orderId || "-"],
    ["Started", fmtUnix(result.startedUnix)],
  ];
  return h("div", { class: "update" },
    h("h3", { class: "sub" }, "Last update"),
    kv(facts, "cols"),
    h("pre", { class: "log" }, (result.logTail || []).map((line) => `${line}\n`)));
}

function holds(view) {
  const list = view.holds || [];
  if (list.length === 0) {
    return h("p", {}, "Not held.");
  }
  return h("ul", {}, list.map((hold) => h("li", {},
    `Held (${scopeLabel(hold.scope)}) by ${hold.setBy || "-"} at ${fmtUnix(hold.setUnix)}: `,
    hold.reason || "-")));
}

function serviceSection(ctx, nodeId, view) {
  const service = view.service;
  const form = { reason: "", abandon: false };
  const needReason = (run) => async () => {
    if (trimmed(form.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(form.reason)) || run();
  };
  const reason = () => trimmed(form.reason);
  const healthWord = enumLabel(view["health"], "HEALTH_");
  const tone = view.health === "HEALTH_UNHEALTHY" ? "bad" : view.health === "HEALTH_HEALTHY" ? "ok" : "muted";
  return h("section", { class: "service card" },
    h("div", { class: "card-head" },
      h("h3", {}, service),
      pill(healthWord, tone),
      pill(view.up === true ? "up" : "down", view.up === true ? "ok" : "bad"),
      view.holdsSlot === true ? pill("holds the update slot", "info") : null,
      h("span", { class: "ver" }, typeof view.installedVersion === "string" ? view.installedVersion : "not installed")),
    kv([
      ["Installed version", typeof view.installedVersion === "string" ? view.installedVersion : "not installed"],
      ["Health", healthWord],
      ["Up", view.up === true ? "yes" : "no"],
      ["Update slot", view.holdsSlot === true ? "holds the update slot" : "-"],
      ["Gate", plain(enumLabel(view.gateReason, "GATE_REASON_"))],
      ["Membership", plain(enumLabel(view.membership, "MEMBERSHIP_"))],
      ["Excluded", typeof view.excludedReason === "string" ? `yes: ${view.excludedReason}` : "no"],
    ], "cols"),
    holds(view),
    lastUpdate(view.lastUpdate),
    h("fieldset", { class: "act" },
      h("legend", {}, `Act on ${service} here`),
      field("Reason", "reason", form, "reason", { maxlength: 256 }),
      checkbox("Abandon: also free the update slot this host holds for the service (the host is gone)",
        "abandon", form, "abandon"),
      h("div", { class: "buttons" },
        action(ctx, "Exclude", "exclude", needReason(async () => {
          if (form.abandon === true
            && !ctx.confirm(`Exclude ${nodeId} from ${service} and free the update slot it holds (abandon)?`)) {
            return "Not excluded.";
          }
          await ctx.api.hosts.exclude(
            { service, nodeId, reason: reason(), abandon: form.abandon === true });
          ctx.refresh();
          return `Excluded ${nodeId} from ${service}.`;
        })),
        action(ctx, "Include", "include", async () => {
          await ctx.api.hosts.include({ service, nodeId });
          ctx.refresh();
          return `Included ${nodeId} in ${service}.`;
        }),
        action(ctx, "Abort the running update", "abort", needReason(async () => {
          const reply = await ctx.api.hosts.abort({ nodeId, service, reason: reason() });
          ctx.refresh();
          return reply.queued === true
            ? `Abort of ${service} queued for ${nodeId}'s next heartbeat.`
            : `Nothing to abort: ${nodeId} runs nothing of ${service}, so no abort was queued.`;
        })),
        action(ctx, "Hold here", "hold", needReason(async () => {
          await ctx.api.services.hold(
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Held ${service} on ${nodeId}.`;
        })),
        action(ctx, "Release the hold here", "unhold", needReason(async () => {
          await ctx.api.services.unhold(
            { scope: { host: nodeId }, service, reason: reason() });
          ctx.refresh();
          return `Released the hold of ${service} on ${nodeId}.`;
        })))));
}

// The power buttons: the proto's action, the button's name and label, the words of what was sent,
// and what the confirm says. `cuts` buttons stop the host's running system and are shown as danger.
const POWER_BUTTONS = [
  ["POWER_ACTION_STATUS", "power-status", "Status", "status", null],
  ["POWER_ACTION_ON", "power-on", "On", "on", null],
  ["POWER_ACTION_SOFT", "power-soft", "Soft shutdown", "soft shutdown",
    "Soft shutdown asks the host's system to shut itself down; it does so in its own time, and this answer does not say when it is off."],
  ["POWER_ACTION_OFF", "power-off", "Off", "off",
    "Off cuts the power: the running system is stopped at once, and a soft shutdown still running is cut."],
  ["POWER_ACTION_CYCLE", "power-cycle", "Cycle", "cycle",
    "Cycle cuts the power and turns it on: the running system is stopped at once, and a soft shutdown still running is cut."],
  ["POWER_ACTION_RESET", "power-reset", "Reset", "reset",
    "Reset restarts the host at once: the running system is stopped, and a soft shutdown still running is cut."],
];

// The words of a power action, as the answer names it (`POWER_ACTION_SOFT` is `soft shutdown`).
function powerWord(value) {
  const found = POWER_BUTTONS.find(([id]) => id === value);
  return found === undefined ? plain(enumLabel(value, "POWER_ACTION_")) : found[3];
}

// What a Console.Power answer says: the state the BMC reported before the command, and the command
// sent (the coordinator sends `on` for a cycle of a host that is off).
export function powerReport(nodeId, action, reply) {
  const was = reply.state === "POWER_STATE_ON" ? "on" : reply.state === "POWER_STATE_OFF" ? "off" : "in a state the BMC did not say";
  const sent = typeof reply.sent === "string" ? reply.sent : "";
  if (action === "POWER_ACTION_STATUS" || sent === "" || sent === "POWER_ACTION_STATUS") {
    return `${nodeId} is ${was}.`;
  }
  const tail = sent === "POWER_ACTION_SOFT" ? " The host shuts down in its own time." : "";
  return sent === action
    ? `${nodeId} was ${was}; ${powerWord(sent)} sent.${tail}`
    : `${nodeId} was ${was}; ${powerWord(sent)} sent instead of ${powerWord(action)}.${tail}`;
}

// Boot flags in words: `disk, every boot, UEFI`.
function bootFlags(device, persistent, efi) {
  const name = { BOOT_DEVICE_DISK: "disk", BOOT_DEVICE_PXE: "the network (PXE)", BOOT_DEVICE_BIOS: "BIOS setup" }[device]
    || plain(enumLabel(device, "BOOT_DEVICE_"));
  return `${name}, ${persistent === true ? "every boot" : "next boot only"}, ${efi === true ? "UEFI" : "legacy BIOS"}`;
}

// The flags of a Console.SetBootDevice answer, as the BMC reads them back.
export function bootReport(nodeId, reply) {
  if (typeof reply.device !== "string" || reply.device === "BOOT_DEVICE_UNSPECIFIED") {
    return `The BMC of ${nodeId} holds no boot device override.`;
  }
  return `The BMC of ${nodeId} now boots ${bootFlags(reply.device, reply.persistent, reply.efi)}.`;
}

// Power and boot: the host's BMC, reached by the coordinator, never by the browser. Every button
// needs a reason, asks first (the confirm is a courtesy; the coordinator decides) and is audited.
// No refresh follows: the host's state changes slowly, and the answer says what the BMC reported.
// The Console page (`serialconsole.js`) shows the same section beside its terminal.
export function powerSection(ctx, nodeId) {
  const form = { reason: "", device: "BOOT_DEVICE_DISK", persistent: false, efi: false };
  const needReason = (run) => async () => {
    if (trimmed(form.reason) === "") {
      return "Give a reason first.";
    }
    return reasonProblem(trimmed(form.reason)) || run(trimmed(form.reason));
  };
  const power = ([id, name, label, , warning]) => action(ctx, label, name, needReason(async (reason) => {
    const question = warning === null ? `Send ${label.toLowerCase()} to ${nodeId}?` : `Send ${label.toLowerCase()} to ${nodeId}? ${warning}`;
    if (!ctx.confirm(question)) {
      return "Not sent.";
    }
    return powerReport(nodeId, id, await ctx.api.console.power({ hostId: nodeId, action: id, reason }));
  }), warning !== null && id !== "POWER_ACTION_SOFT" ? { class: "danger" } : {});
  return h("fieldset", { class: "act power card" },
    h("legend", {}, "Power and boot"),
    h("p", { class: "note" },
      "This is the BMC of a Linux server, reached by the coordinator. Every press is audited with the reason. "
      + "To fix a host that boots the wrong thing: set the boot device to disk, every boot, then cycle."),
    field("Reason", "power-reason", form, "reason", { maxlength: 256 }),
    h("div", { class: "buttons" }, POWER_BUTTONS.map(power)),
    select("Boot device", "boot-device", form, "device", [
      ["BOOT_DEVICE_DISK", "Disk"], ["BOOT_DEVICE_PXE", "Network (PXE)"], ["BOOT_DEVICE_BIOS", "BIOS setup"]]),
    checkbox("Every boot from now on", "boot-persistent", form, "persistent"),
    checkbox("UEFI", "boot-efi", form, "efi"),
    action(ctx, "Set boot device", "set-boot-device", needReason(async (reason) => {
      const { device, persistent, efi } = form;
      if (!ctx.confirm(`Set the boot device of ${nodeId} to ${bootFlags(device, persistent, efi)}?`)) {
        return "Not sent.";
      }
      return bootReport(nodeId, await ctx.api.console.setBootDevice(
        { hostId: nodeId, device, persistent: persistent === true, efi: efi === true, reason }));
    })));
}

// The audit actions that end an uninstall: the host reported the service gone, or the service
// was assigned to the host again before it did.
const UNINSTALLED = "uninstalled";
const UNINSTALL_CANCELLED = "uninstall_cancelled";

// The services whose last uninstall on `nodeId` was cancelled, from the entries of Audit.List
// (newest last): a later `uninstalled` of the same service ends the mark.
function cancelledUninstalls(entries, nodeId) {
  const last = new Map();
  for (const entry of entries) {
    if (entry.nodeId === nodeId && typeof entry.service === "string"
      && (entry.action === UNINSTALLED || entry.action === UNINSTALL_CANCELLED)) {
      last.set(entry.service, entry.action);
    }
  }
  return new Set(Array.from(last).filter(([, action]) => action === UNINSTALL_CANCELLED).map(([s]) => s));
}

// The rows of the services Services.List assigns to `nodeId`, or orders it to uninstall.
// `cancelled` names the assigned services whose last uninstall was cancelled.
function assignedRows(entries, nodeId, cancelled) {
  const rows = [];
  for (const entry of entries) {
    const desired = typeof entry.desiredVersion === "string" ? entry.desiredVersion : null;
    const mine = list(entry, "assigned").find((a) => a.nodeId === nodeId);
    if (mine) {
      const direct = list(entry, "assignments").some((given) => given["host"] === nodeId && given.removed !== true);
      const installed = typeof mine.installedVersion === "string" ? mine.installedVersion : null;
      let state = "behind";
      if (desired === null) {
        state = "-";
      } else if (installed === null) {
        state = "not installed";
      } else if (installed === desired) {
        state = "at desired";
      }
      if (cancelled.has(entry.service)) {
        state = `${state}; uninstall cancelled`;
      }
      rows.push([entry.service, direct ? "this host" : `role ${mine.role}`, installed || "not installed",
        desired || "none set", state, mine.minVersion || "-"]);
    } else if (list(entry, "uninstalling").includes(nodeId)) {
      rows.push([entry.service, "-", "-", desired || "none set", "uninstalling", "-"]);
    }
  }
  return rows;
}

// Whether Services.List assigns any service to `nodeId`.
function assignsAny(entries, nodeId) {
  return entries.some((entry) => list(entry, "assigned").some((a) => a.nodeId === nodeId));
}

// The host's ended uninstalls, read only when a service is assigned to it (a cancelled uninstall
// is one whose service was assigned again): `{cancelled}`, a set of services, or `{cancelled,
// error}` when Audit.List failed, with none marked. Audit.List answers the oldest entries first,
// a page at a time, so the pages are read from the first, each from the `nextSeq` of the one
// before, until a page is empty or `nextSeq` does not move.
async function uninstallEnds(ctx, listing, nodeId) {
  if (listing.error || !assignsAny(listing.entries, nodeId)) {
    return { cancelled: new Set() };
  }
  const entries = [];
  try {
    let request = { nodeId };
    for (;;) {
      const page = await ctx.api.audit.list(request);
      const got = list(page, "entries");
      entries.push(...got);
      const next = typeof page.nextSeq === "string" ? page.nextSeq : "0";
      if (got.length === 0 || next === (request.sinceSeq || "0")) {
        break;
      }
      request = { nodeId, sinceSeq: next };
    }
  } catch (error) {
    return { cancelled: new Set(), error };
  }
  return { cancelled: cancelledUninstalls(entries, nodeId) };
}

// `listing` is `{entries}` of Services.List, or `{error}` when it failed; `ends` is what
// `uninstallEnds` read.
function assignedSection(listing, nodeId, ends) {
  if (listing.error) {
    return h("section", { class: "assigned" }, h("h3", {}, "Assigned services"),
      h("p", { class: "error", role: "alert" }, describeError(listing.error)));
  }
  const rows = assignedRows(listing.entries, nodeId, ends.cancelled);
  return h("section", { class: "assigned" }, h("h3", {}, "Assigned services"),
    rows.length === 0
      ? h("p", {}, "No service is assigned to this host.")
      : table(["Service", "Through", "Installed", "Desired", "State", "Minimum"],
        rows.map((cells) => h("tr", {}, cells.map((c) => h("td", {}, c))))),
    ends.error
      ? h("p", { class: "note" }, `Cancelled uninstalls could not be read: ${describeError(ends.error)}`)
      : null);
}

// The facts the host inventory gives of `host`: the record its enrollment matched, as a link to
// the Inventory page's enrolled records with that record marked, the record's MACs and serial,
// when the host's approval was made, and when the coordinator first heard the host since then
// ("not yet" for an enrolled host it has not heard since). Each is "-" when there is none.
function inventoryFacts(ctx, host) {
  const recordId = typeof host.inventoryRecordId === "string" ? host.inventoryRecordId : "";
  const record = recordId === "" ? "none matched" : h("button", {
    type: "button", class: "linklike", name: "open-record",
    on: { click: () => ctx.navigate("inventory", { enrolled: "true", record: recordId }) },
  }, recordId);
  const enrolled = fmtUnix(host.enrolledUnix) !== "-";
  const firstHeard = fmtUnix(host.firstHeartbeatUnix);
  return [
    ["Inventory record", record],
    ["Data MACs", list(host, "dataMacs").join(", ") || "-"],
    ["BMC MAC", host.bmcMac || "-"],
    ["Serial", host.serial || "-"],
    ["Enrolled", enrolled ? fmtUnix(host.enrolledUnix) : "not enrolled"],
    ["First heartbeat", firstHeard === "-" && enrolled ? "not yet" : firstHeard],
  ];
}

// When a reason began, in words: the time and whose clock it is, or that it was not reported.
function sinceWords(reason) {
  const when = fmtUnix(reason.since);
  return when === "-" ? "Since: not reported." : `Since ${when} (${reason.sinceIs}).`;
}

// What hostkeeper holds of a js-agent's taint: the hook's halt reason, what the agent said and the
// hook's last log lines. The hook tells hostkeeper no check and no run id, so they are said to be
// not reported rather than left out. No action of hostkeeper clears a taint yet: it is cleared on
// the host, and then the halt (or the failed update) is cleared here.
function taintFacts(reason) {
  const then = reason.fix === "clear-halt" ? "Clear halt" : "Retry update";
  return h("div", { class: "taint", name: "taint" },
    kv([
      ["Taint", "the agent is tainted (its update hook reports the reason tainted)"],
      ["Finding", reason.taint.said || "-"],
      ["Check", "not reported to hostkeeper"],
      ["Run id", "not reported to hostkeeper"],
    ]),
    reason.taint.log.length === 0 ? null : h("pre", { class: "log" }, reason.taint.log.map((line) => `${line}\n`)),
    h("p", { class: "note" },
      `Clear taint: hostkeeper has no action that clears a taint yet. Clear it on the host, then press ${then} here.`));
}

// The words under a reason no button fixes.
const NO_BUTTON = {
  unhealthy: "Nothing to press here: the service itself says it is unhealthy. Its Services tab has its last update and the actions on it.",
  prerequisite: "Nothing to press here: the host's daemon tries again by itself, at the next try the message names.",
  alert: "Nothing to press here: the coordinator's words above say what to do.",
};

// The one action that fixes `reason`: Clear halt (Releases.ClearHalt of its service, with the
// reason typed in `form`, after a confirm, since it lifts the service's halt on every host), Retry
// update (Hosts.Retry of the host and its service, after a confirm), or the words of why there is
// no button.
function fixOf(ctx, nodeId, reason, form) {
  const service = reason.service;
  if (reason.fix === "clear-halt") {
    return action(ctx, `Clear halt of ${service}`, `clear-halt-${service}`, async () => {
      const typed = trimmed(form.reason);
      if (typed === "") {
        return "Give a reason first.";
      }
      const problem = reasonProblem(typed);
      if (problem) {
        return problem;
      }
      const taint = reason.taint === null ? ""
        : ` The agent on ${nodeId} is still tainted: its update hook may halt again until the taint is cleared on the host.`;
      if (!ctx.confirm(`Clear the halt of ${service}? It lifts the halt on every host of ${service}, not only ${nodeId}; they are ordered again as its update plan and holds let them.${taint}`)) {
        return "Not cleared.";
      }
      await ctx.api.releases.clearHalt({ service, reason: typed });
      ctx.refresh();
      return `Cleared the halt of ${service}.`;
    }, { class: "primary" });
  }
  if (reason.fix === "retry") {
    return action(ctx, `Retry update of ${service}`, `retry-${service}`, async () => {
      if (!ctx.confirm(`Retry the update of ${service} on ${nodeId}? Its failed report is set aside, and it is ordered the desired version again when its update plan and holds let it.`)) {
        return "Not retried.";
      }
      const reply = await ctx.api.hosts.retry({ nodeId, service });
      ctx.refresh();
      const failedAt = reply.setAside && typeof reply.setAside["target"] === "string" ? ` to ${reply.setAside["target"]}` : "";
      return reply.entry
        ? `Set aside the failed update of ${service}${failedAt}: ${nodeId} may be ordered it again from its next heartbeat; its Services tab says what its gate waits for.`
        : `The failed update of ${service}${failedAt} was set aside already: its Services tab says what ${nodeId}'s gate waits for.`;
    }, { class: "primary" });
  }
  return h("p", { class: "note" }, NO_BUTTON[reason.kind]);
}

// Why the host is not healthy: each reason in plain words, when it began, what was said of it, a
// taint's facts, and its action; one Reason field (`form.reason`, kept across a change of tab)
// serves every Clear halt. Nothing when there is no reason and the alerts were read.
// `alertsFailure` is the error of Alerts.List, or null.
function healthSection(ctx, nodeId, reasons, alertsFailure, form) {
  if (reasons.length === 0 && alertsFailure === null) {
    return null;
  }
  return h("section", { class: "health card", name: "health" },
    h("h3", {}, reasons.length === 0 ? "Alerts" : `Why ${nodeId} is not healthy`),
    alertsFailure === null ? null : h("p", { class: "warning", role: "alert" },
      `The alerts were not read (${describeError(alertsFailure)}): only what the host itself reported is shown.`),
    reasons.some((reason) => reason.fix === "clear-halt")
      ? field("Reason (for Clear halt)", "health-reason", form, "reason", { maxlength: 256 })
      : null,
    reasons.length === 0 ? null : h("ul", { class: "reasons" }, reasons.map((reason) => h("li", {
      class: `reason ${reason.tone}`, name: "reason",
    },
    h("strong", {}, reason.title),
    reason.said === null ? null : h("p", { class: "said" }, reason.said),
    h("p", { class: "since" }, sinceWords(reason)),
    reason.taint === null ? null : taintFacts(reason),
    fixOf(ctx, nodeId, reason, form)))));
}

function chooser(ctx) {
  const form = { nodeId: "" };
  return h("section", { class: "page" },
    h("h2", {}, "Host"),
    h("p", {}, "Choose a host on Home, or name one."),
    field("Host id", "node-id", form, "nodeId"),
    action(ctx, "Open", "open", async () => {
      if (trimmed(form.nodeId) === "") {
        return "Name a host first.";
      }
      ctx.navigate("host", { nodeId: trimmed(form.nodeId) });
      return undefined;
    }));
}

export async function render(ctx) {
  const nodeId = ctx.params && typeof ctx.params.nodeId === "string" ? ctx.params.nodeId : "";
  if (nodeId === "") {
    return chooser(ctx);
  }
  let host;
  let failure = null;
  try {
    host = (await ctx.api.hosts.get({ nodeId })).host || {};
  } catch (error) {
    failure = error;
  }
  let listing;
  try {
    listing = { entries: list(await ctx.api.services.list({}), "services") };
  } catch (error) {
    listing = { error };
  }
  const ends = await uninstallEnds(ctx, listing, nodeId);
  if (failure !== null) {
    return h("section", { class: "page" },
      errorView(`Host ${nodeId}`, failure).children, assignedSection(listing, nodeId, ends),
      retireSection(ctx, nodeId));
  }
  let groups = null;
  let groupsFailure = null;
  try {
    groups = (await allPages((token) => ctx.api.hostGroups.list(token === "" ? {} : { pageToken: token }), "hostGroups"))
      .map((one) => ((one || {}).group || {}).name);
  } catch (error) {
    groupsFailure = error;
  }
  const ssh = await loadSsh(host, () => ctx.api.ssh.hostKeys({ hostId: nodeId }));
  const now = ctx.now();
  const agent = host.heartbeat && host.heartbeat.agent;
  const where = await placementSection(ctx, host, nodeId, () => repaint());
  let alerts = [];
  let alertsFailure = null;
  try {
    alerts = list(await ctx.api.alerts.list({}), "alerts");
  } catch (error) {
    alertsFailure = error;
  }
  const reasons = reasonsOf(host, alerts);
  const healthForm = { reason: "" };
  const labelsForm = {
    labels: Object.entries(host.labels || {}).map(([k, v]) => `${k}=${v}`).join(", "),
  };
  const [stateWord, stateTone] = statusOf(host, reasons);
  const services = list(host, "services");
  const certWord = certState(host.certNotAfterUnix, now);
  const rootWord = rootState(host.trustedRootReason);
  const servicesAttention = services.some((s) => s.health === "HEALTH_UNHEALTHY"
    || (s.lastUpdate && /FAIL/.test(String(s.lastUpdate.outcome))));
  const trustAttention = certWord === "expired" || certWord === "soon"
    || (typeof host.lastRenewError === "string" && host.lastRenewError !== "")
    || ["stale", "missing", "unparseable"].includes(rootWord);
  const group = typeof host.hostGroup === "string" && host.hostGroup !== "" ? host.hostGroup : "";
  const version = services.length > 0 && typeof services[0].installedVersion === "string"
    ? `${services[0].service} ${services[0].installedVersion}` : "";
  const labelCount = Object.keys(host.labels || {}).length;
  const panels = {
    overview: [
      kv([
        ["Role", host.role || "-"],
        ["Heartbeat", host["live"] === true ? "live" : "silent"],
        ["Last heartbeat", fmtUnix(host.lastHeartbeatUnix)],
        ["Agent", agent ? `${agent["name"] || "-"} ${agent.version || ""}` : "-"],
        ["Labels", labelsForm.labels || "-"],
        ["Excluded from", (host.exclusions || []).map((x) => `${x.service} (${x.reason || "-"})`).join(", ") || "-"],
        ...inventoryFacts(ctx, host),
      ], "cols card"),
      assignedSection(listing, nodeId, ends),
    ],
    services: services.length === 0
      ? [h("p", { class: "quiet" }, "The host reports no service.")]
      : services.map((view) => serviceSection(ctx, nodeId, view)),
    trust: [h("div", { class: "pair" }, certificate(host, now), trustedRoot(host))],
    labels: [h("fieldset", { class: "card" },
      h("legend", {}, "Labels"),
      h("div", { class: "chips" }, Object.entries(host.labels || {}).map(([k, v]) => h("span", { class: "chip" }, `${k}=${v}`))),
      field("Labels (key=value, separated by commas)", "labels", labelsForm, "labels"),
      action(ctx, "Set labels", "set-labels", async () => {
        const parsed = parseLabels(labelsForm.labels);
        if (parsed.problem) {
          return parsed.problem;
        }
        const { labels } = parsed;
        const had = Object.keys(host.labels || {}).length;
        if (Object.keys(labels).length === 0 && had > 0
          && !ctx.confirm(`Remove every label of ${nodeId}?`)) {
          return "Labels not changed.";
        }
        await ctx.api.hosts.setLabels({ nodeId, labels });
        ctx.refresh();
        return `Labels of ${nodeId} set.`;
      }, { class: "primary" }))],
    placement: [where.section],
    group: [hostGroupSection(ctx, nodeId, host, groups, groupsFailure)],
    ssh: sshPanel(ctx, nodeId, ssh, (fingerprints) => ctx.api.ssh.pinHostKeys({ hostId: nodeId, fingerprints })),
    power: [powerSection(ctx, nodeId)],
  };
  const tabs = [
    ["overview", "Overview", ""],
    ["services", "Services", servicesAttention ? "bad" : ""],
    ["trust", "Certificate", trustAttention ? "warn" : ""],
    ["labels", "Labels", ""],
    ["placement", "Placement", where.held ? "" : "muted"],
    ["group", "Host group", ""],
    ["ssh", "Remote SSH", ""],
    ["power", "Power and boot", ""],
  ];
  if (!tabs.some(([id]) => id === chosen.tab) || chosen.node !== nodeId) {
    chosen.node = nodeId;
    chosen.tab = "overview";
  }
  let root;
  const repaint = () => ctx.show(assemble());
  const assemble = () => {
    root = h("section", { class: "page hostpage" },
      where.crumbs,
      h("header", { class: "hostbar" },
        h("div", { class: "hb-main" },
          h("div", { class: "hb-title" },
            h("span", { class: `hb-led ${stateTone}`, "aria-hidden": "true" }),
            h("span", { class: "kicker" }, "Host"),
            h("h2", {}, host.nodeId || nodeId),
            pill(stateWord, stateTone)),
          h("div", { class: "hb-meta" },
            [["Role", host.role], ["Group", group || "Unassigned"], ["Runs", version],
              ["Last heartbeat", fmtUnix(host.lastHeartbeatUnix)],
              ["Labels", labelCount === 0 ? "" : `${labelCount}`]]
              .filter(([, v]) => typeof v === "string" && v !== "")
              .map(([k, v]) => h("span", { class: "meta" }, h("small", {}, k), v)))),
        where.mini),
      healthSection(ctx, nodeId, reasons, alertsFailure, healthForm),
      h("nav", { class: "tabs", "aria-label": "host sections" },
        tabs.map(([id, label, tone]) => h("button", {
          type: "button", name: `tab-${id}`, class: `tab${chosen.tab === id ? " on" : ""}`,
          "aria-pressed": String(chosen.tab === id),
          on: { click: () => { chosen.tab = id; repaint(); } },
        }, label, tone === "" ? null : h("span", { class: `dot ${tone}`, "aria-hidden": "true" })))),
      tabs.map(([id]) => h("div", { class: `panel${chosen.tab === id ? "" : " off"}` }, panels[id])),
      retireSection(ctx, nodeId));
    return root;
  };
  return assemble();
}

// The tab the page shows, kept across refreshes of the same host.
const chosen = { node: "", tab: "overview" };

// The rack the host stands in, drawn small: the rack's U as a column, the other sleds as dim blocks
// and the sled this host stands on lit, with its address and place in words under it; an empty
// dashed column and "Unplaced" for a host on no sled. 'held' is the host's placement, 'rackAt' its rack.
export function miniRack(layout, held, rackAt) {
  if (held === undefined || rackAt === undefined) {
    return h("div", { class: "mini" },
      h("div", { class: "mini-rack empty", "aria-hidden": "true", vars: { "--units": 12 } }),
      h("p", { class: "mini-cap" }, h("strong", {}, "Unplaced"), "on no sled"));
  }
  const block = (sled, me) => h("span", {
    class: me ? "mini-u me" : "mini-u", "aria-hidden": "true",
    vars: { "--r": rackAt.height - (sled.u + sled.height - 1) + 1, "--s": sled.height },
  });
  const others = layout.sleds.filter((x) => x.rack === rackAt.id && x.id !== held.sled);
  const mine = layout.sleds.find((x) => x.id === held.sled);
  return h("div", { class: "mini" },
    h("div", { class: "mini-rack", role: "img", "aria-label": `${rackName(layout, rackAt.id)}, U${held.u}`, vars: { "--units": rackAt.height } },
      others.map((x) => block(x, false)), mine === undefined ? null : block(mine, true)),
    h("p", { class: "mini-cap" },
      h("strong", {}, rackName(layout, rackAt.id)),
      `U${held.u}${held.height > 1 ? `-${held.u + held.height - 1}` : ""} of ${rackAt.height}`),
    h("p", { class: "mini-cap", name: "address" }, held.address));
}

// Where the host stands, and the controls that move it: one choice, the Sled slot, among the free
// slots of the lab by address (the slot it stands on included), then Place or Move, and Unplace. The
// place belongs to the physical server, which is the host's inventory entry when one matches it, else
// the host. The lab layout is read once; when it cannot be, the section says so and offers nothing.
// Every change is audited by the coordinator.
async function placementSection(ctx, host, nodeId, show) {
  let layout;
  try {
    layout = layoutOf(await ctx.api.layout.get({}));
  } catch (error) {
    return {
      crumbs: null,
      mini: null,
      held: false,
      section: h("fieldset", { class: "card" },
        h("legend", {}, "Placement"),
        h("p", { class: "warning", role: "alert" }, `The lab layout cannot be read: ${describeError(error)}`)),
    };
  }
  const entry = typeof host.inventoryRecordId === "string" ? host.inventoryRecordId : "";
  const server = entry === "" ? { kind: "host", id: nodeId } : { kind: "inventory", id: entry };
  const key = `${server.kind}:${server.id}`;
  const held = layout.placements.find((p) => p.key === key);
  const rackAt = held === undefined ? undefined : layout.racks.find((r) => r.id === held.rack);
  const options = slotOptions(layout, key, false);
  const state = { slot: held === undefined ? (options.length > 0 ? options[0][0] : "") : `${held.sled}:${held.slot}` };
  const standing = held === undefined
    ? "Unplaced: it stands on no sled."
    : `Stands at ${held.address} (${rackName(layout, held.rack)}, U${held.u}, a sled ${held.height}U high).`;
  const spot = rackAt === undefined ? {} : { rack: rackAt.id };
  return {
    crumbs: crumbs(layout, { ...spot, host: host.nodeId || nodeId }, (view, params) => ctx.navigate(view, params)),
    mini: miniRack(layout, held, rackAt),
    held: held !== undefined,
    section: h("fieldset", { class: "card" },
      h("legend", {}, "Placement"),
      h("p", { name: "standing" }, standing),
      options.length === 0
        ? h("p", { class: "note" }, "No sled slot is free: add a sled on Home, in Edit layout.")
        : select("Sled slot", "place-slot", state, "slot", options),
      h("div", { class: "buttons" }, action(ctx, held === undefined ? "Place" : "Move", "place", async () => {
        const chosen = slotOf(state.slot);
        if (chosen === null) {
          return "Choose a sled slot.";
        }
        try {
          await ctx.api.layout.place({
            subject: subjectOf(server), sled: chosen.sled, slot: chosen.slot,
            reason: `${held === undefined ? "placed" : "moved"} ${nodeId}`,
          });
        } catch (error) {
          return layoutProblem(error);
        }
        ctx.refresh();
        return `${nodeId} is at ${options.find(([value]) => value === state.slot)[1]}.`;
      }, { class: "primary" }),
      held === undefined ? null : action(ctx, "Unplace", "unplace", async () => {
        try {
          await ctx.api.layout.unplace({ subject: subjectOf(server), reason: `unplaced ${nodeId}` });
        } catch (error) {
          return layoutProblem(error);
        }
        ctx.refresh();
        return `${nodeId} is unplaced.`;
      }))),
  };
}

// Retire: the reason is optional; the host is refused from then on, so it asks first. The answer
// says whether an update in flight still holds the host's slot.
function retireSection(ctx, nodeId) {
  const form = { reason: "" };
  return h("fieldset", { class: "danger-zone" },
    h("legend", {}, "Danger zone"),
    h("div", { class: "dz-row" },
      h("div", { class: "dz-text" },
        h("strong", {}, "Retire this host"),
        h("p", { class: "note" },
          "Retire revokes the host: its heartbeats and renewals are refused, and its own service assignments are "
          + "dropped. Nothing is uninstalled.")),
      h("div", { class: "dz-form" },
        field("Reason (optional)", "retire-reason", form, "reason", { maxlength: 256 }),
        action(ctx, "Retire this host", "retire", async () => {
          const reason = trimmed(form.reason);
          const problem = reason === "" ? null : reasonProblem(reason);
          if (problem) {
            return problem;
          }
          if (!ctx.confirm(`Retire ${nodeId}? Its heartbeats and renewals are refused from then on.`)) {
            return "Not retired.";
          }
          const reply = await ctx.api.hosts.retire({ nodeId, reason });
          ctx.refresh();
          return reply.updateInFlight === true
            ? `Retired ${nodeId}: its update slot is freed when it reports its update final.`
            : `Retired ${nodeId}.`;
        }, { class: "danger" }))));
}
