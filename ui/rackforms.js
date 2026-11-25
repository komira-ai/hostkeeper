// The edit mode's controls on a rack (docs/design/rack-sleds.md): "+ Add sled" and "+ Add device" on
// each rack, each opening one form above the racks titled with what it adds and where (the rack, and the
// U or the slot); the rack's height as a stepper (one U shorter, a height from a list, one U taller) and
// a grip on its bottom edge that is dragged (or moved with the arrow keys) to make it taller or shorter.
//
// What the operator chose lives in `drafts`, which the page keeps across its refreshes, so a form keeps
// its inputs after an add: the next add of the same sled is one more click, at the next free U, and the
// next device goes to the next free slot. Errors are told in the form (or under the rack), not only in
// the status line. A shrink that would cut a sled off the top of the rack is refused before any call,
// naming the sled. Each add or resize is one call of `Layout` (`PutSled`, `Place` or `PutRack`), made
// through `env.calls` (the page's: no module outside pages/ calls the API), then the page is read again.

import { h, field } from "./view.js";
import {
  DEFAULT_RACK_HEIGHT, MAX_RACK_HEIGHT, SLED_HEIGHTS, SLED_SLOTS, byAddress, dragHeight, freeSledStarts, freeSlotsIn, layoutProblem,
  nextSledStart, rackHeightProblem, rackName, subjectOf, uSpan,
} from "./lab.js";
import { pick } from "./sledpanels.js";

// The pixels of one U when the drawn rack cannot be measured (the stylesheet's smallest is 11, its
// largest 28).
const UNIT_PX = 16;
// The longest label of a sled (the coordinator's limit).
const LABEL_MAX = 32;

// The choices of the page that outlive a refresh: the form open (`opened`, `{kind: "sled" | "device",
// rack}` or null), the inputs of each form (`sled`, `device`; `device.after` the address of the slot the
// last device went to), what the last add said (`said`) or why it failed (`error`), the refusal shown
// under a rack (`rackError`, `{rack, text}`), whether the form's first field is to take the focus once
// (`focus`), the control to take the focus back after the page is drawn again (`refocus`,
// `<name>:<rack id>`: every draw makes new elements), the rack the positions are of (`rack`), whether an
// add is on its way (`busy`), and the height of a new rack (`newRackHeight`).
export function makeDrafts() {
  return {
    opened: null,
    sled: { height: "1", slots: "1", label: "", u: "" },
    device: { item: "", slot: "", after: "" },
    rack: "",
    refocus: null,
    said: "",
    error: "",
    rackError: null,
    focus: false,
    busy: false,
    newRackHeight: String(DEFAULT_RACK_HEIGHT),
  };
}

// Opens the form of `kind` for `rackId` (a second click on the same + closes it); what the last add or
// resize said is cleared.
function openForm(env, kind, rackId) {
  const { drafts } = env;
  const same = drafts.opened !== null && drafts.opened.kind === kind && drafts.opened.rack === rackId;
  drafts.opened = same ? null : { kind, rack: rackId };
  // The inputs stay, but a position belongs to its rack: another rack's form starts at its first free U
  // and slot.
  if (!same && drafts.rack !== rackId) {
    drafts.rack = rackId;
    drafts.sled.u = "";
    drafts.device.slot = "";
    drafts.device.after = "";
  }
  drafts.refocus = same ? `${kind === "sled" ? "add-sled-open" : "add-device-open"}:${rackId}` : null;
  drafts.said = "";
  drafts.error = "";
  drafts.rackError = null;
  drafts.focus = !same;
  env.redraw();
}

// Gives `node` (a control named `name` of the rack `rackId`) the focus once it is in the page, when it is
// the control the operator used before the page was drawn again.
function keep(env, node, name, rackId) {
  if (env.drafts.refocus === `${name}:${rackId}`) {
    node.on = Object.assign({}, node.on, {
      mounted: (e) => {
        env.drafts.refocus = null;
        if (e.currentTarget && e.currentTarget.focus) {
          e.currentTarget.focus();
        }
      },
    });
  }
  return node;
}

// The two + buttons of a rack, each saying what it adds and to which rack.
export function rackAdds(env, rack) {
  const where = rackName(env.layout, rack.id);
  const button = (kind, name, words) => {
    const on = env.drafts.opened !== null && env.drafts.opened.kind === kind && env.drafts.opened.rack === rack.id;
    return keep(env, h("button", {
      type: "button", name, class: `rack-add-btn${on ? " on" : ""}`,
      "aria-label": `Add a ${kind} to ${where}`, "aria-expanded": on ? "true" : "false", ...(on ? { "aria-controls": "rack-form" } : {}),
      on: { click: () => openForm(env, kind, rack.id) },
    }, h("span", { class: "plus", "aria-hidden": "true" }, "+"), ` ${words}`), name, rack.id);
  };
  return h("div", { class: "rack-adds" },
    button("sled", "add-sled-open", "Add sled"),
    button("device", "add-device-open", "Add device"));
}

// Makes `rack` `height` U high: refused in words under the rack (and no call) when that is no height
// or would cut a sled off; else one PutRack keeping its row, and the page read again. The control named
// `control` that asked for it takes the focus back.
async function resizeRack(env, rack, height, control) {
  const { app, layout, drafts } = env;
  if (height === rack.height) {
    return;
  }
  drafts.refocus = `${control}:${rack.id}`;
  const problem = rackHeightProblem(layout, rack.id, height);
  if (problem !== null) {
    drafts.rackError = { rack: rack.id, text: problem };
    env.redraw();
    return;
  }
  try {
    await env.calls.putRack({ id: rack.id, row: rack.row, heightU: height, reason: "resize" });
  } catch (error) {
    drafts.rackError = { rack: rack.id, text: layoutProblem(error) };
    env.redraw();
    return;
  }
  drafts.rackError = null;
  app.refresh();
  app.report(`${rackName(layout, rack.id)} is now ${height}U.`);
}

// The height of a rack, set directly: one U shorter, the height from a list (1 to the limit), one U taller.
export function rackStepper(env, rack) {
  const where = rackName(env.layout, rack.id);
  const form = { height: String(rack.height) };
  const heights = Array.from({ length: MAX_RACK_HEIGHT }, (_, i) => MAX_RACK_HEIGHT - i);
  return h("div", { class: "rack-size", role: "group", "aria-label": `Height of ${where}` },
    keep(env, h("button", {
      type: "button", name: "rack-shrink", class: "step", "aria-label": `Make ${where} one U shorter`,
      disabled: rack.height <= 1, on: { click: () => resizeRack(env, rack, rack.height - 1, "rack-shrink") },
    }, "\u2212"), "rack-shrink", rack.id),
    keepSelect(env, pick("rack-height", form, "height", heights.map((n) => [n, `${n}U`]), "Height",
      () => resizeRack(env, rack, Number(form.height), "rack-height")), "rack-height", rack.id),
    keep(env, h("button", {
      type: "button", name: "rack-grow", class: "step", "aria-label": `Make ${where} one U taller`,
      disabled: rack.height >= MAX_RACK_HEIGHT, on: { click: () => resizeRack(env, rack, rack.height + 1, "rack-grow") },
    }, "+"), "rack-grow", rack.id));
}

// `keep` for the select inside the label `control` that `pick` makes.
function keepSelect(env, control, name, rackId) {
  const select = control.children.find((c) => c.tag === "select");
  if (select !== undefined) {
    keep(env, select, name, rackId);
  }
  return control;
}

// Why `rack` cannot be resized, under it, when the last try was refused.
export function rackError(env, rack) {
  const said = env.drafts.rackError;
  return said !== null && said.rack === rack.id
    ? h("p", { class: "error rack-error", role: "alert", name: "rack-size-error" }, said.text)
    : null;
}

// The grip on a rack's bottom edge: dragged down it makes the rack taller, up shorter, a U for each U
// of the drawing the pointer moved; while it moves it shows the height it would set (marked when that
// would cut a sled), and on release the rack is resized once. The arrow keys do the same a U at a time.
export function resizeGrip(env, rack) {
  const where = rackName(env.layout, rack.id);
  const grip = { from: null, unit: UNIT_PX, wanted: rack.height };
  const show = (el, to) => {
    if (el && el.querySelector) {
      const label = el.querySelector(".grip-u");
      if (label !== null) {
        label.textContent = `${to}U`;
      }
    }
    if (el && el.classList) {
      el.classList.toggle("cut", rackHeightProblem(env.layout, rack.id, to) !== null);
      el.classList.toggle("dragging", grip.from !== null);
    }
  };
  return keep(env, h("button", {
    type: "button", name: "rack-resize-grip", class: "rack-grip",
    title: `Drag to resize ${where}`,
    "aria-label": `Resize ${where}, now ${rack.height}U: drag the bottom edge, or press the down arrow for one U more and the up arrow for one U less`,
    on: {
      pointerdown: (e) => {
        const el = e.currentTarget;
        const chassis = el && el.closest ? el.closest(".rack") : null;
        const drawn = chassis && chassis.querySelector ? chassis.querySelector(".chassis") : null;
        grip.unit = drawn && drawn.offsetHeight > 0 ? drawn.offsetHeight / rack.height : UNIT_PX;
        grip.from = e.clientY;
        grip.wanted = rack.height;
        if (el && el.setPointerCapture && e.pointerId !== undefined) {
          el.setPointerCapture(e.pointerId);
        }
        show(el, grip.wanted);
      },
      pointermove: (e) => {
        if (grip.from === null) {
          return;
        }
        grip.wanted = dragHeight(rack.height, e.clientY - grip.from, grip.unit);
        show(e.currentTarget, grip.wanted);
      },
      pointerup: (e) => {
        if (grip.from === null) {
          return undefined;
        }
        grip.from = null;
        show(e.currentTarget, grip.wanted);
        return resizeRack(env, rack, grip.wanted, "rack-resize-grip");
      },
      keydown: (e) => {
        const by = e.key === "ArrowDown" ? 1 : (e.key === "ArrowUp" ? -1 : 0);
        if (by === 0) {
          return undefined;
        }
        e.preventDefault();
        return resizeRack(env, rack, dragHeight(rack.height, by, 1), "rack-resize-grip");
      },
    },
  }, h("span", { class: "grip-bar", "aria-hidden": "true" }), h("span", { class: "grip-u", "aria-hidden": "true" }, `${rack.height}U`)),
  "rack-resize-grip", rack.id);
}

// What a form's add does: `run()` makes the one call and returns what to say, or throws; while it runs a
// second click is ignored. The control `control` (`<name>:<rack id>`) takes the focus back. Success keeps the inputs, says what was added, and reads the page again; a
// refusal is told in the form and the page stays as it is.
async function submit(env, run, control) {
  const { app, drafts } = env;
  if (drafts.busy) {
    return;
  }
  drafts.busy = true;
  drafts.focus = false;
  drafts.refocus = control;
  let said;
  try {
    said = await run();
  } catch (error) {
    drafts.error = layoutProblem(error);
    drafts.said = "";
    env.redraw();
    return;
  } finally {
    drafts.busy = false;
  }
  if (said.error !== undefined) {
    drafts.error = said.error;
    drafts.said = "";
    env.redraw();
    return;
  }
  drafts.error = "";
  drafts.said = said.done;
  app.refresh();
  // The page's own status line announces it too (the form's line is drawn anew with its text).
  app.report(said.done);
}

// A form's frame: its title, the preview line, the fields, its buttons and what the last add said.
function formFrame(env, title, preview, fields, button, note) {
  const { drafts } = env;
  return h("form", { class: "rack-form", id: "rack-form", "aria-label": title, on: { submit: () => button.on.click() } },
    h("h3", { name: "rack-form-title" }, title),
    preview === null ? null : h("p", { class: "preview", name: "rack-form-preview" }, preview),
    h("div", { class: "fields" }, fields),
    note,
    drafts.error === "" ? null : h("p", { class: "error", role: "alert", name: "rack-form-error" }, drafts.error),
    h("div", { class: "buttons" }, button,
      h("button", {
        type: "button", name: "close-rack-form",
        on: {
          click: () => {
            drafts.refocus = `${drafts.opened.kind === "sled" ? "add-sled-open" : "add-device-open"}:${drafts.opened.rack}`;
            drafts.opened = null;
            drafts.said = "";
            drafts.error = "";
            env.redraw();
          },
        },
      }, "Close")),
    h("p", { class: "said", role: "status", name: "rack-form-said" }, drafts.said));
}

// Makes the first control of the form take the focus once, when it was just opened (and no control
// the operator used since is to take it back).
function focusFirst(env, control) {
  if (env.drafts.focus && env.drafts.refocus === null) {
    const select = control.children.find((c) => c.tag === "select" || c.tag === "input");
    if (select !== undefined) {
      select.on.mounted = (e) => {
        env.drafts.focus = false;
        if (e.currentTarget && e.currentTarget.focus) {
          e.currentTarget.focus();
        }
      };
    }
  }
  return control;
}

// Add a sled to `rack`: Height (U), Slots, an optional Label and the Position (only the lowest U where
// that height fits); the title and the preview say where it goes, and the rack draws its outline there.
function sledForm(env, rack) {
  const { app, layout, drafts } = env;
  const form = drafts.sled;
  const height = Number(form.height);
  const starts = freeSledStarts(layout, rack.id, height);
  if (!starts.includes(Number(form.u))) {
    const next = nextSledStart(layout, rack.id, height, Number(form.u) || 1);
    form.u = next === null ? "" : String(next);
  }
  const where = rackName(layout, rack.id);
  const u = Number(form.u);
  const slots = Number(form.slots);
  const title = form.u === "" ? `Add a sled to ${where}` : `Add a sled to ${where} at ${uSpan(u, height)}`;
  const preview = form.u === ""
    ? null
    : `It takes ${uSpan(u, height)} of ${where}: ${height}U, ${slots} ${slots === 1 ? "slot" : "slots"}. The outline in the rack shows where.`;
  // A choice clears what the last add said and draws the form again, the select keeping the focus.
  const redraw = (name) => () => {
    drafts.said = "";
    drafts.error = "";
    drafts.focus = false;
    drafts.refocus = `${name}:${rack.id}`;
    env.redraw();
  };
  const add = keep(env, h("button", {
    type: "button", name: "add-sled", class: "primary",
    on: {
      // The inputs are read when it is clicked, not when the form was drawn (a choice draws the form
      // again, which moves the position to one where the height fits; the label is typed with no redraw).
      click: () => submit(env, async () => {
        const label = form.label.trim();
        const [height, slots, u] = [Number(form.height), Number(form.slots), Number(form.u)];
        if (form.u === "") {
          return { error: `No U of ${where} has room for a sled ${height}U high: pick a smaller height, or make the rack taller.` };
        }
        if (label.length > LABEL_MAX) {
          return { error: `A label is at most ${LABEL_MAX} characters.` };
        }
        await env.calls.putSled({ id: "", rack: rack.id, u, heightU: height, slots, label, reason: "add sled" });
        // The next add of the same sled goes just above this one (the next free U from there, read
        // when the page is drawn again).
        form.u = String(u + height);
        return { done: `Added a ${height}U sled with ${slots} ${slots === 1 ? "slot" : "slots"} at ${where}, ${uSpan(u, height)}.` };
      }, `add-sled:${rack.id}`),
    },
  }, "Add sled"), "add-sled", rack.id);
  const fields = [
    focusFirst(env, keepSelect(env, pick("new-sled-height", form, "height", SLED_HEIGHTS.map((n) => [n, `${n}U`]), "Height (U)", redraw("new-sled-height")), "new-sled-height", rack.id)),
    keepSelect(env, pick("new-sled-slots", form, "slots", SLED_SLOTS.map((n) => [n, String(n)]), "Slots", redraw("new-sled-slots")), "new-sled-slots", rack.id),
    field("Label (optional)", "new-sled-label", form, "label", { maxlength: LABEL_MAX, placeholder: "Mac Studio" }),
    keepSelect(env, pick("new-sled-u", form, "u", starts.map((s) => [s, uSpan(s, height)]), "Position (lowest U)", redraw("new-sled-u")), "new-sled-u", rack.id),
  ];
  const none = form.u === ""
    ? h("p", { class: "error", role: "alert", name: "rack-form-room" }, `No room for a ${height}U sled in ${where}: pick a smaller height, or make the rack taller.`)
    : null;
  return formFrame(env, title, preview, fields, add, none);
}

// Add a device to `rack`: the server or switch (from those on no sled) and the free slot of a sled of
// the rack it goes on; the title and the preview name the slot, and the rack marks its bay. A rack with
// no free slot offers to add a sled instead; with nothing to place, says so and where a new switch is
// added.
function deviceForm(env, rack) {
  const { app, layout, all, drafts } = env;
  const form = drafts.device;
  const waiting = all.filter((x) => x.placement === null);
  const slots = freeSlotsIn(layout, rack.id);
  if (!waiting.some((x) => x.key === form.item)) {
    form.item = waiting.length > 0 ? waiting[0].key : "";
  }
  if (!slots.some((s) => `${s.sled}:${s.slot}` === form.slot)) {
    const after = form.after === "" ? undefined : slots.find((s) => byAddress(s, { address: form.after }) > 0);
    const chosen = after !== undefined ? after : slots[0];
    form.slot = chosen === undefined ? "" : `${chosen.sled}:${chosen.slot}`;
  }
  const where = rackName(layout, rack.id);
  const item = waiting.find((x) => x.key === form.item);
  const spot = slots.find((s) => `${s.sled}:${s.slot}` === form.slot);
  const kind = (x) => (x.device === undefined ? "server" : "switch");
  const title = spot === undefined ? `Add a device to ${where}` : `Add a device to ${where} on ${spot.address}`;
  const preview = item === undefined || spot === undefined
    ? null
    : `${item.name} goes on slot ${spot.slot} of the sled at ${uSpan(spot.u, spot.height)}: ${spot.address}. The bay is marked in the rack.`;
  // A choice clears what the last add said and draws the form again, the select keeping the focus.
  const redraw = (name) => () => {
    drafts.said = "";
    drafts.error = "";
    drafts.focus = false;
    drafts.refocus = `${name}:${rack.id}`;
    env.redraw();
  };
  const add = keep(env, h("button", {
    type: "button", name: "add-device", class: "primary",
    on: {
      // The choices are read when it is clicked, not when the form was drawn.
      click: () => submit(env, async () => {
        const item = waiting.find((x) => x.key === form.item);
        const spot = slots.find((s) => `${s.sled}:${s.slot}` === form.slot);
        if (item === undefined) {
          return { error: "Nothing waits to be placed: every server and switch stands on a sled." };
        }
        if (spot === undefined) {
          return { error: `${where} has no free slot: add a sled first.` };
        }
        await env.calls.place({ subject: subjectOf(item), sled: spot.sled, slot: spot.slot, reason: `placed ${item.name}` });
        // The next device goes to the next free slot after this one.
        form.after = spot.address;
        form.slot = "";
        return { done: `Added ${item.name} to ${where} at ${spot.address}.` };
      }, `add-device:${rack.id}`),
    },
  }, "Add device"), "add-device", rack.id);
  const fields = [
    focusFirst(env, keepSelect(env, pick("new-device-item", form, "item", waiting.map((s) => [s.key, `${s.name} (${kind(s)})`]), "Server or switch", redraw("new-device-item")), "new-device-item", rack.id)),
    keepSelect(env, pick("new-device-slot", form, "slot", slots.map((s) => [`${s.sled}:${s.slot}`, `${s.address} (${uSpan(s.u, s.height)})`]), "Slot", redraw("new-device-slot")), "new-device-slot", rack.id),
  ];
  const notes = [];
  if (waiting.length === 0) {
    notes.push(h("p", { class: "note", name: "rack-form-nothing" }, "Every server and switch already stands on a sled. A new switch is added in Inventory.",
      " ", h("button", { type: "button", class: "linklike", name: "to-inventory", on: { click: () => app.navigate("inventory", { kind: "switch" }) } }, "Add a switch")));
  }
  if (slots.length === 0) {
    notes.push(h("p", { class: "error", role: "alert", name: "rack-form-room" }, `${where} has no free slot: add a sled first.`,
      " ", h("button", { type: "button", class: "linklike", name: "to-sled-form", on: { click: () => openForm(env, "sled", rack.id) } }, "Add a sled instead")));
  }
  return formFrame(env, title, preview, fields, add, notes);
}

// The form open for one of the racks `racks` in view, or nothing (a form whose rack is not in view is
// not shown).
export function rackForm(env, racks) {
  const opened = env.drafts.opened;
  const rack = opened === null ? undefined : racks.find((r) => r.id === opened.rack);
  if (rack === undefined) {
    return null;
  }
  return opened.kind === "sled" ? sledForm(env, rack) : deviceForm(env, rack);
}

// What the open form shows in the rack before anything is added: `{kind: "sled", rack, u, height}`
// for the outline of the sled, `{kind: "slot", sled, slot}` for the bay of the device; null when no
// form is open or it has nowhere to put it.
export function formPreview(drafts, racks) {
  const opened = drafts.opened;
  if (opened === null || !racks.some((r) => r.id === opened.rack)) {
    return null;
  }
  if (opened.kind === "sled") {
    return drafts.sled.u === "" ? null : { kind: "sled", rack: opened.rack, u: Number(drafts.sled.u), height: Number(drafts.sled.height) };
  }
  const match = /^(.+):(\d+)$/.exec(drafts.device.slot);
  return match === null ? null : { kind: "slot", sled: match[1], slot: Number(match[2]) };
}
