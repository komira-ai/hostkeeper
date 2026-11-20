// What is due on a host, in words, from the maintenance report Hosts.Get and Hosts.List answer in
// `Host.maintenance` (docs/design/maintenance-updates.md, sections 1 and 4.2), with the age of the
// report's last look (`maintenanceLookAgeSecs`) and whether the coordinator calls it stale
// (`maintenanceStale`). The host page's header and its Updates tab, and the Maintenance page, show
// what this module says; it calls nothing and changes nothing.
//
// The due items become short phrases, in this order:
// - a Linux kernel update (a `linux` item that needs a reboot), or a `reboot` item whose label is
//   `kernel`: "kernel update, restart needed", once however many items say it;
// - any other `reboot` item, when there is no kernel phrase: "restart needed", once;
// - the other `linux` items from a security origin: "12 security updates" ("1 security update");
// - the other `linux` items: "3 other updates" ("1 other update");
// - each `macos` item: "macOS 26.1 to 26.2, restart" (", restart" when it needs one);
// - each `xcode` item: "Xcode 27.2 available";
// - an item of a kind this module does not know: "update <label>";
// - the items the report left out (`dueLeftOut`): "and 4 more".

import { h, enumLabel, fmtUnix, list, table } from "./view.js";

const KIND = "MAINTENANCE_KIND_";

// The text of a report's field, or "" when it is absent or not a text.
function said(value) {
  return typeof value === "string" ? value : "";
}

// A count and its noun, the noun made plural for any count but one: `count(3, "update")`.
function counted(count, noun) {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

// The kernel phrase is said for a `linux` item that needs a reboot, and for the `reboot` reason
// `kernel` (a newer kernel installed and not running).
function isKernel(item) {
  return (item.kind === `${KIND}LINUX` && item.needsReboot === true)
    || (item.kind === `${KIND}REBOOT` && said(item.label) === "kernel");
}

// The phrase of one `macos` item: "macOS 26.1 to 26.2, restart". Without a `from`, "macOS 26.2";
// without a `to`, the label stands for it, and without either, "update".
function macosPhrase(item) {
  const to = said(item["to"]) || said(item.label) || "update";
  const from = said(item.from);
  const what = from === "" ? `macOS ${to}` : `macOS ${from} to ${to}`;
  return item.needsReboot === true ? `${what}, restart` : what;
}

// The phrases of what `report` (a `MaintenanceReport`, or absent) lists as due, in the order the
// module's header gives; empty when it lists nothing or there is no report.
export function duePhrases(report) {
  const due = list(report || {}, "due");
  const out = [];
  if (due.some(isKernel)) {
    out.push("kernel update, restart needed");
  } else if (due.some((item) => item.kind === `${KIND}REBOOT`)) {
    out.push("restart needed");
  }
  const linux = due.filter((item) => item.kind === `${KIND}LINUX` && !isKernel(item));
  const security = linux.filter((item) => item["security"] === true).length;
  if (security > 0) {
    out.push(counted(security, "security update"));
  }
  if (linux.length > security) {
    out.push(counted(linux.length - security, "other update"));
  }
  for (const item of due) {
    if (item.kind === `${KIND}MACOS`) {
      out.push(macosPhrase(item));
    } else if (item.kind === `${KIND}XCODE`) {
      out.push(`Xcode ${said(item["to"]) || said(item.label) || "update"} available`);
    } else if (![`${KIND}LINUX`, `${KIND}REBOOT`].includes(item.kind)) {
      out.push(`update ${said(item.label) || "of a kind this page does not know"}`);
    }
  }
  const left = Number((report || {}).dueLeftOut || 0);
  if (left > 0) {
    out.push(`and ${left} more`);
  }
  return out;
}

// Whether `host` (a `Host` of Hosts.Get or Hosts.List) has a report that lists something due.
export function isDue(host) {
  return duePhrases(host.maintenance).length > 0;
}

// Whether the coordinator calls the last look of `host` stale: then an empty due list does not mean
// nothing is due.
export function isStale(host) {
  return host.maintenance !== undefined && host.maintenanceStale === true;
}

// An age in seconds as the page says it: "40 s ago", "12 min ago", "3 h ago", "2 days ago", each
// rounded down;
// "not looked yet" when absent (the daemon has not looked, or there is no report).
export function ageWords(secs) {
  const age = Number(secs);
  if (secs === undefined || secs === null || secs === "" || !Number.isFinite(age) || age < 0) {
    return "not looked yet";
  }
  if (age < 60) {
    return `${age} s ago`;
  }
  if (age < 3600) {
    return `${Math.floor(age / 60)} min ago`;
  }
  if (age < 86400) {
    return `${Math.floor(age / 3600)} h ago`;
  }
  return `${counted(Math.floor(age / 86400), "day")} ago`;
}

// The age of the last look of `host`. The coordinator answers an age of 0 for a look ahead of its
// clock, and calls one more than 300 seconds ahead stale: a stale look of age 0 is said to be
// ahead of the clock, not "0 s ago".
function lookAge(host) {
  const age = host.maintenanceLookAgeSecs;
  return isStale(host) && Number(age) === 0 ? "ahead of the coordinator's clock" : ageWords(age);
}

// The last look of `host` in short: its age, and ", stale" when the coordinator calls it so.
export function lookWords(host) {
  if (host.maintenance === undefined) {
    return "not reported";
  }
  return `${lookAge(host)}${isStale(host) ? ", stale" : ""}`;
}

// The notice of the host page's header: a pill per phrase of what is due, and a stale pill when the
// look is stale; nothing for a host with nothing due and a fresh look, or with no report.
export function dueNotice(host) {
  const phrases = duePhrases(host.maintenance);
  const stale = isStale(host);
  if (phrases.length === 0 && !stale) {
    return null;
  }
  return h("div", { class: "hb-due", name: "due" },
    h("small", {}, "Due"),
    phrases.length === 0 ? h("span", { class: "quiet" }, "nothing listed") : null,
    phrases.map((phrase) => h("span", { class: "pill warn", name: "due-item" }, phrase)),
    stale ? h("span", { class: "pill muted", name: "due-stale" }, `stale look (${lookAge(host)})`) : null);
}

// Bytes as GiB with one decimal, "-" when absent.
function gib(value) {
  const bytes = Number(value);
  if (value === undefined || value === "" || !Number.isFinite(bytes) || bytes < 0) {
    return "-";
  }
  return `${(bytes / 2 ** 30).toFixed(1)} GiB`;
}

function yes(value) {
  return value === true ? "yes" : "no";
}

// A row of a table: one cell per value.
function row(cells) {
  return h("tr", {}, cells.map((value) => h("td", {}, value)));
}

// Rows of facts: a name and its value.
function facts(rows, name) {
  return h("div", { class: "table-wrap card", name }, h("table", { class: "kv" },
    h("tbody", {}, rows.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v))))));
}

// What the host runs and when its daemon last looked.
function hostFacts(host, report) {
  const at = report["host"] || {};
  const os = [said(at.osName), said(at.osVersion)].filter((part) => part !== "").join(" ");
  const build = said(at.osBuild);
  const rows = [
    ["Operating system", os === "" ? "-" : `${os}${build === "" ? "" : ` (${build})`}`],
    ["Running kernel", said(at.runningKernel) || "-"],
  ];
  if (said(at.newestKernel) !== "") {
    rows.push(["Newest kernel installed", at.newestKernel]);
  }
  if (said(at.hardwareSerial) !== "") {
    rows.push(["Hardware serial", at.hardwareSerial]);
  }
  rows.push(
    ["Booted", fmtUnix(at.bootUnix)],
    ["Last look", `${fmtUnix(report.lookedUnix)} (${lookWords(host)})`],
    ["Listing lines no parser knew", String(report.unparsedLines || 0)],
  );
  return facts(rows, "update-facts");
}

// What a macOS update needs of the Mac.
function macFacts(mac) {
  const account = mac.updateAccount || {};
  const tools = mac.commandLineTools || {};
  return facts([
    ["FileVault", enumLabel(mac.filevault, "FILE_VAULT_STATE_")],
    ["Restart without unlock (authrestart)", yes(mac.authrestartSupported)],
    ["Free on the data volume", gib(mac.dataFreeBytes)],
    ["Update account", account.present !== true ? "absent"
      : `present, administrator: ${yes(account.admin)}, Secure Token: ${yes(account.secureToken)}`],
    ["Command Line Tools", `receipt: ${yes(tools.receipt)}, files: ${yes(tools.files)}, version: ${said(tools.version) || "-"}`],
    ["Device-management conflicts", list(mac, "mdmConflicts").join(", ") || "none"],
  ], "mac-facts");
}

// Each due item, as the report lists it.
function dueTable(host, report) {
  const due = list(report, "due");
  if (due.length === 0) {
    return h("p", { class: isStale(host) ? "warning" : "quiet", name: "nothing-due" }, isStale(host)
      ? "The last look listed nothing, but it is stale: something may be due that it has not seen."
      : "Nothing is due.");
  }
  const left = Number(report.dueLeftOut || 0);
  return [
    table(["Kind", "From", "To", "Restart", "Security", "Label"], due.map((item) => row([
      enumLabel(item.kind, KIND), said(item.from) || "-", said(item["to"]) || "-", yes(item.needsReboot),
      yes(item["security"]), said(item.label) || "-",
    ]))),
    left > 0 ? h("p", { class: "note" }, `${counted(left, "more item")} due that the report leaves out.`) : null,
  ];
}

// The Xcodes installed, and which one `xcode-select` names.
function xcodeTable(report) {
  const xcodes = list(report, "xcodes");
  if (xcodes.length === 0) {
    return h("p", { class: "quiet" }, "No Xcode is installed in /Applications.");
  }
  return table(["Version", "Build", "Path", "Selected"], xcodes.map((x) => row([
    said(x.version) || "-", said(x["build"]) || "-", said(x.path) || "-", yes(x.selected),
  ])));
}

// The host page's Updates tab: what the host runs, each due item, the Xcodes of a Mac, and the last
// look and how old it is. Read only.
export function updatesPanel(host) {
  const report = host.maintenance;
  if (report === undefined) {
    return [h("p", { class: "quiet", name: "no-report" },
      "The host's daemon reports no maintenance: it does not announce maintenance-v1, or the coordinator has read no report of it yet.")];
  }
  const mac = report.mac;
  return [
    hostFacts(host, report),
    h("h3", { class: "sub" }, "Due"),
    dueTable(host, report),
    mac === undefined ? null : [h("h3", { class: "sub" }, "Mac"), macFacts(mac),
      h("h3", { class: "sub" }, "Xcode"), xcodeTable(report)],
  ];
}
