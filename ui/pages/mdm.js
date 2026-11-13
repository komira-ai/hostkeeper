// MDM setup: one plain form and nothing else but a one-line status above it and a Verify button
// below it. Calls: Mdm.Status, Mdm.ListSettings and Mdm.ListSecrets on every drawing; on Save,
// PutPushCertificate, PutSecret (the ABM key and certificate, the DNS credential), PutAbmToken and
// PutSetting (`dns_zone`, `acme_contact`), and PutName while no name is set; on the link next to the token field, GetAbmPublicKey; on
// Verify, Verify.
//
// The status line is what the coordinator computes from its records: "MDM: configured", or what is
// missing, and the MDM name, read-only. Only while no name is set, one more field takes it (the
// name cannot be had from anywhere else); once it is set, it is not on the form. Each field is one labelled
// input with a one-line hint of where its file comes from and a link to the documentation that
// says how to get it. A part the coordinator holds is shown as "stored" with its expiry beside its
// field, never its value.
//
// Save sends whatever fields are filled, in this order: the MDM name (only while none is set), the
// push certificate with its key, the ABM
// key and certificate, the ABM token (a `.p7m` or JSON, told by its content), the GoDaddy
// credential (and the DNS zone, derived from the MDM name, when none is set), the contact address
// of the ACME account. An empty file field keeps what is stored. Each part is checked by the
// coordinator, which refuses a certificate and key that are not a pair, a token that does not open
// with the ABM key or is not well-formed JSON; each refusal is shown under its own field, and the
// other parts are still stored. The ACME directory and the DNS provider are not on the page: the
// directory is the coordinator's setting (the production one by default) and GoDaddy is the one
// provider the page offers.
//
// Uploads are read when Save is pressed, sent and dropped: a file is kept only as the file the
// browser handed over, never its contents. The GoDaddy secret is typed into a password field and
// dropped whether or not the coordinator took it. The page shows no value of a secret.

import { ApiError } from "../api.js";
import { toBase64, utf8 } from "../bytes.js";
import {
  h, action, describeError, docLink, enumLabel, errorView, field, filePicker, fmtUnix, trimmed,
} from "../view.js";
import * as session from "../session.js";

const TITLE = "MDM setup";
const DOC = "mdm-setup.md";
// The limits of operator.proto, in bytes.
const PEM_LIMIT = 16384;
const TOKEN_LIMIT = 32768;
const DNS_PROVIDER = "godaddy";
// The second-level labels under which a country code is a public suffix (a country code under `co`, for one).
const SECOND_LEVEL = ["ac", "co", "com", "edu", "go", "gov", "ne", "net", "or", "org"];

// What each condition of `Mdm.Status`'s `missing` says in the status line, by its token (the value
// without its prefix, in lower case): a part that is missing, or what is wrong with one.
const MISSING = {
  push_cert_missing: "push certificate",
  push_key_missing: "push key",
  abm_token_missing: "ABM token",
  name_missing: "MDM name",
  tls_cert_missing: "TLS certificate",
  tls_key_missing: "TLS key",
};
const PROBLEMS = {
  push_pair_invalid: "push certificate and key do not match",
  push_cert_expired: "push certificate expired",
  push_cert_not_yet_valid: "push certificate not valid yet",
  abm_token_unreadable: "ABM token unreadable",
  abm_token_expired: "ABM token expired",
  tls_pair_invalid: "TLS certificate and key do not match",
  tls_cert_expired: "TLS certificate expired",
  tls_cert_not_yet_valid: "TLS certificate not valid yet",
  tls_name_mismatch: "TLS certificate does not name the MDM name",
};

// What the coordinator's refusal means for the field it is shown under, by its reason.
const REFUSALS = {
  certificate_invalid: "This is not a push certificate: it must be PEM, with a topic that starts com.apple.mgmt.External.",
  key_invalid: "This is not a readable RSA private key (PEM).",
  key_mismatch: "This key is not the key of the certificate.",
  certificate_expired: "This certificate has expired.",
  topic_changed: "This certificate has another topic than the one in use.",
  no_pending_request: "No certificate request is pending: upload the certificate with its key.",
  secrets_unavailable: "The coordinator holds no master key (--mdm-secrets-key-file), so it keeps no secret.",
  abm_unavailable: "This coordinator has no ABM client.",
  abm_key_missing: "The ABM key is not held here: upload the ABM key and certificate with the token.",
  abm_token_unreadable: "This file does not open with the ABM key held, or is not a token.",
  abm_credential_refused: "Apple refused the token's signature: its parts do not belong together, or the token was revoked in ABM.",
  abm_clock_refused: "Apple refused the time of this coordinator's machine: it must be within an hour of Apple's.",
  abm_unreachable: "No answer came from Apple. Try again.",
};

// The refusal of a call in words, then the API's own code and reason in brackets.
function refusal(error) {
  const said = describeError(error);
  const words = error instanceof ApiError ? REFUSALS[error.reason] : undefined;
  return words === undefined ? said : `${words} (${said})`;
}

// The words of one condition of `Mdm.Status`'s `missing` (its token when this page does not know
// it).
export function conditionWords(value) {
  const token = String(value).replace(/^MDM_CONDITION_/, "").toLowerCase();
  return MISSING[token] || PROBLEMS[token] || token;
}

// The status line: "MDM: configured", or "MDM: missing a, b; c expired", and the name.
export function statusLine(status) {
  const tokens = (status.missing || []).map((v) => String(v).replace(/^MDM_CONDITION_/, "").toLowerCase());
  const missing = tokens.filter((t) => MISSING[t] !== undefined).map((t) => MISSING[t]);
  const wrong = tokens.filter((t) => MISSING[t] === undefined).map(conditionWords);
  const parts = missing.length > 0 ? [`missing ${missing.join(", ")}`, ...wrong] : wrong;
  const said = status.configured === true ? "configured" : parts.join("; ") || "not configured";
  return `MDM: ${said}. Name: ${status["name"] || "not set"}`;
}

// The zone to order the certificate in: the registrable domain of `name` (the last two labels, or
// the last three under a suffix of two labels such as `co` and a country code); "" when the name has fewer than two labels.
export function registrableDomain(name) {
  const labels = String(name || "").trim().toLowerCase().split(".").filter((l) => l !== "");
  if (labels.length < 2) {
    return "";
  }
  const last = labels[labels.length - 1];
  const keep = labels.length >= 3 && last.length === 2 && SECOND_LEVEL.includes(labels[labels.length - 2]) ? 3 : 2;
  return labels.slice(-keep).join(".");
}

// "stored" with the expiry when the part has one; "" when nothing is held.
function storedWords(info, notAfterUnix) {
  if (!info) {
    return "";
  }
  const date = fmtUnix(notAfterUnix === undefined ? info.notAfterUnix : notAfterUnix).slice(0, 10);
  return date === "-" ? "stored ✓" : `stored ✓ (expires ${date})`;
}

// The version in use of the secret `kind` in a `ListSecrets` reply, or undefined.
function inUse(reply, kind) {
  return ((reply && reply.secrets) || []).find((s) => s.kind === kind && s.supersededUnix === undefined);
}

// The value of the setting `name` in a `ListSettings` reply, or "".
function settingValue(reply, name) {
  const found = ((reply && reply.settings) || []).find((s) => s["name"] === name);
  return found === undefined ? "" : found.value || "";
}

// The reply of a read that only adds to the page, or null when it cannot be had.
async function optional(read) {
  try {
    return await read();
  } catch {
    return null;
  }
}

// One field of the form: its input, what is stored, the hint, the link to the documentation (when
// there is a `help` anchor) and the coordinator's refusal of the last Save.
function row(flow, key, input, hint, stored, help) {
  const refused = flow.errors[key];
  return h("div", { class: "field", name: `row-${key}` },
    input,
    stored === "" ? null : h("span", { class: "note", name: `${key}-stored` }, stored),
    h("p", { class: "note" }, hint,
      help === undefined ? null : [" ", docLink(`${key}-help`, "How do I get this?", DOC, help)]),
    refused === undefined ? null : h("p", { class: "error", role: "alert", name: `${key}-error` }, refused));
}

// The bytes of a chosen file within `limit`, else a sentence saying it is too long. The size is
// read before the contents, so a huge file is not loaded.
async function readFile(file, what, limit) {
  if (typeof file.size === "number" && file.size > limit) {
    return `${what} is at most ${limit} bytes; this file is ${file.size}.`;
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  return bytes.length > limit ? `${what} is at most ${limit} bytes; this file is ${bytes.length}.` : bytes;
}

// Whether a token file is JSON (its first character that is not white space is `{`) and not a
// `.p7m`, which is DER or S/MIME text.
function looksLikeJson(bytes) {
  for (const byte of bytes) {
    if (byte !== 0x20 && byte !== 0x09 && byte !== 0x0a && byte !== 0x0d) {
      return byte === 0x7b;
    }
  }
  return false;
}

// Whether `bytes` is a JSON object.
function wellFormedObject(bytes) {
  try {
    const value = JSON.parse(String.fromCharCode(...bytes));
    return value !== null && typeof value === "object" && !Array.isArray(value);
  } catch {
    return false;
  }
}

// The Save: each filled part in turn, each refusal on its own field. Returns the sentence the page
// says. `state` is what the coordinator held when the page was drawn.
async function save(ctx, flow, state) {
  flow.errors = {};
  const stored = [];
  const refuse = (key, text) => {
    flow.errors[key] = text;
  };
  // The bytes of the file in `flow[key]`; a sentence goes on its field and null comes back.
  const read = async (key, what, limit) => {
    const bytes = await readFile(flow[key], what, limit);
    if (typeof bytes === "string") {
      refuse(key, bytes);
      return null;
    }
    return bytes;
  };
  // Runs `call`, which stores a part; a refusal goes on `key`. Whether it was stored.
  const store = async (key, name, call) => {
    try {
      await call();
    } catch (error) {
      refuse(key, refusal(error));
      return false;
    }
    stored.push(name);
    return true;
  };

  // The MDM name, while none is set: the zone is worked out from it.
  const name = trimmed(flow.mdmName);
  if (state.mdmName === "" && name !== "") {
    try {
      const reply = await ctx.api.mdm.putName({ name });
      state.mdmName = reply["name"] || name;
      stored.push("MDM name");
    } catch (error) {
      refuse("mdmName", refusal(error));
    }
  }

  // The push certificate and its key, together: the coordinator checks they are a pair.
  if (flow.pushCert || flow.pushKey) {
    if (!flow.pushKey) {
      refuse("pushKey", "Choose the key of this certificate: the two are checked together.");
    } else if (!flow.pushCert) {
      refuse("pushCert", "Choose the certificate of this key: the two are checked together.");
    } else {
      const cert = await read("pushCert", "The certificate", PEM_LIMIT);
      const key = await read("pushKey", "The key", PEM_LIMIT);
      if (cert !== null && key !== null) {
        const where = (error) => (error instanceof ApiError && ["key_invalid", "key_mismatch"].includes(error.reason) ? "pushKey" : "pushCert");
        try {
          await ctx.api.mdm.putPushCertificate({ certificatePem: toBase64(cert), keyPem: toBase64(key) });
          stored.push("push certificate and key");
          flow.pushCert = null;
          flow.pushKey = null;
        } catch (error) {
          refuse(where(error), refusal(error));
        }
      }
    }
  }

  // The ABM key and certificate: together when both are given, so that the coordinator checks they
  // are a pair; either alone is checked against the one it holds.
  let abmKeyFailed = false;
  if (flow.abmKey || flow.abmCert) {
    const cert = flow.abmCert ? await read("abmCert", "The certificate", PEM_LIMIT) : undefined;
    const key = flow.abmKey ? await read("abmKey", "The key", PEM_LIMIT) : undefined;
    if (cert === null || key === null) {
      abmKeyFailed = true;
    } else {
      const body = key === undefined
        ? { kind: "abm_cert", material: toBase64(cert) }
        : { kind: "abm_key", material: toBase64(key), ...(cert === undefined ? {} : { withCertificate: toBase64(cert) }) };
      const name = cert !== undefined && key !== undefined ? "ABM key and certificate"
        : key === undefined ? "ABM certificate" : "ABM key";
      if (await store(key === undefined ? "abmCert" : "abmKey", name, () => ctx.api.mdm.putSecret(body))) {
        flow.abmKey = null;
        flow.abmCert = null;
      } else {
        abmKeyFailed = true;
      }
    }
  }

  // The ABM token: JSON or `.p7m`, told by its content.
  if (flow.abmToken) {
    const bytes = await read("abmToken", "The token", TOKEN_LIMIT);
    if (bytes !== null) {
      const json = looksLikeJson(bytes);
      if (json && !wellFormedObject(bytes)) {
        refuse("abmToken", "This token file is not well-formed JSON.");
      } else if (!json && abmKeyFailed) {
        refuse("abmToken", "Not tried: the ABM key was not stored, and a .p7m opens only with it.");
      } else {
        const body = json ? { tokenJson: toBase64(bytes) } : { tokenP7m: toBase64(bytes) };
        if (await store("abmToken", "ABM token", () => ctx.api.mdm.putAbmToken(body))) {
          flow.abmToken = null;
        }
      }
    }
  }

  // The GoDaddy credential, and the zone it orders in. The typed values are dropped whatever the
  // answer is.
  const dnsKey = trimmed(flow.dnsKey);
  const dnsSecret = trimmed(flow.dnsSecret);
  flow.dnsKey = "";
  flow.dnsSecret = "";
  let credentialStored = false;
  if (dnsKey !== "" || dnsSecret !== "") {
    const zone = registrableDomain(state.mdmName);
    if (dnsKey === "" || dnsSecret === "") {
      refuse(dnsKey === "" ? "dnsKey" : "dnsSecret", "Give the key and the secret together.");
    } else if (zone === "") {
      refuse("dnsKey", "The MDM name is not set, so the DNS zone cannot be worked out: give the MDM name.");
    } else {
      const body = { provider: DNS_PROVIDER, kind: "sso-key", key: dnsKey, secret: dnsSecret };
      credentialStored = await store("dnsKey", "GoDaddy credential",
        () => ctx.api.mdm.putSecret({ kind: "dns_credential", material: toBase64(utf8(JSON.stringify(body))) }));
      if (credentialStored && state.dnsZone === "") {
        await store("dnsKey", "DNS zone", () => ctx.api.mdm.putSetting({ name: "dns_zone", value: zone }));
      }
    }
  }

  // The contact address: when the operator changed it, or the credential is stored with it.
  const contact = trimmed(flow.acmeEmail);
  if (contact !== "" && contact !== state.acmeContact && (contact !== flow.acmeEmailShown || credentialStored)) {
    if (await store("contact", "contact address", () => ctx.api.mdm.putSetting({ name: "acme_contact", value: contact }))) {
      flow.acmeEmailShown = contact;
    }
  }

  const refused = Object.keys(flow.errors).length;
  if (stored.length === 0 && refused === 0) {
    return "Nothing to save: choose a file or fill a field.";
  }
  const said = stored.length === 0 ? "Nothing was stored." : `Stored: ${stored.join(", ")}.`;
  return refused === 0 ? said : `${said} ${refused} field${refused === 1 ? "" : "s"} refused: see the error under each.`;
}

// What the ABM check says, by the token of `MdmAbmResult`.
const ABM_WORDS = {
  no_token: "No ABM token is held: nothing to check.",
  not_checked: "An ABM token is held and this coordinator cannot ask Apple about it.",
  accepted: "Apple accepts the ABM token.",
  rejected: "Apple refused the ABM token.",
  unreachable: "Apple could not be reached, or its answer says nothing of the token.",
  unreadable: "The ABM token held cannot be read, so Apple was not asked.",
};

// What the listener does, in a sentence.
function listenerWords(reply) {
  if (reply.listener !== true) {
    return "The Macs' listener is not running (--mdm-listen is not set).";
  }
  if (reply.serving !== true) {
    return "The Macs' listener runs and has no TLS certificate to serve: no Mac can connect.";
  }
  const names = (reply.servedNames || []).join(", ") || "-";
  return reply.servedNamesTheName === true
    ? `The Macs' listener serves a certificate for ${names}, which names the MDM name.`
    : `The Macs' listener serves a certificate for ${names}, which does not name the MDM name.`;
}

// The result of a verification: one line for what each part found.
export function verifyResult(reply) {
  const abm = reply.abm || {};
  const token = enumLabel(abm.result, "MDM_ABM_RESULT_");
  const lines = [
    ["Setup", statusLine(reply.status || {})],
    ["Listener", listenerWords(reply)],
    ["ABM", (ABM_WORDS[token] || token) + (abm.organization ? ` Account: ${abm.organization}.` : "")
      + (abm.detail ? ` (${abm.detail})` : "")],
  ];
  return h("ul", { name: "verify-result" }, lines.map(([k, v]) => h("li", {}, `${k}: ${v}`)));
}

async function again(ctx, flow) {
  ctx.show(await view(ctx, flow));
}

// The link next to the token field that gives the coordinator's own public certificate, to add as
// the key of a new server entry in ABM; the coordinator makes the key pair at the first call.
function publicCertificateLink(ctx, flow) {
  return h("button", {
    type: "button",
    class: "linkish",
    name: "download-abm-certificate",
    on: {
      click: async () => {
        try {
          const reply = await ctx.api.mdm.getAbmPublicKey({});
          ctx.download({ name: "abm-server.crt", mime: "text/plain", text: reply.certificatePem || "" });
          await again(ctx, flow);
        } catch (error) {
          ctx.report(refusal(error));
        }
      },
    },
  }, "Download the public certificate to upload to ABM");
}

async function view(ctx, flow) {
  let status;
  try {
    status = await ctx.api.mdm.status({});
  } catch (error) {
    return errorView(TITLE, error);
  }
  const settings = await optional(() => ctx.api.mdm.listSettings({}));
  const secrets = await optional(() => ctx.api.mdm.listSecrets({}));
  const state = {
    mdmName: status["name"] || "",
    dnsZone: settingValue(settings, "dns_zone"),
    acmeContact: settingValue(settings, "acme_contact"),
  };
  if (flow.acmeEmail === null) {
    const who = session.who();
    const username = who === null ? "" : who.username;
    flow.acmeEmail = state.acmeContact !== "" ? state.acmeContact : username.includes("@") ? username : "";
    flow.acmeEmailShown = flow.acmeEmail;
  }
  const abmKey = inUse(secrets, "abm_key");
  const form = h("div", { class: "form", name: "mdm-form" },
    state.mdmName !== "" ? null : row(flow, "mdmName", field("MDM name", "mdm-name", flow, "mdmName", { maxlength: 253 }),
      "The DNS name the Macs enroll at, such as mdm.example.com. It cannot be changed once a Mac has enrolled."),
    row(flow, "pushCert", filePicker("Push certificate (PEM)", "push-cert", flow, "pushCert"),
      "The Apple push certificate, from mdmcert.download or the Apple Push Certificates Portal.",
      storedWords(status.pushCert, status.pushNotAfterUnix), "push-certificate"),
    row(flow, "pushKey", filePicker("Push certificate private key (PEM)", "push-key", flow, "pushKey"),
      "The private key the push certificate was made with.", storedWords(status.pushKey), "push-certificate"),
    row(flow, "abmToken", filePicker("Apple Business Manager token (.p7m or JSON)", "abm-token", flow, "abmToken"),
      "The token downloaded from ABM (a .p7m file), or an existing token (JSON).",
      storedWords(status.abmToken, status.abmNotAfterUnix), "abm-token"),
    secrets !== null && abmKey === undefined ? publicCertificateLink(ctx, flow) : null,
    row(flow, "abmKey", filePicker("Apple Business Manager private key (PEM)", "abm-key", flow, "abmKey"),
      "The key the ABM token is encrypted to, when taking over an existing ABM server.",
      storedWords(abmKey), "abm-key-pair"),
    row(flow, "abmCert", filePicker("Apple Business Manager certificate (PEM)", "abm-cert", flow, "abmCert"),
      "The certificate of that key.", storedWords(inUse(secrets, "abm_cert")), "abm-key-pair"),
    row(flow, "dnsKey", field("GoDaddy API key", "dns-key", flow, "dnsKey", { maxlength: 512 }),
      "For the TLS certificate of the MDM name from Let's Encrypt.", storedWords(status.dnsCredential), "godaddy-api-key"),
    row(flow, "dnsSecret", field("GoDaddy API secret", "dns-secret", flow, "dnsSecret", { type: "password", maxlength: 512 }),
      "The secret of that key.", "", "godaddy-api-key"),
    row(flow, "contact", field("Contact email for Let's Encrypt", "contact", flow, "acmeEmail", { maxlength: 254 }),
      "Let's Encrypt writes here about the certificate.", ""),
    action(ctx, "Save", "save", async () => {
      const said = await save(ctx, flow, state);
      await again(ctx, flow);
      return said;
    }));
  return h("section", { class: "page" },
    h("h2", {}, TITLE),
    h("p", { name: "mdm-status" }, statusLine(status)),
    form,
    h("div", { name: "verify-section" },
      action(ctx, "Verify", "verify", async () => {
        flow.verified = await ctx.api.mdm.verify({});
        await again(ctx, flow);
        return "Verified. See what each part found.";
      }),
      flow.verified === null ? null : verifyResult(flow.verified)));
}

export async function render(ctx) {
  return view(ctx, {
    mdmName: "",
    pushCert: null,
    pushKey: null,
    abmToken: null,
    abmKey: null,
    abmCert: null,
    dnsKey: "",
    dnsSecret: "",
    acmeEmail: null,
    contactShown: "",
    errors: {},
    verified: null,
  });
}
