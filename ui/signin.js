// The two forms that stand before the pages: the sign-in, and the choice of a new password that a
// first sign-in, or a password an admin set, owes before anything else works. They are views
// only: what they do with what is typed is `app.js`'s. A password is held in a variable of the
// form while it is typed, handed to the handler once and forgotten; it is shown nowhere.

import { fmtUnix, h } from "./view.js";

// The words for a refusal of a sign-in route. `NETWORK` is no reply at all; any other code or
// token this does not know reads as a failure of the coordinator, never as the reply's own text.
const WORDS = new Map([
  ["invalid_credentials", "The user name or password is wrong."],
  ["initial_password_expired", "The initial password has expired. Ask an admin for a new one."],
  ["try_later", "Too many attempts, or the coordinator is busy. Try again in a few minutes."],
  ["bad_request", "Enter a user name and a password."],
  ["csrf", "The coordinator did not take the request as coming from this page. Reload the page."],
  ["sign_in_required", "Your session has ended. Sign in again."],
]);

export function wordsFor(error) {
  if (error.code === "NETWORK") {
    return "The coordinator did not answer.";
  }
  if (error.reason === "password_policy") {
    return error.detail === ""
      ? "The new password is not allowed."
      : `The new password is not allowed: ${error.detail}.`;
  }
  return WORDS.get(error.reason) ?? "The coordinator could not do that. Try again.";
}

function alertLine(message) {
  return message ? h("p", { class: "error", role: "alert" }, message) : null;
}

// The sign-in form. `username` is what to show in the user name field (what was typed before a
// refusal), `message` why the last try or the last session ended, and `submit(username, password)`
// is called with what was typed.
export function signInView({ username, message, submit }) {
  const form = { username: username ?? "", password: "" };
  return h("form", {
    class: "sign-in",
    on: {
      submit: () => {
        const typed = { ...form };
        form.password = "";
        submit(typed.username, typed.password);
      },
    },
  },
  h("h1", {}, "hostkeeper"),
  h("p", {}, "Sign in with your user name and password."),
  h("label", {}, "User name",
    h("input", {
      type: "text", name: "username", autocomplete: "username", spellcheck: "false", value: form.username,
      on: { input: (e) => { form.username = e.target.value; } },
    })),
  h("label", {}, "Password",
    h("input", {
      type: "password", name: "password", autocomplete: "current-password", spellcheck: "false",
      on: { input: (e) => { form.password = e.target.value; } },
    })),
  h("button", { type: "submit", name: "sign-in" }, "Sign in"),
  alertLine(message));
}

// The form of the new password: the current one and the new one twice. `until` is the second the
// coordinator will stop taking the change (unix seconds; 0 when it did not say), `message` why the
// last try failed, `submit(current, next, again)` takes what was typed and `signOut()` leaves.
export function changeView({ username, until, message, submit, signOut }) {
  const form = { current: "", next: "", again: "" };
  return h("form", {
    class: "sign-in",
    on: {
      submit: () => {
        const typed = { ...form };
        form.current = "";
        form.next = "";
        form.again = "";
        submit(typed.current, typed.next, typed.again);
      },
    },
  },
  h("h1", {}, "hostkeeper"),
  h("h2", {}, "Choose a new password"),
  h("p", {}, `${username} is signed in with a temporary password. Choose a new one to continue: nothing else works until you do.`),
  until > 0 ? h("p", { class: "note" }, `The temporary password can be exchanged until ${fmtUnix(until)}.`) : null,
  h("label", {}, "Current password",
    h("input", {
      type: "password", name: "current", autocomplete: "current-password", spellcheck: "false",
      on: { input: (e) => { form.current = e.target.value; } },
    })),
  h("label", {}, "New password",
    h("input", {
      type: "password", name: "new", autocomplete: "new-password", spellcheck: "false",
      on: { input: (e) => { form.next = e.target.value; } },
    })),
  h("label", {}, "New password again",
    h("input", {
      type: "password", name: "again", autocomplete: "new-password", spellcheck: "false",
      on: { input: (e) => { form.again = e.target.value; } },
    })),
  h("button", { type: "submit", name: "change-password" }, "Change password"),
  h("button", { type: "button", name: "sign-out", on: { click: () => signOut() } }, "Sign out"),
  alertLine(message));
}
