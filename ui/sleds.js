// The sleds of a rack, drawn: the front elevation of a rack as a column of U with each sled over its
// U range (docs/design/rack-sleds.md). A sled with one slot and an item on it is drawn as that item's
// faceplate (a server's bezel and light, a switch's ports) with no tray to be seen; any other sled is
// a tray with a bay for each slot: a small box for an item (a Mac Studio, a NUC or a plain box, chosen
// by the item's name), a gap for an empty bay. The edit mode makes the same drawing draggable: a sled
// is dragged to a free U of any rack in view, an item to an empty bay, and each of them is also picked
// by a click (aria-pressed) and moved with the arrow keys, Enter putting it down.
//
// This module only draws and decides where a drop fits; every change of the layout is an operation
// `ops` the page gives it (`moveSled(sledId, rackId, u)`, `place(key, sledId, slot)`, `refuse(words)`),
// which makes the one call, tells a refusal in words and reads the layout again.

import { h } from "./view.js";
import { consoleWords, dropStart, fitsAt, occupants, rackName, roleWords, sledWords, sledsIn } from "./lab.js";

// The look of the box of an item on a tray, by its name: "studio" is a Mac Studio, "nuc" a NUC,
// anything else a plain small box. Cosmetic: it is never a record of the layout.
export function boxLook(name) {
  if (/studio/i.test(name)) {
    return "studio";
  }
  return /nuc/i.test(name) ? "nuc" : "generic";
}

// What a server's tooltip says: its name, group, version and state, then its address when it stands
// on a sled.
export function tip(server) {
  const parts = [server.name];
  if (server.group !== "") {
    parts.push(`group ${server.group}`);
  }
  if (server.version !== "") {
    parts.push(`version ${server.version}`);
  }
  parts.push(server.state);
  if (server.placement !== null && server.placement.address !== "") {
    parts.push(server.placement.address);
  }
  return parts.join(" | ");
}

// A switch's tooltip: its name, network and console port, then its address.
function gearTip(item) {
  const parts = [item.device.name, roleWords(item.device.role), `console ${consoleWords(item.device)}`];
  if (item.placement !== null && item.placement.address !== "") {
    parts.push(item.placement.address);
  }
  return parts.join(" | ");
}

// The first row (from the top of the rack) of a sled and how many it spans: the stylesheet's grid.
function span(rack, sled) {
  return { "--r": rack.height - (sled.u + sled.height - 1) + 1, "--s": sled.height };
}

// The board of one rendering of the lab: `env` is `{layout, all, edit, state, ops, go, redraw}`,
// `all` the servers and switches joined with their places, `state` what survives a redraw
// (`selected`, `target`, `drag`, `assign`, `ghosts`), `go(view, params)` the navigation.
export function board(env) {
  const { layout, all, edit, state, ops } = env;
  const items = new Map();
  for (const item of all) {
    if (item.placement !== null) {
      items.set(`${item.placement.sled}:${item.placement.slot}`, item);
    }
  }
  const itemAt = (sled, slot) => items.get(`${sled.id}:${slot}`);
  const sledOf = (id) => layout.sleds.find((s) => s.id === id);
  const picked = (kind, id) => state.selected !== null && state.selected.kind === kind && state.selected.id === id;
  const rackOf = (id) => layout.racks.find((r) => r.id === id);

  // The outline of where a dragged sled would stand, one for each rack: `paint` sets what it shows on
  // the view tree and, once the tree is in the page, on its element, so the drag is not interrupted
  // by drawing the page again.
  function ghostOf(rack) {
    let g = state.ghosts.get(rack.id);
    if (g === undefined) {
      g = { node: null, el: null };
      state.ghosts.set(rack.id, g);
    }
    return g;
  }
  function paint(rack, spot) {
    const g = ghostOf(rack);
    const cls = spot === null ? "drop-ghost" : `drop-ghost on${spot.fits ? "" : " no"}`;
    const vars = spot === null
      ? { "--r": 1, "--s": 1 }
      : span(rack, { u: spot.start, height: spot.height });
    if (g.node !== null) {
      g.node.attrs.class = cls;
      g.node.attrs.vars = vars;
    }
    if (g.el !== null && g.el.setAttribute) {
      g.el.setAttribute("class", cls);
      for (const [name, value] of Object.entries(vars)) {
        g.el.style.setProperty(name, String(value));
      }
    }
  }
  function ghostNode(rack) {
    const g = ghostOf(rack);
    g.el = null;
    const t = state.target;
    const spot = t !== null && t.kind === "sled" && t.rack === rack.id
      ? { start: t.u, height: sledOf(state.selected.id).height, fits: fitsAt(layout, rack.id, sledOf(state.selected.id).height, t.u, state.selected.id) }
      : null;
    g.node = h("span", {
      class: "drop-ghost", "aria-hidden": "true", vars: { "--r": 1, "--s": 1 },
      on: { mounted: (e) => { g.el = e.currentTarget; } },
    });
    paint(rack, spot);
    return g.node;
  }
  function clearDrag() {
    state.drag = null;
    for (const [id, g] of state.ghosts) {
      const rack = rackOf(id);
      if (rack !== undefined) {
        paint(rack, null);
      }
    }
  }

  // The handlers that make an element something to drag: `drag` is what it carries
  // (`{kind: "sled", id}` or `{kind: "item", key}`).
  function draggable(drag) {
    return {
      dragstart: (e) => {
        if (e && e.stopPropagation) {
          e.stopPropagation();
        }
        if (e && e.dataTransfer) {
          e.dataTransfer.effectAllowed = "move";
          e.dataTransfer.setData("text/plain", drag.kind === "sled" ? drag.id : drag.key);
        }
        state.drag = drag;
      },
      dragend: clearDrag,
    };
  }

  // The keys of a picked sled or item: arrows move its target, Enter puts it there, Escape lets go.
  function keys(kind) {
    return (e) => {
      const sel = state.selected;
      if (sel === null) {
        return;
      }
      if (e.key === "Escape") {
        state.selected = null;
        state.target = null;
        env.redraw();
        return;
      }
      if (kind === "sled") {
        const sled = sledOf(sel.id);
        const at = state.target !== null ? state.target : { kind: "sled", rack: sled.rack, u: sled.u };
        const by = e.key === "ArrowUp" ? 1 : (e.key === "ArrowDown" ? -1 : 0);
        if (by !== 0) {
          e.preventDefault();
          const to = Math.min(Math.max(at.u + by, 1), Math.max(rackOf(at.rack).height - sled.height + 1, 1));
          state.target = { kind: "sled", rack: at.rack, u: to };
          env.redraw();
        } else if (e.key === "Enter") {
          e.preventDefault();
          if (state.target === null || (state.target.rack === sled.rack && state.target.u === sled.u)) {
            return;
          }
          if (!fitsAt(layout, state.target.rack, sled.height, state.target.u, sled.id)) {
            ops.refuse(`${sled.address} does not fit at U${state.target.u}: another sled stands there or it would reach past the top.`);
            return;
          }
          ops.moveSled(sled.id, state.target.rack, state.target.u);
        }
        return;
      }
      const item = all.find((x) => x.key === sel.id);
      if (item === undefined || item.placement === null) {
        return;
      }
      const sled = sledOf(item.placement.sled);
      const at = state.target !== null ? state.target.slot : item.placement.slot;
      const by = e.key === "ArrowRight" ? 1 : (e.key === "ArrowLeft" ? -1 : 0);
      if (by !== 0) {
        e.preventDefault();
        state.target = { kind: "slot", sled: sled.id, slot: Math.min(Math.max(at + by, 0), sled.slots - 1) };
        env.redraw();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (state.target !== null && state.target.slot !== item.placement.slot) {
          ops.place(item.key, sled.id, state.target.slot);
        }
      }
    };
  }

  // The element a picked thing gets focus on, so the keys reach it after the page is drawn again.
  const focusing = { mounted: (e) => { if (e.currentTarget && e.currentTarget.focus) { e.currentTarget.focus(); } } };

  // Picks a sled or an item (a second click lets go of it).
  function pick(kind, id, on) {
    state.selected = on ? null : { kind, id };
    state.target = null;
    state.assign = null;
    env.redraw();
  }

  function open(item) {
    if (item.device !== undefined) {
      env.go("switch", { id: item.device.id });
    } else if (item.gone === true) {
      return;
    } else if (item.host !== null) {
      env.go("host", { nodeId: item.host });
    } else {
      env.go("inventory", { record: item.id });
    }
  }

  // A server's faceplate over its U: bezel, status LED, name. A click opens its host (the edit mode
  // picks its sled up instead); hover says name, group, version, state and address.
  function serverPlate(server, sled, rack) {
    const on = picked("sled", sled.id);
    return h("button", {
      type: "button",
      class: `server s-${server.state}${sled.height >= 2 ? " tall" : ""}${on ? " picked" : ""}`,
      name: edit ? "pick-sled" : "open-server",
      title: tip(server),
      "aria-pressed": edit ? (on ? "true" : "false") : undefined,
      draggable: edit ? "true" : undefined,
      vars: span(rack, sled),
      on: edit
        ? { click: () => pick("sled", sled.id, on), keydown: keys("sled"), ...draggable({ kind: "sled", id: sled.id }), ...(on ? focusing : {}) }
        : { click: () => open(server) },
    },
    h("span", { class: "ear left", "aria-hidden": "true" }),
    h("span", { class: "led", "aria-hidden": "true" }),
    h("span", { class: "plate-id" },
      h("span", { class: "plate-name" }, server.name),
      sled.height >= 2 && server.group !== "" ? h("span", { class: "plate-meta" }, server.group) : null),
    h("span", { class: "plate-state" }, server.state),
    h("span", { class: "bezel", "aria-hidden": "true" }),
    h("span", { class: "ear right", "aria-hidden": "true" }));
  }

  // A switch's faceplate over its U: a flat plate with a strip of ports (two rows of them on a tall
  // one), a tag for the network it carries, its name and a console jack when its console cable is
  // set. No light and no state word: hostkeeper hears nothing from a switch.
  function gearPlate(item, sled, rack) {
    const on = picked("sled", sled.id);
    const device = item.device;
    const ports = Array.from({ length: sled.height >= 2 ? 24 : 12 }, () => h("span", { class: "port" }));
    return h("button", {
      type: "button",
      class: `gear r-${device.role}${sled.height >= 2 ? " tall" : ""}${on ? " picked" : ""}`,
      name: edit ? "pick-sled" : "open-gear",
      title: gearTip(item),
      "aria-pressed": edit ? (on ? "true" : "false") : undefined,
      draggable: edit ? "true" : undefined,
      vars: span(rack, sled),
      on: edit
        ? { click: () => pick("sled", sled.id, on), keydown: keys("sled"), ...draggable({ kind: "sled", id: sled.id }), ...(on ? focusing : {}) }
        : { click: () => open(item) },
    },
    h("span", { class: "ear left", "aria-hidden": "true" }),
    h("span", { class: "gear-tag" }, device.role === "bmc" ? "BMC" : "DATA"),
    h("span", { class: "plate-name" }, device.name),
    h("span", { class: "ports", "aria-hidden": "true" }, ports),
    device.consoleHost === "" ? null : h("span", { class: "jack", "aria-hidden": "true", title: "console" }),
    h("span", { class: "ear right", "aria-hidden": "true" }));
  }

  // The box of an item on a bay of a tray: a small Mac Studio, NUC or plain box with its light (a
  // server) or its tag (a switch). A click opens the host or the switch page; the edit mode picks it
  // up instead, and it is dragged to another empty bay.
  function box(item, sled, slot) {
    const on = picked("item", item.key);
    const isGear = item.device !== undefined;
    const name = isGear ? item.device.name : item.name;
    return h("button", {
      type: "button",
      class: `box ${boxLook(name)} ${isGear ? `netbox r-${item.device.role}` : `s-${item.state}`}${on ? " picked" : ""}`,
      name: edit ? "pick-item" : (isGear ? "open-gear" : "open-server"),
      title: isGear ? gearTip(item) : tip(item),
      "aria-pressed": edit ? (on ? "true" : "false") : undefined,
      draggable: edit ? "true" : undefined,
      on: edit
        ? { click: () => pick("item", item.key, on), keydown: keys("item"), ...draggable({ kind: "item", key: item.key }), ...(on ? focusing : {}) }
        : { click: () => open(item) },
    },
    isGear ? h("span", { class: "gear-tag" }, item.device.role === "bmc" ? "BMC" : "DATA") : h("span", { class: "led", "aria-hidden": "true" }),
    h("span", { class: "box-name" }, name),
    isGear ? null : h("span", { class: "box-state" }, item.state),
    edit ? h("span", { class: "box-slot", "aria-hidden": "true" }, `${slot}`) : null);
  }

  // An empty bay: a gap (the edit mode: a faint slot number, a button that assigns an item to it, and
  // a place to drop an item on).
  function bay(sled, slot) {
    if (!edit) {
      return h("span", { class: "bay empty", "aria-hidden": "true" });
    }
    const address = `${sled.address}-S${slot}`;
    const here = state.target !== null && state.target.kind === "slot" && state.target.sled === sled.id && state.target.slot === slot;
    const assigning = state.assign !== null && state.assign.sled === sled.id && state.assign.slot === slot;
    const drop = (e) => {
      if (e && e.preventDefault) {
        e.preventDefault();
      }
      if (e && e.currentTarget && e.currentTarget.classList) {
        e.currentTarget.classList.remove("over");
      }
      const d = state.drag;
      clearDrag();
      if (d !== null && d.kind === "item") {
        ops.place(d.key, sled.id, slot);
      }
    };
    return h("button", {
      type: "button", class: `bay empty${here || assigning ? " target" : ""}`, name: "empty-bay",
      title: `${address}: empty`, "aria-label": `empty slot ${address}`,
      on: {
        click: () => {
          const sel = state.selected;
          if (sel !== null && sel.kind === "item") {
            ops.place(sel.id, sled.id, slot);
            return;
          }
          state.selected = null;
          state.target = null;
          state.assign = assigning ? null : { sled: sled.id, slot };
          env.redraw();
        },
        dragover: (e) => {
          if (state.drag !== null && state.drag.kind === "item") {
            e.preventDefault();
            if (e.currentTarget && e.currentTarget.classList) {
              e.currentTarget.classList.add("over");
            }
          }
        },
        dragleave: (e) => {
          if (e && e.currentTarget && e.currentTarget.classList) {
            e.currentTarget.classList.remove("over");
          }
        },
        drop,
      },
    }, `${slot}`);
  }

  // A sled that is not a lone item's faceplate: a tray with a bay for each of its slots. The edit
  // mode gives it a handle (to pick up, drag and mark), the plain view none.
  function tray(sled, rack) {
    const on = picked("sled", sled.id);
    const bays = [];
    for (let slot = 0; slot < sled.slots; slot += 1) {
      const item = itemAt(sled, slot);
      bays.push(item === undefined ? bay(sled, slot) : box(item, sled, slot));
    }
    return h("div", {
      class: `sled slots-${sled.slots}${occupants(layout, sled.id).length === 0 ? " vacant" : ""}${on ? " picked" : ""}`,
      role: "group", "aria-label": `sled ${sled.address}${sled.label === "" ? "" : ` ${sled.label}`}`,
      title: `${sled.address}${sled.label === "" ? "" : ` ${sled.label}`} | ${sled.height}U, ${sled.slots} ${sled.slots === 1 ? "slot" : "slots"}`,
      vars: { ...span(rack, sled), "--n": sled.slots },
    },
    h("span", { class: "ear left", "aria-hidden": "true" }),
    edit
      ? h("button", {
        type: "button", class: "sled-handle", name: "pick-sled", draggable: "true",
        title: sledWords(layout, sled), "aria-label": `pick up sled ${sled.address}`, "aria-pressed": on ? "true" : "false",
        on: { click: () => pick("sled", sled.id, on), keydown: keys("sled"), ...draggable({ kind: "sled", id: sled.id }), ...(on ? focusing : {}) },
      }, h("span", { class: "sled-id" }, sled.label === "" ? `S${sled.number}` : sled.label), h("span", { class: "sled-size" }, `${sled.height}U`))
      : null,
    h("div", { class: "bays" }, bays),
    h("span", { class: "ear right", "aria-hidden": "true" }));
  }

  // One sled: a lone item's faceplate, or a tray.
  function sledView(sled, rack) {
    const lone = sled.slots === 1 ? itemAt(sled, 0) : undefined;
    if (lone === undefined) {
      return tray(sled, rack);
    }
    return lone.device === undefined ? serverPlate(lone, sled, rack) : gearPlate(lone, sled, rack);
  }

  // The U of a rack as a column of slots: a sled over its U, a blank panel over a run of free U (in
  // the edit mode, one button for each free U, to put a picked sled on or to drop a dragged one).
  function elevation(rack, big) {
    const parts = [];
    for (let u = rack.height; u >= 1; u -= 1) {
      parts.push(h("span", { class: u % 5 === 0 || u === 1 ? "u-num five" : "u-num", "aria-hidden": "true", vars: { "--r": rack.height - u + 1 } }, u));
    }
    const here = sledsIn(layout, rack.id);
    const at = new Map(here.map((s) => [s.u + s.height - 1, s]));
    const covered = new Set();
    for (const s of here) {
      for (let u = s.u; u < s.u + s.height; u += 1) {
        covered.add(u);
      }
    }
    let run = 0;
    let runTop = 0;
    const flush = () => {
      if (run > 0) {
        parts.push(h("span", { class: "blank", "aria-hidden": "true", vars: { "--r": rack.height - runTop + 1, "--s": run } }));
      }
      run = 0;
    };
    // Where the picked or dragged sled would stand if it were put on the free U `u`: its lowest U, or null.
    const startFor = (sled, u) => dropStart(layout, rack.id, sled.height, u, sled.id);
    const putOn = (u) => {
      const sel = state.selected;
      if (sel === null || sel.kind !== "sled") {
        return;
      }
      const sled = sledOf(sel.id);
      const start = startFor(sled, u);
      if (start === null) {
        ops.refuse(`${sled.address} does not fit at U${u}: it needs ${sled.height} free U.`);
      } else {
        ops.moveSled(sled.id, rack.id, start);
      }
    };
    for (let u = rack.height; u >= 1; u -= 1) {
      const s = at.get(u);
      if (s !== undefined) {
        flush();
        parts.push(sledView(s, rack));
      } else if (!covered.has(u)) {
        if (edit) {
          parts.push(h("button", {
            type: "button", class: "slot", name: "free-u", title: `U${u}`,
            "aria-label": `free U${u} of ${rackName(layout, rack.id)}`,
            vars: { "--r": rack.height - u + 1 },
            on: {
              click: () => putOn(u),
              dragover: (e) => {
                const d = state.drag;
                if (d === null || d.kind !== "sled") {
                  return;
                }
                const sled = sledOf(d.id);
                const start = startFor(sled, u);
                if (start === null) {
                  paint(rack, null);
                  return;
                }
                e.preventDefault();
                paint(rack, { start, height: sled.height, fits: true });
              },
              drop: (e) => {
                if (e && e.preventDefault) {
                  e.preventDefault();
                }
                const d = state.drag;
                clearDrag();
                if (d === null || d.kind !== "sled") {
                  return;
                }
                const sled = sledOf(d.id);
                const start = startFor(sled, u);
                if (start === null) {
                  ops.refuse(`${sled.address} does not fit at U${u}: it needs ${sled.height} free U.`);
                } else {
                  ops.moveSled(sled.id, rack.id, start);
                }
              },
            },
          }));
        } else {
          if (run === 0) {
            runTop = u;
          }
          run += 1;
        }
      }
    }
    flush();
    if (edit) {
      parts.push(ghostNode(rack));
    }
    return h("div", { class: `chassis${big ? " big" : ""}`, vars: { "--units": rack.height } }, parts);
  }

  return { elevation, itemAt, draggable, clearDrag };
}
