// The operator's sign-in, as this page knows it: who it is, the CSRF token the coordinator gave
// with the session, and whether a new password is owed first. It is held in this module's memory
// and nowhere else: not in browser storage, not in the address. The session itself is the
// browser's own HttpOnly credential, which no script here can read or send by hand; the CSRF
// token is the one secret this module keeps, and reloading the page asks the coordinator for it
// again. Nothing here renders, logs or copies it.

let held = null;
// One more at every sign-in and every sign-out, so what was started under one sign-in can tell
// that it no longer belongs to the current one.
let generation = 0;
const listeners = [];

function changed() {
  generation += 1;
  for (const listener of listeners) {
    listener();
  }
}

// Keeps the session the coordinator described: `{username, csrf, must_change, must_change_until}`
// as `/v1/login`, `/v1/session` and `/v1/password` answer it. A description without a user name
// and a CSRF token is refused: false, and nothing kept. A change of password answers the same
// shape for the same session, and replaces what was held.
export function signIn(view) {
  const ok = view !== null && typeof view === "object"
    && typeof view.username === "string" && view.username !== ""
    && typeof view.csrf === "string" && view.csrf !== "";
  if (!ok) {
    return false;
  }
  const until = Number(view.must_change_until);
  held = {
    username: view.username,
    csrf: view.csrf,
    mustChange: view.must_change === true,
    mustChangeUntil: view.must_change === true && Number.isFinite(until) && until > 0 ? until : 0,
  };
  changed();
  return true;
}

// Notes that the coordinator refused a call until the password is changed, which the session
// did not say: the page then asks for the change. False when nobody is signed in.
export function requireChange() {
  if (held === null) {
    return false;
  }
  held = { ...held, mustChange: true };
  changed();
  return true;
}

// Forgets the session: the "Sign out" control, and a refusal.
export function signOut() {
  const had = held !== null;
  held = null;
  if (had) {
    changed();
  }
}

// Forgets the session if its CSRF token is still `refused`, the one a refused call sent; a
// session the operator has signed in to since is kept.
export function forget(refused) {
  if (held !== null && held.csrf === refused) {
    signOut();
  }
}

// The current sign-in's number.
export function revision() {
  return generation;
}

// The CSRF token, or null when nobody is signed in.
export function getCsrf() {
  return held === null ? null : held.csrf;
}

// Who is signed in and whether a new password is owed: `{username, mustChange, mustChangeUntil}`
// (the deadline in unix seconds, 0 when none is known), or null. The CSRF token is not in it.
export function who() {
  if (held === null) {
    return null;
  }
  return { username: held.username, mustChange: held.mustChange, mustChangeUntil: held.mustChangeUntil };
}

export function isSignedIn() {
  return held !== null;
}

// Calls `listener` after every sign-in and every sign-out.
export function onChange(listener) {
  listeners.push(listener);
}
