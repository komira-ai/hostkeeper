// The panels of the Home page's edit mode that work on sleds: Add sled (size, slots, U location),
// the panel of the sled or item picked up (Resize, Move..., Remove, Unassign) and the list of the
// unplaced items offered for an empty bay. Each choice is made from a list the layout holds, never
// typed but for a sled's label; the U locations offered are only those where the size fits. A button
// makes one operation of `ops` (the page's: one call, and the page is read again).

import { h, action, field } from "./view.js";
import {
  SLED_HEIGHTS, SLED_SLOTS, freeSledStarts, occupants, rackName, slotOf, slotOptions,
} from "./lab.js";

// A select of `[value, label]` options whose choice is kept in `state[key]`; `after` runs when it changes.
export function pick(name, state, key, options, label, after) {
  const choices = options.map(([value, text]) => h("option", { value: String(value), selected: String(state[key]) === String(value) }, text));
  const control = h("select", { name }, choices);
  control.on = {
    change: (e) => {
      state[key] = e.target.value;
      for (const choice of choices) {
        choice.attrs.selected = choice.attrs.value === String(state[key]);
      }
      if (after) {
        after();
      }
    },
  };
  return h("label", {}, label, control);
}

// Add sled, for the racks in view: the rack (when there is more than one), the size in U, the slots
// and the U location, only where that size fits in the rack chosen.
export function addSledPanel(env) {
  const { app, layout, state, ops, racks, redraw } = env;
  if (racks.length === 0) {
    return null;
  }
  const form = state.newSled;
  if (!racks.some((r) => r.id === form.rack)) {
    form.rack = racks[0].id;
  }
  const starts = freeSledStarts(layout, form.rack, Number(form.height));
  if (!starts.includes(Number(form.u))) {
    form.u = starts.length > 0 ? String(starts[0]) : "";
  }
  return h("div", { class: "buttons add-sled" },
    racks.length > 1 ? pick("new-sled-rack", form, "rack", racks.map((r) => [r.id, rackName(layout, r.id)]), "Rack", redraw) : null,
    pick("new-sled-height", form, "height", SLED_HEIGHTS.map((n) => [n, `${n}U`]), "Size", redraw),
    pick("new-sled-slots", form, "slots", SLED_SLOTS.map((n) => [n, String(n)]), "Slots", null),
    pick("new-sled-u", form, "u", starts.map((u) => [u, `U${u}`]), "U location", null),
    action(app, "Add sled", "add-sled", async () => {
      if (form.u === "") {
        return `No U of ${rackName(layout, form.rack)} has room for a sled ${form.height}U high.`;
      }
      return ops.addSled(form.rack, Number(form.height), Number(form.slots), Number(form.u));
    }, { class: "primary" }),
    starts.length === 0 ? h("span", { class: "note" }, "No room for that size in this rack.") : null);
}

// The panel of a picked sled: its address and place, what stands on it (each with Unassign), Resize
// (size, slots, label), Move... (rack and U location) and Remove.
function sledPanel(env, sled) {
  const { app, layout, state, ops, redraw } = env;
  if (state.resize === null || state.resize.id !== sled.id) {
    state.resize = { id: sled.id, height: String(sled.height), slots: String(sled.slots), label: sled.label };
    state.moving = false;
  }
  if (state.move === null || state.move.id !== sled.id) {
    state.move = { id: sled.id, rack: sled.rack, u: String(sled.u) };
  }
  const form = state.resize;
  const mv = state.move;
  const here = occupants(layout, sled.id);
  const names = here.map((p) => {
    const item = env.all.find((x) => x.key === p.key);
    return item === undefined ? p.id : item.name;
  });
  const starts = freeSledStarts(layout, mv.rack, sled.height, sled.id);
  if (!starts.includes(Number(mv.u))) {
    mv.u = starts.length > 0 ? String(starts[0]) : "";
  }
  const target = state.target !== null && state.target.kind === "sled"
    ? h("p", { class: "note", name: "target-note", role: "status" },
      `Target: ${rackName(layout, state.target.rack)}, U${state.target.u}. Enter puts it there, Escape lets go.`)
    : null;
  return h("div", { class: "moving sled-panel", role: "status" },
    h("strong", {}, sled.address),
    `${sled.label === "" ? "" : ` ${sled.label}`}, ${rackName(layout, sled.rack)}, U${sled.u}: ${sled.height}U, ${sled.slots} ${sled.slots === 1 ? "slot" : "slots"}`,
    names.length === 0 ? ", empty." : `, with ${names.join(", ")}.`,
    h("p", { class: "note" }, "Drag it to a free U, or press the up and down arrow keys and Enter."),
    target,
    h("div", { class: "buttons" },
      pick("resize-height", form, "height", SLED_HEIGHTS.map((n) => [n, `${n}U`]), "Size", null),
      pick("resize-slots", form, "slots", SLED_SLOTS.map((n) => [n, String(n)]), "Slots", null),
      field("Label", "resize-label", form, "label", { maxlength: 32 }),
      action(app, "Resize", "resize-sled", () => ops.resizeSled(sled.id, Number(form.height), Number(form.slots), form.label.trim()))),
    h("div", { class: "buttons" },
      h("button", { type: "button", name: "move-sled", "aria-pressed": state.moving ? "true" : "false", on: { click: () => { state.moving = !state.moving; redraw(); } } }, "Move..."),
      action(app, "Remove", "remove-sled", () => ops.removeSled(sled.id, names), { class: "danger" }),
      h("button", { type: "button", name: "cancel-move", on: { click: () => { state.selected = null; state.target = null; redraw(); } } }, "Cancel")),
    state.moving
      ? h("div", { class: "buttons" },
        pick("move-rack", mv, "rack", layout.racks.map((r) => [r.id, rackName(layout, r.id)]), "Rack", redraw),
        pick("move-u", mv, "u", starts.map((u) => [u, `U${u}`]), "U location", null),
        action(app, "Move here", "move-sled-go", async () => (mv.u === ""
          ? `No U of ${rackName(layout, mv.rack)} has room for ${sled.address}.`
          : ops.moveSled(sled.id, mv.rack, Number(mv.u))), { class: "primary" }))
      : null,
    here.map((p, i) => action(app, `Unassign ${names[i]}`, "unassign", () => ops.unplace(p.key))));
}

// The panel of a picked item: where it stands, Unassign, and Move... to another free slot.
function itemPanel(env, item) {
  const { app, layout, state, ops, redraw } = env;
  const held = item.placement;
  const options = slotOptions(layout, item.key, false);
  if (state.move === null || state.move.id !== item.key) {
    state.move = { id: item.key, slot: options.length > 0 ? options[0][0] : "" };
  }
  const mv = state.move;
  if (!options.some(([value]) => value === mv.slot)) {
    mv.slot = options.length > 0 ? options[0][0] : "";
  }
  const target = state.target !== null && state.target.kind === "slot"
    ? h("p", { class: "note", name: "target-note", role: "status" }, `Target: slot ${state.target.slot}. Enter puts it there, Escape lets go.`)
    : null;
  return h("div", { class: "moving item-panel", role: "status" },
    h("strong", {}, item.name),
    held === null
      ? " is not on a sled: click an empty bay, or drag it onto one."
      : ` stands at ${held.address}. Drag it to an empty bay, or press the left and right arrow keys and Enter.`,
    target,
    h("div", { class: "buttons" },
      h("button", { type: "button", name: "move-item", "aria-pressed": state.moving ? "true" : "false", on: { click: () => { state.moving = !state.moving; redraw(); } } }, "Move..."),
      held === null ? null : action(app, "Unassign", "unassign", () => ops.unplace(item.key)),
      h("button", { type: "button", name: "cancel-move", on: { click: () => { state.selected = null; state.target = null; redraw(); } } }, "Cancel")),
    state.moving
      ? h("div", { class: "buttons" },
        pick("move-slot", mv, "slot", options, "Sled slot", null),
        action(app, "Move here", "move-item-go", async () => {
          const spot = slotOf(mv.slot);
          return spot === null ? "No free slot to move it to." : ops.place(item.key, spot.sled, spot.slot);
        }, { class: "primary" }))
      : null);
}

// The panel of what is picked: a sled or an item; nothing when nothing is.
export function selectionPanel(env) {
  const sel = env.state.selected;
  if (sel === null) {
    return null;
  }
  if (sel.kind === "sled") {
    const sled = env.layout.sleds.find((x) => x.id === sel.id);
    return sled === undefined ? null : sledPanel(env, sled);
  }
  const item = env.all.find((x) => x.key === sel.id);
  return item === undefined ? null : itemPanel(env, item);
}

// An empty bay chosen: the unplaced items, a button for each that puts it on that bay.
export function assignPanel(env) {
  const { app, layout, all, state, ops, redraw } = env;
  const bay = state.assign;
  if (bay === null) {
    return null;
  }
  const sled = layout.sleds.find((x) => x.id === bay.sled);
  if (sled === undefined) {
    return null;
  }
  const unplaced = all.filter((x) => x.placement === null);
  return h("div", { class: "moving assign-panel", role: "status" },
    h("strong", {}, `${sled.address}-S${bay.slot}`),
    unplaced.length === 0
      ? " is empty, and every server and switch stands on a sled already."
      : " is empty: choose what stands on it.",
    h("div", { class: "buttons" },
      unplaced.map((item) => action(app, item.name, "assign-item", () => ops.place(item.key, bay.sled, bay.slot))),
      h("button", { type: "button", name: "cancel-assign", on: { click: () => { state.assign = null; redraw(); } } }, "Cancel")));
}
