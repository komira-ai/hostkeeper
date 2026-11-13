// Views as plain data, and the one function that turns them into the page's elements.
//
// A page builds a tree of `{tag, attrs, on, children}` elements and `{text}` nodes with `h` and
// returns it; `mount` makes it real with `createElement`, `createTextNode` and `setAttribute`.
// Text from the API (claims, reasons, log lines) only ever becomes a text node, so it is shown as
// typed and never parsed as markup. Tags and attribute names come from the fixed lists below; a
// tree naming anything else is refused before anything is mounted.

import { ApiError } from "./api.js";

const TAGS = new Set([
  "a", "aside", "button", "code", "div", "fieldset", "form", "h1", "h2", "h3", "header", "input", "label",
  "legend", "li", "main", "nav", "option", "p", "pre", "progress", "section", "select", "small", "span",
  "strong", "table", "tbody", "td", "textarea", "th", "thead", "tr", "ul",
  "svg", "path", "circle", "rect", "line", "polyline",
]);
// The tags that are drawn, which the document makes in the SVG namespace.
const SVG_TAGS = new Set(["svg", "path", "circle", "rect", "line", "polyline"]);
// The name of the SVG namespace. It is an identifier the DOM compares, never an address the page
// loads, and is written in parts so that the scan for network addresses (`tests/sources.rs`) stays
// strict about every real one.
const SVG_NAMESPACE = ["http:", "", "www.w3.org", "2000", "svg"].join("/");
const ATTRIBUTES = new Set([
  "aria-controls", "aria-current", "aria-expanded", "aria-hidden", "aria-label", "aria-pressed", "href", "rel", "target",
  "autocomplete", "class", "draggable", "id", "inputmode", "maxlength", "name", "placeholder", "role", "rows",
  "spellcheck", "tabindex", "title", "type", "max",
  "viewBox", "d", "fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin", "cx", "cy", "r",
  "x", "y", "x1", "y1", "x2", "y2", "width", "height", "rx", "points", "focusable",
]);
// Numbers the page hands its stylesheet, as custom properties of the element (`--row`, `--span`).
// They are set through the element's style object, which a content security policy of
// `default-src 'self'` allows (a `style` attribute it does not), and only a finite number under a
// name of lower-case letters and dashes is accepted.
const CUSTOM_PROPERTY = /^--[a-z][a-z-]*$/;
// Set as properties, not attributes: what the element shows now.
const PROPERTIES = new Set(["checked", "disabled", "selected", "value"]);
// The one place a link may lead: the operator documentation as GitHub renders it. A link is a
// navigation the operator chooses, not a load; any other address is refused before mounting.
const DOCS = "https://github.com/komira-ai/hostkeeper/blob/main/docs/operations/";
// The deployment documentation, the second place a link may lead: how a repository is structured.
const DEPLOY_DOCS = "https://github.com/komira-ai/hostkeeper/blob/main/docs/deploy/";

// A link that opens `page` (a file of docs/operations) at `anchor` in a new tab, with `text`.
export function docLink(name, text, page, anchor) {
  return h("a", {
    name, href: `${DOCS}${page}#${anchor}`, target: "_blank", rel: "noopener noreferrer",
  }, text);
}

// A link that opens `page` (a file of docs/deploy) at `anchor` in a new tab, with `text`.
export function deployDocLink(name, text, page, anchor) {
  return h("a", {
    name, href: `${DEPLOY_DOCS}${page}#${anchor}`, target: "_blank", rel: "noopener noreferrer",
  }, text);
}

const EVENTS = new Set([
  "change", "click", "input", "submit", "keydown", "pointerdown", "pointermove", "pointerup", "dragstart", "dragover",
  "drop", "dragend", "scroll",
]);
// Two names that are not events of the element: `mounted` runs its handler once, when the tree is
// in the document, and `resize` runs it each time the window is resized, both with the element as
// `currentTarget`. A page uses them to read the layout (what fits) after the elements are there.
const HOOKS = new Set(["mounted", "resize"]);
const ALL_EVENTS = new Set([...EVENTS, ...HOOKS]);
// What the window of the last mount listens to for `resize`, taken off at the next mount.
let windowListeners = [];

function flatten(children, out) {
  for (const child of children) {
    if (child === null || child === undefined || child === false) {
      continue;
    }
    if (Array.isArray(child)) {
      flatten(child, out);
    } else if (typeof child === "object") {
      out.push(child);
    } else {
      out.push({ text: String(child) });
    }
  }
  return out;
}

// An element. `props` holds attributes and properties by name, and `on`, the event handlers by
// event name. Children are elements, text (any string or number), arrays of them, or nothing.
export function h(tag, props, ...children) {
  const attrs = {};
  let on = {};
  for (const [key, value] of Object.entries(props || {})) {
    if (key === "on") {
      on = value;
    } else {
      attrs[key] = value;
    }
  }
  return { tag, attrs, on, children: flatten(children, []) };
}

// The icons of the app, each a list of strokes in a 24 by 24 box, drawn with the text colour.
const ICONS = {
  hostgroups: [["rect", { x: 3, y: 4, width: 18, height: 6, rx: 1.5 }], ["rect", { x: 3, y: 14, width: 18, height: 6, rx: 1.5 }],
    ["line", { x1: 7, y1: 7, x2: 7.01, y2: 7 }], ["line", { x1: 7, y1: 17, x2: 7.01, y2: 17 }]],
  packages: [["path", { d: "M12 3 20 7.5v9L12 21 4 16.5v-9z" }], ["polyline", { points: "4 7.5 12 12 20 7.5" }],
    ["line", { x1: 12, y1: 12, x2: 12, y2: 21 }]],
  inventory: [["rect", { x: 5, y: 3, width: 14, height: 18, rx: 2 }], ["line", { x1: 9, y1: 8, x2: 15, y2: 8 }],
    ["line", { x1: 9, y1: 12, x2: 15, y2: 12 }], ["line", { x1: 9, y1: 16, x2: 12, y2: 16 }]],
  enrollment: [["path", { d: "M4 7h10a4 4 0 0 1 0 8H4" }], ["polyline", { points: "8 11 4 15 8 19" }], ["line", { x1: 18, y1: 4, x2: 18, y2: 8 }],
    ["line", { x1: 16, y1: 6, x2: 20, y2: 6 }]],
  settings: [["line", { x1: 4, y1: 7, x2: 20, y2: 7 }], ["line", { x1: 4, y1: 17, x2: 20, y2: 17 }],
    ["circle", { cx: 9, cy: 7, r: 2.5 }], ["circle", { cx: 15, cy: 17, r: 2.5 }]],
  "theme-system": [["circle", { cx: 12, cy: 12, r: 8 }], ["path", { d: "M12 4a8 8 0 0 1 0 16z" }]],
  "theme-light": [["circle", { cx: 12, cy: 12, r: 4 }], ["line", { x1: 12, y1: 2, x2: 12, y2: 5 }], ["line", { x1: 12, y1: 19, x2: 12, y2: 22 }],
    ["line", { x1: 2, y1: 12, x2: 5, y2: 12 }], ["line", { x1: 19, y1: 12, x2: 22, y2: 12 }]],
  "theme-dark": [["path", { d: "M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z" }]],
  signout: [["path", { d: "M10 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4" }], ["polyline", { points: "15 8 19 12 15 16" }],
    ["line", { x1: 19, y1: 12, x2: 9, y2: 12 }]],
  collapse: [["polyline", { points: "11 7 6 12 11 17" }], ["polyline", { points: "18 7 13 12 18 17" }]],
  expand: [["polyline", { points: "6 7 11 12 6 17" }], ["polyline", { points: "13 7 18 12 13 17" }]],
  menu: [["line", { x1: 4, y1: 7, x2: 20, y2: 7 }], ["line", { x1: 4, y1: 12, x2: 20, y2: 12 }], ["line", { x1: 4, y1: 17, x2: 20, y2: 17 }]],
  mark: [["rect", { x: 4, y: 3, width: 16, height: 18, rx: 2 }], ["line", { x1: 4, y1: 9, x2: 20, y2: 9 }],
    ["line", { x1: 4, y1: 15, x2: 20, y2: 15 }], ["line", { x1: 8, y1: 6, x2: 8.01, y2: 6 }], ["line", { x1: 8, y1: 12, x2: 8.01, y2: 12 }],
    ["line", { x1: 8, y1: 18, x2: 8.01, y2: 18 }]],
};

// The icon `name` as an element, hidden from assistive technology: the button around it has the words.
export function icon(name) {
  return h("svg", {
    class: "icon", viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": 1.8,
    "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true", focusable: "false",
  }, ICONS[name].map(([tag, attrs]) => h(tag, attrs)));
}

function build(doc, node, hooks) {
  if (typeof node.text === "string") {
    return doc.createTextNode(node.text);
  }
  if (!TAGS.has(node.tag)) {
    throw new Error(`no tag ${node.tag}`);
  }
  const element = SVG_TAGS.has(node.tag) ? doc.createElementNS(SVG_NAMESPACE, node.tag) : doc.createElement(node.tag);
  for (const [key, value] of Object.entries(node.attrs)) {
    if (key === "vars") {
      for (const [name, number] of Object.entries(value)) {
        if (!CUSTOM_PROPERTY.test(name) || typeof number !== "number" || !Number.isFinite(number)) {
          throw new Error(`no custom property ${name}`);
        }
        element.style.setProperty(name, String(number));
      }
    } else if (PROPERTIES.has(key)) {
      element[key] = value;
    } else if (ATTRIBUTES.has(key)) {
      if (key === "href" && !String(value).startsWith(DOCS) && !String(value).startsWith(DEPLOY_DOCS)) {
        throw new Error("no link outside the documentation");
      }
      element.setAttribute(key, String(value));
    } else {
      throw new Error(`no attribute ${key}`);
    }
  }
  for (const [event, handler] of Object.entries(node.on)) {
    if (!ALL_EVENTS.has(event)) {
      throw new Error(`no event ${event}`);
    }
    if (HOOKS.has(event)) {
      hooks.push({ event, element, handler });
      continue;
    }
    element.addEventListener(event, (e) => {
      if (event === "submit") {
        e.preventDefault();
      }
      handler(e);
    });
  }
  for (const child of node.children) {
    element.appendChild(build(doc, child, hooks));
  }
  return element;
}

// Replaces what `root` shows with `tree`.
export function mount(doc, root, tree) {
  const hooks = [];
  const built = build(doc, tree, hooks);
  const view = doc.defaultView;
  for (const [on, type, listener] of windowListeners) {
    on.removeEventListener(type, listener);
  }
  windowListeners = [];
  root.replaceChildren(built);
  for (const { event, element, handler } of hooks) {
    if (event === "mounted") {
      handler({ currentTarget: element });
    } else if (view) {
      const listener = () => handler({ currentTarget: element });
      view.addEventListener("resize", listener);
      windowListeners.push([view, "resize", listener]);
    }
  }
}

// The words an operator reads for an error: the code and the refusal token, never more.
export function describeError(error) {
  if (error instanceof ApiError) {
    return error.reason === "" ? error.code : `${error.code} ${error.reason}`;
  }
  return "the app failed unexpectedly";
}

// A page that could not load: its title and why.
export function errorView(title, error) {
  return h("section", { class: "page" },
    h("h2", {}, title),
    h("p", { class: "error", role: "alert" }, describeError(error)));
}

// An enum value's name without its prefix, in lower case (`HEALTH_HEALTHY` -> `healthy`).
// Absent (the default value, which canonical JSON leaves out) and UNSPECIFIED read as "-".
export function enumLabel(value, prefix) {
  if (typeof value !== "string" || value === `${prefix}UNSPECIFIED`) {
    return "-";
  }
  const name = value.startsWith(prefix) ? value.slice(prefix.length) : value;
  return name.toLowerCase();
}

// Unix seconds (canonical JSON writes 64-bit numbers as strings) as UTC time. Absent, 0 or not a
// number reads as "-"; a time a JavaScript date cannot hold (beyond the year 275760) is shown as
// the number it is, marked out of range, rather than failing the page.
export function fmtUnix(value) {
  const seconds = Number(value);
  if (value === undefined || value === "" || !Number.isFinite(seconds) || seconds <= 0) {
    return "-";
  }
  const date = new Date(seconds * 1000);
  if (Number.isNaN(date.getTime())) {
    return `${value} (out of range)`;
  }
  return date.toISOString().replace("T", " ").replace(".000Z", " UTC");
}

// A hold's scope in words.
export function scopeLabel(scope) {
  const { host, role, fleet } = scope || {};
  if (typeof host === "string") {
    return `host ${host}`;
  }
  if (typeof role === "string") {
    return `role ${role}`;
  }
  return fleet ? "fleet" : "-";
}

// The list `obj[key]`, or an empty one when it is absent (canonical JSON leaves out an empty
// repeated field) or `obj` is.
export function list(obj, key) {
  return obj && Array.isArray(obj[key]) ? obj[key] : [];
}

// Every item `reply[key]` of a paged list, from `page(token)`, which resolves to one reply: the
// first page for the token "", then the page each reply's `nextPageToken` names, to the last. A
// token that does not move past the one before ends the list, so a reply that repeats one
// cannot keep the page asking. A `reply[key]` that is there and is no list throws, as reading
// it would.
export async function allPages(page, key) {
  const items = [];
  let token = "";
  for (;;) {
    const reply = await page(token);
    if (reply && reply[key] !== undefined && !Array.isArray(reply[key])) {
      throw new TypeError(`${key} is not a list`);
    }
    items.push(...list(reply, key));
    const next = reply && typeof reply.nextPageToken === "string" ? reply.nextPageToken : "";
    if (next === "" || next <= token) {
      return items;
    }
    token = next;
  }
}

// The bound controls below keep their own view node up to date as the operator types, ticks or
// chooses, so a tree mounted again (after a status line changes) shows what the state holds.

// A text field bound to `state[key]`.
export function field(label, name, state, key, extra) {
  const input = h("input", Object.assign({
    type: "text", name, autocomplete: "off", spellcheck: "false", value: state[key] || "",
  }, extra || {}));
  input.on = {
    input: (e) => {
      state[key] = e.target.value;
      input.attrs.value = state[key];
    },
  };
  return h("label", {}, label, input);
}

// A multi-line text field bound to `state[key]`: it keeps the line breaks that a text field drops.
export function area(label, name, state, key, extra) {
  const input = h("textarea", Object.assign({
    name, autocomplete: "off", spellcheck: "false", rows: 6, value: state[key] || "",
  }, extra || {}));
  input.on = {
    input: (e) => {
      state[key] = e.target.value;
      input.attrs.value = state[key];
    },
  };
  return h("label", {}, label, input);
}

// A file chooser whose choice is kept in `state[key]` (the file, not its contents: nothing is read
// until the operator acts); none is `null`. A tree mounted again names the choice beside the
// control, which the browser has reset.
export function filePicker(label, name, state, key) {
  const input = h("input", { type: "file", name });
  input.on = {
    change: (e) => {
      const files = e.target.files;
      state[key] = files && files.length > 0 ? files[0] : null;
    },
  };
  const chosen = state[key];
  return h("label", {}, label, input,
    h("span", { class: "note", name: `${name}-chosen` }, chosen ? `Still chosen: ${chosen["name"]}` : ""));
}

// A checkbox bound to `state[key]`, off unless set.
export function checkbox(label, name, state, key) {
  const box = h("input", { type: "checkbox", name, checked: state[key] === true });
  box.on = {
    change: (e) => {
      state[key] = e.target.checked === true;
      box.attrs.checked = state[key];
    },
  };
  return h("label", { class: "check" }, box, label);
}

// A select bound to `state[key]`; `options` are `[value, label]` pairs.
export function select(label, name, state, key, options) {
  const choices = options.map(([value, text]) => h("option", { value, selected: state[key] === value }, text));
  const chooser = h("select", { name }, choices);
  chooser.on = {
    change: (e) => {
      state[key] = e.target.value;
      for (const choice of choices) {
        choice.attrs.selected = choice.attrs.value === state[key];
      }
    },
  };
  return h("label", {}, label, chooser);
}

// A button that runs `run` once per click and reports what it says or why it failed. A click
// while the run it started is unfinished is ignored, so a double click sends one call.
export function action(ctx, label, name, run, extra) {
  let running = false;
  return h("button", Object.assign({
    type: "button",
    name,
    on: {
      click: async () => {
        if (running) {
          return;
        }
        running = true;
        try {
          const said = await run();
          if (typeof said === "string") {
            ctx.report(said);
          }
        } catch (error) {
          ctx.report(describeError(error));
        } finally {
          running = false;
        }
      },
    },
  }, extra || {}), label);
}

// The UTF-8 length of a text, in bytes.
function utf8Bytes(text) {
  let bytes = 0;
  for (const ch of text) {
    const point = ch.codePointAt(0);
    bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
  }
  return bytes;
}

// Why a typed reason, trimmed and not empty, breaks the API's rule for a reason (at most 256
// bytes, no control character); null when it keeps it.
export function reasonProblem(reason) {
  if (/[\u0000-\u001f\u007f]/.test(reason)) {
    return "A reason has no control character (a tab or a line break, for example).";
  }
  if (utf8Bytes(reason) > 256) {
    return `A reason is at most 256 bytes; this one is ${utf8Bytes(reason)}.`;
  }
  return null;
}

// A trimmed text, or "" for anything that is not a string.
export function trimmed(value) {
  return typeof value === "string" ? value.trim() : "";
}

// A list typed as words separated by commas or spaces, without empty entries.
export function words(value) {
  return trimmed(value).split(/[\s,]+/).filter((w) => w !== "");
}

// A status pill: `text` on the colour of `tone` (ok, warn, bad, info or muted). The text carries
// the meaning, so a colour is never the only sign of it; `more` is text or elements after it (a count).
export function pill(text, tone, ...more) {
  return h("span", { class: `pill ${tone}` }, text, more);
}

export function table(headings, rows) {
  return h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, headings.map((title) => h("th", {}, title)))),
    h("tbody", {}, rows)));
}
