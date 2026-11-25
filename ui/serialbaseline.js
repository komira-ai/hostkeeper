// Serial console baseline: whether the daemon keeps the host's serial console set up, as its last
// gRPC heartbeat said (`heartbeat.serialConsole` of Hosts.Get: `port`, `state`, `detail`). The Host
// page's Overview shows it as one row, "Serial console", for a host that reports it, and a warning
// while the console moves at the next reboot or the baseline failed. A host whose heartbeat carries
// no report shows nothing. The functions here are pure: no call is made.

import { h } from "./view.js";

// The report of `host`, or null when its last heartbeat carries none.
function report(host) {
  const found = host.heartbeat && host.heartbeat.serialConsole;
  return found ? found : null;
}

// The words of a report: "<port>, applied", "<port>, reboot pending", "<port>, failed (<detail>)",
// "<port>, unsupported (<detail>)", and "<port>, <state>" for any other state.
function words(found) {
  const port = found.port || "";
  const state = found.state || "";
  const detail = found.detail || "";
  switch (state) {
    case "applied":
      return `${port}, applied`;
    case "reboot_pending":
      return `${port}, reboot pending`;
    case "failed":
    case "unsupported":
      return detail ? `${port}, ${state} (${detail})` : `${port}, ${state}`;
    default:
      return `${port}, ${state}`;
  }
}

// The Overview row of `host`: `[["Serial console", words]]`, or no row.
export function baselineRows(host) {
  const found = report(host);
  return found === null ? [] : [["Serial console", words(found)]];
}

// The warning under the row: one alert line for a reboot still to come or a failed baseline, else
// nothing.
export function baselineNotice(host) {
  const found = report(host);
  if (found === null) {
    return [];
  }
  if (found.state === "reboot_pending") {
    return [h("p", { class: "warning", role: "alert" },
      `The serial console is set up on ${found.port}; the kernel's console moves to it at the host's `
      + "next reboot. hostkeeper does not reboot the host.")];
  }
  if (found.state === "failed") {
    return [h("p", { class: "warning", role: "alert" },
      `The serial console baseline on ${found.port} failed at ${found.detail || "an unknown step"}.`)];
  }
  return [];
}
