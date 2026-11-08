// The data layer: the one place the app talks to the network.
//
// The pages call the operator API through the client generated from the protos
// (`gen/client.js`, its types in `gen/client.d.ts`): `api.<service>.<method>(request)`, as
// `api.hosts.list(request)`. Each of its functions is one `call` of this module's: a
// POST of the request's canonical JSON to `/<package>.<Service>/<Method>` on the origin that
// served the app, with the operator's token in the `Authorization` header. The reply is the
// response's canonical JSON, or, on a refusal, an HTTP error status with the body
// `{"code": "<gRPC code>", "reason": "<refusal token>"}`.
//
// `fetch` and the token are injected (`createApi`), so tests run this module against a fake
// gateway. No error this module throws carries the token, a header or a reply body: only the
// code, the reason and the HTTP status, each checked against its grammar first.

import { createClient } from "./gen/client.js";

// A method's full name, as the descriptors spell it: `hostkeeper.operator.v1.Hosts/List`.
const METHOD = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*\/[A-Z][A-Za-z0-9]*$/;
// A gRPC status code's name, as the gateway writes it.
const CODE = /^[A-Z][A-Z_]{0,31}$/;
// A refusal token: the refusal enum value's name without its prefix, in lower case.
const REASON = /^[a-z][a-z0-9_]{0,63}$/;

// Why a call did not return a reply. `code` is the gRPC code's name, or one of this module's own:
// `SIGN_IN_REQUIRED` (no token; nothing was sent), `NETWORK` (no reply arrived), `BAD_REPLY`
// (a reply that is not JSON), `BAD_METHOD` (a name that is not a method's; nothing was sent).
// `reason` is the refusal token, or "" when the gateway gave none. `httpStatus` is 0 when no
// reply arrived.
export class ApiError extends Error {
  constructor(code, reason, httpStatus) {
    super(reason === "" ? code : `${code}: ${reason}`);
    this.code = code;
    this.reason = reason;
    this.httpStatus = httpStatus;
  }

  get name() {
    return "ApiError";
  }

  // The token was refused, or there is none: the app returns to sign-in.
  get signInRequired() {
    return (
      this.code === "SIGN_IN_REQUIRED" ||
      this.code === "UNAUTHENTICATED" ||
      this.code === "PERMISSION_DENIED"
    );
  }
}

// The refusal a gateway error reply says. A code or a reason that breaks its grammar, that is
// part of the token or that holds the token, is not kept: the error never carries text it should
// not. `text` is null when the body could not be read: the status still says what it says.
function refusal(status, text, token) {
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch (_) {
    parsed = null;
  }
  const body = parsed !== null && typeof parsed === "object" ? parsed : {};
  const kept = (value, grammar) => typeof value === "string" && grammar.test(value)
    && !token.includes(value) && !value.includes(token);
  let code = kept(body.code, CODE) ? body.code : "UNKNOWN";
  if (status === 401) {
    code = "UNAUTHENTICATED";
  } else if (status === 403) {
    code = "PERMISSION_DENIED";
  }
  const reason = kept(body.reason, REASON) ? body.reason : "";
  return new ApiError(code, reason, status);
}

// The function that sends one call: `call(method, body)` posts `body` to `method`
// (`<package>.<Service>/<Method>`) and resolves to the reply, or rejects with an `ApiError`.
// `fetch` (a URL and an init object) is the browser's, or a test's fake. `getToken()` returns
// the token or null. `clearToken(error, token)` is told that `token`, the one the refused call
// sent, was refused: it forgets the token only if it is still the one held, so a late refusal of
// a token the operator has since replaced signs nothing out.
export function createCall({ fetch, getToken, clearToken }) {
  return async function call(method, body) {
    if (typeof method !== "string" || !METHOD.test(method)) {
      throw new ApiError("BAD_METHOD", "", 0);
    }
    const token = getToken();
    if (typeof token !== "string" || token === "") {
      throw new ApiError("SIGN_IN_REQUIRED", "", 0);
    }
    let response;
    try {
      response = await fetch(`/${method}`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(body === undefined ? {} : body),
        credentials: "omit",
        cache: "no-store",
        redirect: "error",
        referrerPolicy: "no-referrer",
      });
    } catch (_) {
      throw new ApiError("NETWORK", "", 0);
    }
    const status = response.status;
    let text = null;
    try {
      text = await response.text();
    } catch (_) {
      text = null;
    }
    if (status < 200 || status > 299) {
      const error = refusal(status, text, token);
      if (error.signInRequired) {
        clearToken(error, token);
      }
      throw error;
    }
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
  };
}

// The client the pages call, `api.<service>.<method>(request)`: the generated one, over
// `createCall`'s `call` with the same `fetch` and token.
export function createApi(deps) {
  return createClient(createCall(deps));
}
