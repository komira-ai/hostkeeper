// Full Disk Access: whether a Mac's grant was in force for its daemon, as its last gRPC heartbeat
// said (`fullDiskAccess` of Hosts.Get, the enum's name; absent when unspecified). The Host page's
// Overview shows it as one row, "Full Disk Access", for a host that reports a state, and a warning
// under the row while the grant is not in force. A host that is not a Mac has no such grant, so it
// shows no row, and so does a host whose heartbeat the coordinator does not hold (the platform is
// read from it); a Mac whose last heartbeat carries no state (a daemon that predates the field)
// shows "not reported". The functions here are pure: no call is made.

import { h } from "./view.js";

// The platform Hosts.Get answers for a Mac, in the host's last heartbeat.
const MAC_PLATFORM = "PLATFORM_DARWIN_ARM64";

// The warning while the grant is not in force.
const NOT_GRANTED = "Full Disk Access is not granted: the privacy profile of "
  + "docs/operations/mac-full-disk-access.md is not in force on this Mac. Anything that needs the "
  + "grant fails until it is.";

// The state of the grant whose enum name Hosts.Get answers as `value`: "granted", "not granted",
// "not determined", "not a Mac", or "not reported" for an absent, unspecified or unknown value.
export function accessState(value) {
  switch (value) {
    case "FULL_DISK_ACCESS_GRANTED":
      return "granted";
    case "FULL_DISK_ACCESS_NOT_GRANTED":
      return "not granted";
    case "FULL_DISK_ACCESS_UNDETERMINED":
      return "not determined";
    case "FULL_DISK_ACCESS_NOT_APPLICABLE":
      return "not a Mac";
    default:
      return "not reported";
  }
}

// The state `host` shows, or null for no row: none for a host that is not a Mac, and for a host
// that reports nothing and is not known to be a Mac.
function shown(host) {
  const state = accessState(host.fullDiskAccess);
  if (state === "not a Mac") {
    return null;
  }
  const platform = host.heartbeat && host.heartbeat.platform;
  return state === "not reported" && platform !== MAC_PLATFORM ? null : state;
}

// The Overview row of `host`: `[["Full Disk Access", state]]`, or no row.
export function accessRows(host) {
  const state = shown(host);
  return state === null ? [] : [["Full Disk Access", state]];
}

// The warning under the row: one alert line while the grant is not in force, else nothing.
export function accessWarning(host) {
  return shown(host) === "not granted" ? [h("p", { class: "warning", role: "alert" }, NOT_GRANTED)] : [];
}
