// What the operator reads when a console stream closes: the coordinator closes it with a code and
// a reason token (docs/design/console-broker.md, section 12; `crates/hostkeeper-console/src/close.rs`),
// and the page shows the sentence for the token. A token this page has no words for is shown as it
// came, with its code; a close with no token (the connection dropped, 1006) is told by whether the
// stream had opened.

// The sentence for each reason token the coordinator closes a stream with.
export const CLOSES = {
  device_unreachable: "The device did not answer at its address, or its connection ended.",
  bmc_sol_busy: "Someone else holds the BMC's Serial over LAN, or took it from this session.",
  bmc_busy: "The BMC has no free session. Try again in a minute.",
  bmc_auth_failed: "The BMC refused the coordinator's BMC credential.",
  idle: "Nothing crossed the console for the idle limit, so the coordinator closed it.",
  total_limit: "The console was open for the longest time the coordinator allows.",
  closed_by_operator: "An operator closed this session.",
  token_revoked: "Your access to consoles ended (your account or your console access was removed), so the session was closed.",
  address_mismatch: "The BMC at the host's address is not the host's BMC, so nothing was sent to it.",
  coordinator_stopping: "The coordinator is stopping. Open the console again once it is back.",
  mac_screen_sharing_off: "Screen Sharing is off on the Mac: it refused the connection.",
  mac_unreachable_or_asleep: "The Mac did not answer: it is asleep, off, or not at its address.",
  bmc_failed: "The coordinator could not run the BMC tool or read its answer.",
  device_closed: "The device closed the connection.",
  protocol_error: "The coordinator closed the console: this page broke the stream's protocol.",
  message_too_big: "The coordinator closed the console: this page sent a message over 64 KiB.",
};

// The sentence for a stream that closed with `code` and `reason` (the close frame's, a number and a
// string); `opened` is whether the stream had opened.
export function closeWords(code, reason, opened) {
  const token = typeof reason === "string" ? reason : "";
  if (Object.prototype.hasOwnProperty.call(CLOSES, token)) {
    return CLOSES[token];
  }
  if (token !== "") {
    return `The console closed (${code} ${token}).`;
  }
  return opened
    ? "The connection to the coordinator closed without saying why."
    : "The coordinator did not accept the connection. Your sign-in may have ended: sign in again and retry.";
}
