// What the test drivers share, loaded at `test/kit.js` beside the app's modules: an API client
// over the fake gateway (`gatewayFetch`, defined in Rust), a page context that records what a page
// does, queries over the view trees pages return, and a fake document that records every element
// it makes. The assertions are in the Rust tests; this only drives and reports.

import { createApi, createCall } from "../ui/api.js";
import * as session from "../ui/session.js";
import { mount } from "../ui/view.js";

// What the data layer is given over the fake gateway, signed in as `operator` with `csrf` as the
// session's CSRF token, or signed out when it is null.
function gateway(csrf) {
  session.signOut();
  if (csrf !== null) {
    session.signIn({ username: "operator", csrf, must_change: false });
  }
  return {
    fetch: gatewayFetch,
    getCsrf: session.getCsrf,
    // What the app does with a refusal that ends the session, or that owes a new password.
    refused: (error, sent) => {
      if (error.passwordChangeRequired) {
        session.requireChange();
      } else {
        session.forget(sent);
      }
    },
  };
}

// The generated client over the data layer and the fake gateway, signed in with `csrf`.
export function api(csrf) {
  return createApi(gateway(csrf));
}

// The data layer's own `call(method, body)` alone, signed in with `csrf`: how a test sends a
// method name that no function of the client would.
export function transport(csrf) {
  return createCall(gateway(csrf));
}

// Lets every job an event started run: a request and its reply cross a few promises, and this
// yields to more of them than the longest chain the app makes. It counts turns of the job queue,
// not time.
export async function settle() {
  for (let turn = 0; turn < 60; turn++) {
    await null;
  }
}

// The time a page context reads, in unix seconds, unless a test gives another.
export const NOW = 1800000000;

// A page context over `client` that records what the page does. `answers` are the operator's
// replies to the page's questions, in order; a question beyond them is answered no. Its clock
// reads `now` (NOW when absent).
export function recorder(client, params, answers, now) {
  const seen = { reports: [], refreshes: 0, navigations: [], questions: [], shown: [], downloads: [], repeats: [] };
  const left = (answers || []).slice();
  const ctx = {
    api: client,
    params: params || {},
    confirm(question) {
      seen.questions.push(question);
      return left.shift() === true;
    },
    now() {
      return now === undefined ? NOW : now;
    },
    navigate(name, to) {
      seen.navigations.push({ name, params: to || {} });
    },
    refresh() {
      seen.refreshes += 1;
    },
    report(text) {
      seen.reports.push(text);
    },
    show(tree) {
      seen.shown.push(tree);
    },
    download(file) {
      seen.downloads.push(file);
    },
    // A repeat the test drives: nothing runs by itself, `run` is called by the test, and a call
    // of the function this returns counts a stop.
    every(run, seconds) {
      const repeat = { run, seconds, stops: 0 };
      seen.repeats.push(repeat);
      return () => {
        repeat.stops += 1;
      };
    },
  };
  return { ctx, seen };
}

// Every element of a view tree, depth first.
export function nodes(tree, out) {
  const all = out || [];
  if (tree && typeof tree.tag === "string") {
    all.push(tree);
    for (const child of tree.children) {
      nodes(child, all);
    }
  }
  return all;
}

// The elements of a view tree named `name`; at least one, or it throws.
export function named(tree, name) {
  const found = nodes(tree).filter((node) => node.attrs["name"] === name);
  if (found.length === 0) {
    throw new Error(`no element named ${name}`);
  }
  return found;
}

// The text of a view tree, its text nodes joined by a space.
export function text(tree) {
  if (typeof tree.text === "string") {
    return tree.text;
  }
  return tree.children.map(text).join(" ");
}

// Every table row of a view tree, as the texts of its cells, in order, each with its white space
// run together and trimmed.
export function rows(tree) {
  return nodes(tree)
    .filter((node) => node.tag === "tr")
    .map((row) => row.children.map((cell) => text(cell).replace(/\s+/g, " ").trim()));
}

// The value cell of the first two-cell row whose heading cell is `label`; it throws when there
// is none.
export function cell(tree, label) {
  const row = rows(tree).find((cells) => cells.length === 2 && cells[0] === label);
  if (row === undefined) {
    throw new Error(`no row ${label}`);
  }
  return row[1];
}

// Every text node of a view tree.
export function texts(tree, out) {
  const all = out || [];
  if (typeof tree.text === "string") {
    all.push(tree.text);
  } else {
    for (const child of tree.children) {
      texts(child, all);
    }
  }
  return all;
}

// Types `value` into the `index`th field named `name`.
export function type(tree, name, value, index) {
  named(tree, name)[index || 0].on.input({ target: { value } });
}

// Ticks or clears the `index`th checkbox named `name`.
export function tick(tree, name, checked, index) {
  named(tree, name)[index || 0].on.change({ target: { checked } });
}

// Chooses `value` in the `index`th select named `name`.
export function choose(tree, name, value, index) {
  named(tree, name)[index || 0].on.change({ target: { value } });
}

// The value of the choice the `index`th select named `name` shows as selected; undefined for none.
export function chosen(tree, name, index) {
  const choice = named(tree, name)[index || 0].children.find((option) => option.attrs.selected === true);
  return choice === undefined ? undefined : choice.attrs.value;
}

// The texts of the choices of the `index`th select named `name`.
export function choices(tree, name, index) {
  return named(tree, name)[index || 0].children.map((option) => text(option));
}

// Clicks the `index`th button named `name` and waits for what it started.
export async function click(tree, name, index) {
  await named(tree, name)[index || 0].on.click();
}

// A document that makes plain objects and records every element it makes.
export function fakeDocument() {
  const created = [];
  function createElement(tag) {
    const element = {
      nodeType: 1,
      tagName: tag.toUpperCase(),
      attributes: {},
      children: [],
      listeners: {},
      setAttribute(name, value) {
        this.attributes[name] = String(value);
      },
      appendChild(child) {
        this.children.push(child);
        return child;
      },
      replaceChildren(...children) {
        this.children = children;
      },
      addEventListener(event, listener) {
        this.listeners[event] = (this.listeners[event] || []).concat([listener]);
      },
    };
    created.push(element);
    return element;
  }
  return {
    created,
    createElement,
    createTextNode(data) {
      return { nodeType: 3, data };
    },
    root: createElement("div"),
  };
}

// Every element under `node` in a fake document, `node` included.
export function elements(node, out) {
  const all = out || [];
  if (node.nodeType === 1) {
    all.push(node);
    for (const child of node.children) {
      elements(child, all);
    }
  }
  return all;
}

// Every text node's data under `node` in a fake document.
export function domTexts(node, out) {
  const all = out || [];
  if (node.nodeType === 3) {
    all.push(node["data"]);
  } else {
    for (const child of node.children) {
      domTexts(child, all);
    }
  }
  return all;
}

// The element named `name` under `node` in a fake document; it throws when there is none.
export function domNamed(node, name) {
  const found = elements(node).find((element) => element.attributes["name"] === name);
  if (found === undefined) {
    throw new Error(`no element named ${name}`);
  }
  return found;
}

// Sends `event` to the listeners of `type` on a fake element.
export function fire(element, type, event) {
  for (const listener of element.listeners[type] || []) {
    listener(event);
  }
}

// Mounts a view tree in a new fake document; returns the document.
export function mounted(tree) {
  const doc = fakeDocument();
  mount(doc, doc.root, tree);
  return doc;
}
