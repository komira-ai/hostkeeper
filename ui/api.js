// The data layer: the one place the app talks to the network.
//
// The pages call the operator API through the client generated from the protos
// (`gen/client.js`, its types in `gen/client.d.ts`): `api.<service>.<method>(request)`, as
// `api.hosts.list(request)`. Each of its functions is one `call` of this module's: a
// POST of the request's canonical JSON to `/<package>.<Service>/<Method>` on the origin that
// served the app. The browser attaches the session credential it holds, which no script here can
// read; this module adds the CSRF token of the session in the `X-HK-CSRF` header. The reply is the
// response's canonical JSON, or, on a refusal, an HTTP error status with the body
// `{"code": "<gRPC code>", "reason": "<refusal token>"}`.
//
// The four sign-in routes (`/v1/login`, `/v1/session`, `/v1/logout`, `/v1/password`) are
// `createAuth`'s. They answer `{"error": "<token>"}` on a refusal, which becomes the same
// `ApiError`.
//
// `fetch` and the CSRF token are injected, so tests run this module against a fake gateway. No
// error this module throws carries the CSRF token, a password, a header or a reply body: only
// the code, the reason, the HTTP status and, for a password that breaks the policy, the rule's
// text, each checked against its grammar first.

import { createClient } from "./gen/client.js";

// A method's full name, as the descriptors spell it: `hostkeeper.operator.v1.Hosts/List`.
const METHOD = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*\/[A-Z][A-Za-z0-9]*$/;
// A gRPC status code's name, as the gateway writes it.
const CODE = /^[A-Z][A-Z_]{0,31}$/;
// A refusal token: the refusal enum value's name without its prefix, in lower case.
const REASON = /^[a-z][a-z0-9_]{0,63}$/;

// Why a call did not return a reply. `code` is the gRPC code's name, or one of this module's own:
// `SIGN_IN_REQUIRED` (nobody is signed in; nothing was sent), `NETWORK` (no reply arrived),
// `BAD_REPLY` (a reply that is not JSON), `BAD_METHOD` (a name that is not a method's; nothing was
// sent). `reason` is the refusal token, or "" when the coordinator gave none. `httpStatus` is 0
// when no reply arrived. `detail` is the text of the password rule a new password broke, or "".
export class ApiError extends Error {
  constructor(code, reason, httpStatus, detail) {
    super(reason === "" ? code : `${code}: ${reason}`);
    this.code = code;
    this.reason = reason;
    this.httpStatus = httpStatus;
    this.detail = detail === undefined ? "" : detail;
  }

  get name() {
    return "ApiError";
  }

  // The session is over or was never there, or the CSRF token it sent was not the session's: the
  // app returns to sign-in. Any other 403 is a refusal of the call, not of the sign-in.
  get signInRequired() {
    return (
      this.code === "SIGN_IN_REQUIRED" ||
      this.code === "UNAUTHENTICATED" ||
      (this.code === "PERMISSION_DENIED" && this.reason === "csrf")
    );
  }

  // The session is good but its user must choose a new password before anything else is served.
  get passwordChangeRequired() {
    return this.code === "PERMISSION_DENIED" && this.reason === "password_change_required";
  }
}

// What a refusal body says, parsed: an object, or {} for anything else.
function bodyOf(text) {
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch (_) {
    parsed = null;
  }
  return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
}

// `value` if it is a string of `grammar` that is not part of any of `secrets`, and holds none of
// them: a refusal never carries text it should not. Else null.
function clean(value, grammar, secrets) {
  const ok = typeof value === "string" && grammar.test(value)
    && secrets.every((secret) => secret === "" || (!secret.includes(value) && !value.includes(secret)));
  return ok ? value : null;
}

// The refusal a gateway error reply says. `text` is null when the body could not be read: the
// status still says what it says.
function refusal(status, text, csrf) {
  const body = bodyOf(text);
  let code = clean(body.code, CODE, [csrf]) ?? "UNKNOWN";
  if (status === 401) {
    code = "UNAUTHENTICATED";
  } else if (status === 403) {
    code = "PERMISSION_DENIED";
  }
  return new ApiError(code, clean(body.reason, REASON, [csrf]) ?? "", status);
}

// The gRPC code an HTTP status of a sign-in route stands for.
const STATUS_CODES = new Map([
  [400, "INVALID_ARGUMENT"],
  [401, "UNAUTHENTICATED"],
  [403, "PERMISSION_DENIED"],
  [405, "UNIMPLEMENTED"],
  [429, "RESOURCE_EXHAUSTED"],
  [500, "INTERNAL"],
  [503, "UNAVAILABLE"],
]);
// A password rule's text, as the coordinator words it: one short line.
const DETAIL = /^[^\u0000-\u001f\u007f]{1,200}$/;

// The refusal tokens the sign-in routes answer. Any other is not kept, so a reply that echoed a
// password could not put it in an error.
const AUTH_ERRORS = new Set([
  "bad_request", "csrf", "initial_password_expired", "internal", "invalid_credentials",
  "method_not_allowed", "password_policy", "sign_in_required", "store_unreachable", "try_later",
]);

// The refusal a sign-in route says: `{"error": "<token>"}`, and for a password that breaks the
// policy `{"error": "password_policy", "reason": "<rule>"}`. `secrets` are the passwords the
// request sent: a rule that holds one is not kept.
function authRefusal(status, text, secrets) {
  const body = bodyOf(text);
  const reason = AUTH_ERRORS.has(body.error) ? body.error : "";
  const rule = typeof body.reason === "string" && DETAIL.test(body.reason)
    && secrets.every((secret) => secret === "" || !body.reason.includes(secret));
  return new ApiError(STATUS_CODES.get(status) ?? "UNKNOWN", reason, status,
    reason === "password_policy" && rule ? body.reason : "");
}

// The one place a request leaves: `fetch` is the browser's, or a test's fake. The browser
// attaches the session credential (same origin), `csrf` goes in `X-HK-CSRF` when there is one,
// and no cache, redirect or referrer is allowed to carry anything further. `body` is sent as
// JSON when there is one. Rejects with NETWORK when no reply arrives.
async function send(fetch, method, url, csrf, body) {
  const headers = {};
  if (body !== undefined) {
    headers["content-type"] = "application/json";
  }
  if (csrf !== null) {
    headers["X-HK-CSRF"] = csrf;
  }
  const init = {
    method,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    referrerPolicy: "no-referrer",
  };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
  }
  try {
    return await fetch(url, init);
  } catch (_) {
    throw new ApiError("NETWORK", "", 0);
  }
}

// A reply's text, or null when it cannot be read.
async function textOf(response) {
  try {
    return await response.text();
  } catch (_) {
    return null;
  }
}

// A success reply's body as an object: {} for an empty one; BAD_REPLY for anything else.
function replyOf(text, status) {
  if (text === null) {
    throw new ApiError("NETWORK", "", status);
  }
  if (text === "") {
    return {};
  }
  let reply;
  try {
    reply = JSON.parse(text);
  } catch (_) {
    throw new ApiError("BAD_REPLY", "", status);
  }
  if (reply === null || typeof reply !== "object" || Array.isArray(reply)) {
    throw new ApiError("BAD_REPLY", "", status);
  }
  return reply;
}

// The function that sends one call: `call(method, body)` posts `body` to `method`
// (`<package>.<Service>/<Method>`) and resolves to the reply, or rejects with an `ApiError`.
// `getCsrf()` returns the session's CSRF token or null. `refused(error, csrf)` is told that the
// call, which sent `csrf`, was refused as a sign-in or a password change is: the app forgets the
// session only if it is still the one that sent it, so a late refusal of a session the operator
// has since replaced signs nothing out.
export function createCall({ fetch, getCsrf, refused }) {
  return async function call(method, body) {
    if (typeof method !== "string" || !METHOD.test(method)) {
      throw new ApiError("BAD_METHOD", "", 0);
    }
    const csrf = getCsrf();
    if (typeof csrf !== "string" || csrf === "") {
      throw new ApiError("SIGN_IN_REQUIRED", "", 0);
    }
    const response = await send(fetch, "POST", `/${method}`, csrf, body === undefined ? {} : body);
    const status = response.status;
    const text = await textOf(response);
    if (status < 200 || status > 299) {
      const error = refusal(status, text, csrf);
      if (error.signInRequired || error.passwordChangeRequired) {
        refused(error, csrf);
      }
      throw error;
    }
    return replyOf(text, status);
  };
}

// The four sign-in routes, over `fetch`. Each resolves to the session the coordinator describes
// (`{username, csrf, must_change, must_change_until}`) or, for `logout`, `{}`; and rejects with an
// `ApiError` whose reason is the route's refusal token (`invalid_credentials`,
// `sign_in_required`, `initial_password_expired`, `try_later`, `password_policy`, ...). A password
// is sent in the body and is in no error.
export function createAuth({ fetch }) {
  async function route(method, url, csrf, body, secrets) {
    const response = await send(fetch, method, url, csrf, body);
    const status = response.status;
    const text = await textOf(response);
    if (status < 200 || status > 299) {
      throw authRefusal(status, text, secrets);
    }
    return replyOf(text, status);
  }
  return {
    // `POST /v1/login`: the session for a user name and password.
    login: (username, password) => route("POST", "/v1/login", null, { username, password }, [password]),
    // `GET /v1/session`: the session the browser holds, or `sign_in_required`.
    session: () => route("GET", "/v1/session", null, undefined, []),
    // `POST /v1/logout`: ends the session `csrf` belongs to.
    logout: (csrf) => route("POST", "/v1/logout", csrf, {}, []),
    // `POST /v1/password`: the session's own password.
    changePassword: (csrf, current, next) => route(
      "POST", "/v1/password", csrf, { current, new: next }, [current, next].filter((p) => p !== "")),
  };
}

// The client the pages call, `api.<service>.<method>(request)`: the generated one, over
// `createCall`'s `call` with the same `fetch` and CSRF token.
export function createApi(deps) {
  return createClient(createCall(deps));
}
