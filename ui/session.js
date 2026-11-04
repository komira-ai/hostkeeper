// The operator's token, held in this module's memory and nowhere else: not in browser storage,
// not in the address. Reloading or closing the tab forgets it, and the app asks for it again.
// Nothing here renders, logs or copies it.

let token = null;
const listeners = [];

function changed() {
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

// Forgets the token: the "Sign out" control, and a reply that says the token is not accepted.
export function signOut() {
  const had = token !== null;
  token = null;
  if (had) {
    changed();
  }
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
