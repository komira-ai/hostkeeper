// MDM setup: the status panel and the push certificate step. Calls: Mdm.Status and Mdm.ListSettings
// (the thresholds of the reminders) on every drawing; on a button, Mdm.CreatePushRequest,
// Mdm.PutPushReply and Mdm.PutPushCertificate (a certificate for the pending request, or, with a key,
// an existing certificate and key).
//
// The status panel is what the coordinator computes from its records: *configured*, or what is
// missing, in words; the name; and, for the push certificate and key, the ABM token and the TLS
// certificate and key, what is held, when it expires and the days left, coloured once the days
// left cross a threshold of the setting that names it (`push_alert_days`, `abm_alert_days`,
// `tls_renew_days`). The page has no logic of its own beyond that: the coordinator checks every
// upload and its refusal is shown exactly as it answers (`CODE reason`).
//
// The push step has three buttons in order. *Create request* makes the CSR (the email goes into
// its subject) and, when asked, posts it to mdmcert.download, which emails its signed reply to that
// address; the CSR and the encryption certificate are public, shown and offered as downloads.
// *Check reply* takes the emailed attachment (a file, or its hex text pasted), checks it is the
// signed request for the pending CSR and offers it as a download, to upload to Apple's push
// portal. *Store certificate* takes the certificate Apple returns. The migration takes an existing
// certificate and its key (PKCS#1 or PKCS#8) in one step.
//
// Uploads are read when the button is pressed, sent and dropped: a file is kept only as the file
// the browser handed over, never its contents, and a key is never typed or pasted. The page shows
// no value of a secret, only what the coordinator says of it (kind, version, fingerprint, expiry).

import { toBase64, utf8 } from "../bytes.js";
import {
  h, action, area, checkbox, errorView, field, filePicker, fmtUnix, table, trimmed,
} from "../view.js";

const TITLE = "MDM setup";
const DAY = 86400;
// The limits of operator.proto on the three uploads, in bytes.
const REPLY_LIMIT = 32768;
const PEM_LIMIT = 16384;
const THRESHOLD_SETTINGS = ["push_alert_days", "abm_alert_days", "tls_renew_days"];

// What each condition of `Mdm.Status`'s `missing` says, by the token (the value without its
// prefix, in lower case).
const CONDITIONS = {
  push_cert_missing: "No push certificate is held.",
  push_key_missing: "No push key is held.",
  push_pair_invalid: "The push certificate or key cannot be read, or the key is not the certificate's.",
  push_cert_expired: "The push certificate has expired.",
  push_cert_not_yet_valid: "The push certificate is not valid yet.",
  abm_token_missing: "No ABM token is held.",
  abm_token_unreadable: "The ABM token cannot be read, or its expiry is not a time.",
  abm_token_expired: "The ABM token has expired.",
  name_missing: "No MDM name is set.",
  tls_cert_missing: "No TLS certificate is held.",
  tls_key_missing: "No TLS key is held.",
  tls_pair_invalid: "The TLS certificate or key cannot be read, or the key is not the certificate's.",
  tls_cert_expired: "The TLS certificate has expired.",
  tls_cert_not_yet_valid: "The TLS certificate is not valid yet.",
  tls_name_mismatch: "The TLS certificate does not name the MDM name.",
};

// The sentence of one value of `missing`; a value this page does not know is shown as its token.
export function conditionWords(value) {
  const token = String(value).replace(/^MDM_CONDITION_/, "").toLowerCase();
  return CONDITIONS[token] || token;
}

// The days of a threshold setting (`60,14`, or `30`) as a list, largest first; null when the
// value is not a list of whole numbers.
export function thresholdDays(value) {
  if (typeof value !== "string" || !/^\d+(,\d+)*$/.test(value.trim())) {
    return null;
  }
  return value.trim().split(",").map(Number).sort((a, b) => b - a);
}

// How near `notAfterUnix` is at `now`, against `days` (largest first, or null for no threshold):
// `{level, words}`. A level is "none" (no expiry known), "ok", "soon" (inside the largest
// threshold), "near" (inside the smallest, when there are two or more), or "expired".
export function expiry(notAfterUnix, now, days) {
  const end = Number(notAfterUnix);
  if (notAfterUnix === undefined || notAfterUnix === "" || !Number.isFinite(end) || end <= 0) {
    return { level: "none", words: "-" };
  }
  if (end <= now) {
    return { level: "expired", words: "expired" };
  }
  const left = Math.floor((end - now) / DAY);
  const words = left === 0 ? "less than a day left" : `${left} day${left === 1 ? "" : "s"} left`;
  if (days === null || days.length === 0 || left > days[0]) {
    return { level: "ok", words };
  }
  return { level: days.length > 1 && left <= days[days.length - 1] ? "near" : "soon", words };
}

// The `value` of the setting `name` in a `ListSettings` reply, or undefined.
function settingValue(reply, name) {
  const found = ((reply && reply.settings) || []).find((s) => s["name"] === name);
  return found === undefined ? undefined : found.value;
}

// The thresholds of the three settings, each a list of days or null.
async function thresholds(ctx) {
  let reply;
  try {
    reply = await ctx.api.mdm.listSettings({});
  } catch {
    return null;
  }
  const out = {};
  for (const name of THRESHOLD_SETTINGS) {
    out[name] = thresholdDays(settingValue(reply, name));
  }
  return out;
}

// What `ListSecrets` shows of the version in use, in a line.
function heldWords(info) {
  if (!info) {
    return "not held";
  }
  return `version ${info.version || 1}, ${String(info.fingerprintSha256 || "").slice(0, 16)}`;
}

// The status panel's table rows: `[label, held, expires, left, level]`.
export function panelRows(status, now, days) {
  const push = expiry(status.pushNotAfterUnix, now, days === null ? null : days.push_alert_days);
  const abm = expiry(status.abmNotAfterUnix, now, days === null ? null : days.abm_alert_days);
  const tls = expiry(status.tlsNotAfterUnix, now, days === null ? null : days.tls_renew_days);
  const row = (label, info, unix, when) => [label, heldWords(info), fmtUnix(unix), when.words, when.level];
  const none = { level: "none", words: "-" };
  return [
    row("Push certificate", status.pushCert, status.pushNotAfterUnix, push),
    row("Push key", status.pushKey, undefined, none),
    row("ABM token", status.abmToken, status.abmNotAfterUnix, abm),
    row("TLS certificate", status.tlsCert, status.tlsNotAfterUnix, tls),
    row("TLS key", status.tlsKey, undefined, none),
  ];
}

function levelClass(level) {
  return level === "ok" || level === "none" ? "" : "warning";
}

function statusPanel(status, now, days) {
  const missing = status.missing || [];
  const rows = panelRows(status, now, days);
  const due = rows.filter((r) => r[4] !== "ok" && r[4] !== "none");
  return h("section", { class: "service", name: "mdm-status" },
    h("h3", {}, "Status"),
    status.configured === true
      ? h("p", { name: "configured" }, "Configured.")
      : h("div", { name: "configured" },
        h("p", { class: "warning" }, "Not configured, because:"),
        h("ul", {}, missing.map((value) => h("li", {}, conditionWords(value))))),
    status.sealing === true
      ? null
      : h("p", { class: "error", role: "alert" },
        "The coordinator holds no master key (--mdm-secrets-key-file), so it keeps no secret: an upload is refused."),
    due.map(([label, , , when, level]) => h("p", { class: "warning", role: "alert" },
      level === "expired" ? `${label} has expired.` : `${label} expires soon: ${when}.`)),
    days === null
      ? h("p", { class: "note" }, "The thresholds of the reminders could not be read; only an expiry is coloured.")
      : null,
    table(["", "In use", "Expires", "Left"], rows.map(([label, held, until, when, level]) =>
      h("tr", { class: levelClass(level) },
        h("th", {}, label), h("td", {}, held), h("td", {}, until), h("td", {}, when)))),
    table(["", ""], [
      ["Push topic", status.pushTopic || "-"],
      ["MDM name", status["name"] || "-"],
      ["Enrollment URL", status.enrollmentUrl || "-"],
      ["TLS names", (status.tlsNames || []).join(", ") || "-"],
    ].map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))));
}

// The bytes the operator gave for one upload: the chosen file, else the text typed or pasted,
// else a sentence saying what is missing or too long. `limit` is the API's limit in bytes. A
// file's size is read before its contents, so a huge file is not loaded.
async function bytesOf(file, pasted, what, limit) {
  if (file) {
    if (typeof file.size === "number" && file.size > limit) {
      return `${what} is at most ${limit} bytes; this file is ${file.size}.`;
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    return bytes.length > limit ? `${what} is at most ${limit} bytes; this file is ${bytes.length}.` : bytes;
  }
  const text = typeof pasted === "string" ? pasted : "";
  if (trimmed(text) === "") {
    return `Choose ${what.toLowerCase()} or paste its text.`;
  }
  const bytes = utf8(text);
  return bytes.length > limit ? `${what} is at most ${limit} bytes; this text is ${bytes.length}.` : bytes;
}

// Draws the page again from the coordinator, keeping `flow`.
async function again(ctx, flow) {
  ctx.show(await view(ctx, flow));
}

function createSection(ctx, flow) {
  const request = flow.request;
  return h("fieldset", {},
    h("legend", {}, "Create the request"),
    h("p", { class: "note" },
      "Makes the key and the request (CSR) for a push certificate. Sent to mdmcert.download, which "
      + "emails the signed request to the address given; not sent, the CSR is for a vendor certificate "
      + "or a signer of your own. A request made before and not yet used is replaced."),
    field("Email address", "push-email", flow, "email", { maxlength: 254 }),
    checkbox("Send the request to mdmcert.download", "push-submit", flow, "submit"),
    action(ctx, "Create request", "create-request", async () => {
      const email = trimmed(flow["email"]);
      if (email === "") {
        return "Give the email address the request is made for.";
      }
      const reply = await ctx.api.mdm.createPushRequest({ email, submit: flow.submit === true });
      flow.request = {
        email,
        csrPem: reply.csrPem || "",
        encryptionCertificatePem: reply.encryptionCertificatePem || "",
        submitted: reply.submitted === true,
      };
      flow.signed = null;
      await again(ctx, flow);
      return reply.submitted === true
        ? `Request made and sent to mdmcert.download: it emails the signed request to ${email}.`
        : "Request made. Nothing was sent.";
    }),
    request === null ? null : requestResult(ctx, request));
}

function requestResult(ctx, request) {
  return h("div", { name: "push-request" },
    request.submitted
      ? h("p", {}, `The signed request comes by email from mdmcert.download to ${request["email"]}, `
        + "as an attachment. Upload that attachment under Check the emailed reply.")
      : h("p", {}, "The request was not sent. Take the CSR to the signer you use."),
    h("p", { class: "note" }, "The CSR and the encryption certificate are public."),
    h("pre", { class: "log", name: "csr" }, request.csrPem),
    h("div", { class: "buttons" },
      h("button", {
        type: "button",
        name: "download-csr",
        on: { click: () => ctx.download({ name: "push-request.csr", mime: "text/plain", text: request.csrPem }) },
      }, "Download the CSR"),
      h("button", {
        type: "button",
        name: "download-encryption-certificate",
        on: {
          click: () => ctx.download({
            name: "push-encryption.crt", mime: "text/plain", text: request.encryptionCertificatePem,
          }),
        },
      }, "Download the encryption certificate")));
}

function replySection(ctx, flow) {
  return h("fieldset", {},
    h("legend", {}, "Check the emailed reply"),
    h("p", { class: "note" },
      "Choose the attachment of the email from mdmcert.download, or paste its text (hex). The "
      + "coordinator decrypts it and checks that it is the signed request for the pending CSR; "
      + "nothing is stored."),
    filePicker("Attachment", "reply-file", flow, "replyFile"),
    area("Or its text", "reply-text", flow, "replyText"),
    action(ctx, "Check reply", "check-reply", async () => {
      const bytes = await bytesOf(flow.replyFile, flow.replyText, "The reply", REPLY_LIMIT);
      if (typeof bytes === "string") {
        return bytes;
      }
      const reply = await ctx.api.mdm.putPushReply({ reply: toBase64(bytes) });
      flow.signed = reply.signedRequest || "";
      flow.replyFile = null;
      flow.replyText = "";
      await again(ctx, flow);
      return "The reply is the signed request for the pending CSR. Download it and upload it to Apple's push portal.";
    }),
    flow.signed === null
      ? null
      : h("div", { name: "push-signed" },
        h("p", {}, "Upload the signed request at identity.apple.com/pushcert, sign in with the Apple ID "
          + "that owns the certificate, and download the certificate Apple returns. Store it under Store the certificate Apple returned."),
        h("button", {
          type: "button",
          name: "download-signed",
          on: {
            click: () => ctx.download({
              name: "push-signed-request.plist", mime: "application/x-plist", base64: flow.signed,
            }),
          },
        }, "Download the signed request")));
}

// The sentence for a stored certificate.
function storedWords(reply) {
  return `Stored the push certificate for ${reply.topic || "-"}, valid until ${fmtUnix(reply.notAfterUnix)}`
    + (reply.renewal === true ? " (it replaces the one in use)." : ".");
}

function certificateSection(ctx, flow) {
  return h("fieldset", {},
    h("legend", {}, "Store the certificate Apple returned"),
    h("p", { class: "note" },
      "It must be the certificate for the pending request's key, unexpired, with a topic that starts "
      + "com.apple.mgmt.External. (and, for a renewal, the topic in use)."),
    filePicker("Certificate file", "certificate-file", flow, "certFile"),
    area("Or the certificate, pasted", "certificate-text", flow, "certText"),
    action(ctx, "Store certificate", "put-certificate", async () => {
      const bytes = await bytesOf(flow.certFile, flow.certText, "The certificate", PEM_LIMIT);
      if (typeof bytes === "string") {
        return bytes;
      }
      const reply = await ctx.api.mdm.putPushCertificate({ certificatePem: toBase64(bytes) });
      flow.certFile = null;
      flow.certText = "";
      await again(ctx, flow);
      return storedWords(reply);
    }));
}

function migrationSection(ctx, flow) {
  return h("fieldset", {},
    h("legend", {}, "Or: upload an existing certificate and key"),
    h("p", { class: "note" },
      "For the certificate and key already in use: the key may be PKCS#1 (BEGIN RSA PRIVATE KEY) or "
      + "PKCS#8. The key is read from its file when the button is pressed, sent and dropped; it is "
      + "never shown."),
    filePicker("Certificate file", "existing-certificate-file", flow, "existingCertFile"),
    area("Or the certificate, pasted", "existing-certificate-text", flow, "existingCertText"),
    filePicker("Key file", "existing-key-file", flow, "existingKeyFile"),
    action(ctx, "Store certificate and key", "put-existing", async () => {
      const certificate = await bytesOf(flow.existingCertFile, flow.existingCertText, "The certificate", PEM_LIMIT);
      if (typeof certificate === "string") {
        return certificate;
      }
      if (!flow.existingKeyFile) {
        return "Choose the key file.";
      }
      const key = await bytesOf(flow.existingKeyFile, "", "The key", PEM_LIMIT);
      if (typeof key === "string") {
        return key;
      }
      const reply = await ctx.api.mdm.putPushCertificate({
        certificatePem: toBase64(certificate),
        keyPem: toBase64(key),
      });
      flow.existingKeyFile = null;
      flow.existingCertFile = null;
      flow.existingCertText = "";
      await again(ctx, flow);
      return storedWords(reply);
    }));
}

async function view(ctx, flow) {
  let status;
  try {
    status = await ctx.api.mdm.status({});
  } catch (error) {
    return errorView(TITLE, error);
  }
  const days = await thresholds(ctx);
  return h("section", { class: "page" },
    h("h2", {}, TITLE),
    h("p", { class: "note" },
      "The push certificate is how the coordinator wakes the Macs. Every step is audited under your name."),
    statusPanel(status, ctx.now(), days),
    h("h3", {}, "Push certificate"),
    createSection(ctx, flow),
    replySection(ctx, flow),
    certificateSection(ctx, flow),
    migrationSection(ctx, flow));
}

export async function render(ctx) {
  return view(ctx, {
    email: "",
    submit: true,
    request: null,
    signed: null,
    replyFile: null,
    replyText: "",
    certFile: null,
    certText: "",
    existingCertFile: null,
    existingCertText: "",
    existingKeyFile: null,
  });
}
