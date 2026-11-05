// The operator's token, held in this module's memory and nowhere else: not in browser storage,
// not in the address. Reloading or closing the tab forgets it, and the app asks for it again.
// Nothing here renders, logs or copies it.

let token = null;
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

// Keeps the token the operator pasted. An empty or blank one is refused: false, and nothing kept.
export function signIn(pasted) {
  const value = typeof pasted === "string" ? pasted.trim() : "";
  if (value === "") {
    return false;
  }
  token = value;
  changed();
  return true;
}

// Forgets the token: the "Sign out" control.
export function signOut() {
  const had = token !== null;
  token = null;
  if (had) {
    changed();
  }
}

// Forgets the token if it is still `refused`, which a reply refused; a token the operator has
// pasted since is kept.
export function forget(refused) {
  if (token !== null && token === refused) {
    signOut();
  }
}

// The current sign-in's number.
export function revision() {
  return generation;
}

export function getToken() {
  return token;
}

export function isSignedIn() {
  return token !== null;
}

// Calls `listener` after every sign-in and every sign-out.
export function onChange(listener) {
  listeners.push(listener);
}
