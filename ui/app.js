// The app's frame: sign-in, the page list, sign-out and the status line.
//
// The token goes from the sign-in form to `session.js` and from there only to `api.js`, which
// sends it in the Authorization header. A reply that refuses the token forgets it, and the frame
// shows the sign-in form again. Pages are switched in memory: the address never changes, so it
// never carries the token or anything else.

import * as session from "./session.js";
import { createApi } from "./api.js";
import { h, describeError, errorView, mount } from "./view.js";
import * as fleet from "./pages/fleet.js";
import * as host from "./pages/host.js";
import * as rollout from "./pages/rollout.js";
import * as release from "./pages/release.js";
import * as holds from "./pages/holds.js";
import * as enrollment from "./pages/enrollment.js";
import * as inventory from "./pages/inventory.js";
import * as audit from "./pages/audit.js";
import * as users from "./pages/users.js";
import * as tokens from "./pages/tokens.js";

const PAGES = [
  ["fleet", "Fleet", fleet],
  ["host", "Host", host],
  ["rollout", "Rollout", rollout],
  ["release", "Release", release],
  ["holds", "Holds", holds],
  ["enrollment", "Enrollment", enrollment],
  ["inventory", "Inventory", inventory],
  ["audit", "Audit", audit],
  ["users", "Users", users],
  ["tokens", "API tokens", tokens],
];

// Starts the app in `root`. `doc` is the document, `fetch` the browser's fetch, `confirm` a
// yes-or-no question to the operator (the browser's confirm) and `now` the time in unix seconds,
// each injected so tests can fake them. Returns the frame's controls, which the browser does not
// need.
export function startApp({ doc, root, fetch, confirm, now }) {
  // Why the coordinator last refused the token, shown on the sign-in form it returns to.
  let refusal = null;
  const api = createApi({
    fetch,
    getToken: session.getToken,
    clearToken: (error, refused) => {
      if (session.getToken() === refused) {
        refusal = describeError(error);
        session.forget(refused);
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
          h("button", { type: "button", name: "sign-out", on: { click: () => session.signOut() } }, "Sign out"))),
      h("p", { class: "status", role: "status" }, status),
      h("main", {}, content));
  }

  function draw() {
    mount(doc, root, frame(shown === null ? h("p", {}, "Loading...") : shown));
  }

  function signInView(message) {
    const form = { token: "" };
    return h("form", {
      class: "sign-in",
      on: {
        submit: () => {
          const pasted = form.token;
          form.token = "";
          if (!session.signIn(pasted)) {
            mount(doc, root, signInView("Paste the operator token."));
          }
        },
      },
    },
    h("h1", {}, "hostkeeper"),
    h("p", {}, "Paste your operator token. It is kept in this tab's memory only and forgotten when the tab is reloaded or closed."),
    h("label", {}, "Operator token",
      h("input", {
        type: "password", name: "token", autocomplete: "off", spellcheck: "false",
        on: { input: (e) => { form.token = e.target.value; } },
      })),
    h("button", { type: "submit", name: "sign-in" }, "Sign in"),
    message ? h("p", { class: "error", role: "alert" }, message) : null);
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
    if (session.isSignedIn()) {
      show();
    } else {
      const said = refusal === null ? null : `The coordinator refused the token (${refusal}). Paste a valid operator token.`;
      refusal = null;
      mount(doc, root, signInView(said));
    }
  }

  session.onChange(onSession);
  onSession();
  return { navigate };
}
