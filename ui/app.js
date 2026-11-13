// The app's frame: sign-in, the page list, sign-out and the status line.
//
// A person signs in with a user name and password (`signin.js`); the coordinator answers with a
// session that the browser keeps for it, and a CSRF token that goes to `session.js` and from there
// only to `api.js`, which sends it in the X-HK-CSRF header. A reply that ends the session forgets
// it, and the frame shows the sign-in form again; a session that owes a new password shows the
// form for it and nothing else. A reload asks the coordinator for the session again. Pages are
// switched in memory: the address never changes, so it never carries anything.

import * as session from "./session.js";
import { createApi, createAuth } from "./api.js";
import { changeView, signInView, wordsFor } from "./signin.js";
import { h, describeError, errorView, mount } from "./view.js";
import * as fleet from "./pages/fleet.js";
import * as host from "./pages/host.js";
import * as rollout from "./pages/rollout.js";
import * as packages from "./pages/packages.js";
import * as hostgroups from "./pages/hostgroups.js";
import * as release from "./pages/release.js";
import * as holds from "./pages/holds.js";
import * as enrollment from "./pages/enrollment.js";
import * as inventory from "./pages/inventory.js";
import * as audit from "./pages/audit.js";
import * as users from "./pages/users.js";
import * as tokens from "./pages/tokens.js";
import * as mdm from "./pages/mdm.js";

const PAGES = [
  ["fleet", "Fleet", fleet],
  ["host", "Host", host],
  ["rollout", "Rollout", rollout],
  ["packages", "Packages", packages],
  ["hostgroups", "Host groups", hostgroups],
  ["release", "Release", release],
  ["holds", "Holds", holds],
  ["enrollment", "Enrollment", enrollment],
  ["inventory", "Inventory", inventory],
  ["audit", "Audit", audit],
  ["users", "Users", users],
  ["tokens", "API tokens", tokens],
  ["mdm", "MDM setup", mdm],
];

// Starts the app in `root`. `doc` is the document, `fetch` the browser's fetch, `confirm` a
// yes-or-no question to the operator (the browser's confirm), `now` the time in unix seconds and
// `download` the saving of a file the page offers (`{name, mime, text}` or `{name, mime, base64}`),
// each injected so tests can fake them. Returns the frame's controls, which the browser does not
// need.
export function startApp({ doc, root, fetch, confirm, now, download }) {
  // Why the session last ended on the coordinator's word, shown on the sign-in form it returns to.
  let notice = null;
  // A sign-in or a change of password is on its way: a second submit waits for it.
  let working = false;
  const auth = createAuth({ fetch });
  const api = createApi({
    fetch,
    getCsrf: session.getCsrf,
    refused: (error, sent) => {
      if (session.getCsrf() !== sent) {
        return;
      }
      if (error.passwordChangeRequired) {
        session.requireChange();
      } else {
        notice = `Your session has ended (${describeError(error)}). Sign in again.`;
        session.forget(sent);
      }
    },
  });
  let current = { view: "fleet", params: {} };
  let shown = null;
  let status = "";
  let generation = 0;

  function frame(content) {
    return h("div", { class: "frame" },
      h("header", {},
        h("h1", {}, "hostkeeper"),
        h("nav", { "aria-label": "pages" },
          PAGES.map(([name, title]) => h("button", {
            type: "button",
            name: `page-${name}`,
            class: name === current.view ? "current" : "",
            on: { click: () => navigate(name, {}) },
          }, title)),
          h("span", { class: "note", name: "who" }, session.who()?.username ?? ""),
          h("button", { type: "button", name: "sign-out", on: { click: () => signOutNow() } }, "Sign out"))),
      h("p", { class: "status", role: "status" }, status),
      h("main", {}, content));
  }

  function draw() {
    mount(doc, root, frame(shown === null ? h("p", {}, "Loading...") : shown));
  }

  function showSignIn(message, username) {
    mount(doc, root, signInView({ username, message, submit: signInWith }));
  }

  function showChange(message) {
    const who = session.who();
    if (who === null) {
      return;
    }
    mount(doc, root, changeView({
      username: who.username,
      until: who.mustChangeUntil,
      message,
      submit: changeWith,
      signOut: signOutNow,
    }));
  }

  // `POST /v1/login`; the session it answers is kept, which draws what comes next.
  async function signInWith(username, password) {
    if (working) {
      return;
    }
    working = true;
    let view;
    try {
      view = await auth.login(username, password);
    } catch (error) {
      working = false;
      showSignIn(wordsFor(error), username);
      return;
    }
    working = false;
    if (!session.signIn(view)) {
      showSignIn("The coordinator's answer was not a session.", username);
    }
  }

  // `POST /v1/password` from the form of the new password.
  async function changeWith(current, next, again) {
    if (working) {
      return;
    }
    if (next !== again) {
      showChange("The two new passwords are not the same.");
      return;
    }
    const sent = session.getCsrf();
    working = true;
    let view;
    try {
      view = await auth.changePassword(sent, current, next);
    } catch (error) {
      working = false;
      if (session.getCsrf() !== sent) {
        return;
      }
      if (error.reason === "initial_password_expired") {
        notice = wordsFor(error);
        signOutNow();
      } else if (error.reason === "sign_in_required" || error.reason === "csrf") {
        notice = wordsFor(error);
        session.forget(sent);
      } else if (error.reason === "invalid_credentials") {
        showChange("The current password is wrong.");
      } else {
        showChange(wordsFor(error));
      }
      return;
    }
    working = false;
    if (session.getCsrf() === sent && !session.signIn(view)) {
      showChange("The coordinator's answer was not a session.");
    }
  }

  // Leaves at once, then tells the coordinator to end the session; if it cannot be told, the
  // form says the session ends only when it has been idle long enough.
  function signOutNow() {
    const sent = session.getCsrf();
    if (sent === null) {
      return;
    }
    session.forget(sent);
    auth.logout(sent).catch((error) => {
      if (error.reason !== "sign_in_required" && !session.isSignedIn()) {
        showSignIn("The coordinator did not confirm the sign-out: the session ends when it has been idle long enough.");
      }
    });
  }

  // Asks the coordinator for the session the browser already holds (after a reload).
  async function restore() {
    let view;
    try {
      view = await auth.session();
    } catch (error) {
      if (error.reason === "sign_in_required") {
        showSignIn(null);
      } else {
        showSignIn(wordsFor(error));
      }
      return;
    }
    if (!session.signIn(view)) {
      showSignIn("The coordinator's answer was not a session.");
    }
  }

  // What a page may do. `refresh` and `report` act while the operator stays on the page the
  // context was made for, under the sign-in it was made under; `show` only while this rendering
  // of it is the latest.
  function context(mine, at) {
    const signedIn = session.revision();
    const here = () => current === at && session.isSignedIn() && session.revision() === signedIn;
    return {
      api,
      params: at.params,
      confirm: (question) => confirm(question) === true,
      now: () => now(),
      download: (file) => download(file),
      navigate,
      refresh: () => {
        if (here()) {
          show();
        }
      },
      report: (text) => {
        if (here()) {
          status = text;
          draw();
        }
      },
      show: (tree) => {
        if (here() && mine === generation) {
          shown = tree;
          draw();
        }
      },
    };
  }

  async function show() {
    generation += 1;
    const mine = generation;
    const at = current;
    const [, title, page] = PAGES.find(([name]) => name === at.view);
    shown = null;
    draw();
    let tree;
    try {
      tree = await page.render(context(mine, at));
    } catch (error) {
      tree = errorView(title, error);
    }
    if (mine === generation && session.isSignedIn() && current === at) {
      shown = tree;
      draw();
    }
  }

  function navigate(name, params) {
    current = { view: name, params: params || {} };
    status = "";
    show();
  }

  function onSession() {
    generation += 1;
    status = "";
    shown = null;
    const who = session.who();
    if (who === null) {
      const said = notice;
      notice = null;
      showSignIn(said);
    } else if (who.mustChange) {
      showChange(null);
    } else {
      show();
    }
  }

  session.onChange(onSession);
  mount(doc, root, h("p", {}, "Loading..."));
  restore();
  return { navigate };
}
