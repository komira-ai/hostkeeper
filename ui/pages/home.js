// Home: the lab. The first view is the floor plan, an overhead grid of the lab's rows, each a band
// of rack footprints with their U in use and the state of the servers in them. A click on a row
// opens its racks as front elevations side by side in a strip that scrolls (wheel, drag and arrow
// keys, arrow buttons and one rack at a time on a phone, with the number of the rack in view and
// a fade at an end that has more); a click on a server opens its host page. Above the floor plan,
// the conditions that need a person as one compact strip; below it, the servers not placed yet.
// `list` (true) shows every server in one compact list instead of the floor plan.
// Breadcrumbs: Lab > Row > Rack > Host.
//
// The layout is the coordinator's own small model (rows, racks numbered in their row and as many U
// high as they are made, sleds in the racks, items on the slots of the sleds; docs/design/rack-sleds.md):
// `lab.js` reads it, `sleds.js` draws the sleds. A hostkeeper
// runs one lab; the cluster has a name, shown on top and renamed in the edit mode. The edit mode
// (`Edit layout`, `edit` in the page's params) adds the controls: add a row, add a rack (its height
// from a list), resize, move to another row and remove a rack or a row, rename the cluster, add a sled
// (its size in U, its slots and the U where it fits), resize, move (by dragging it to a free U, or the
// arrow keys, or Move...) and remove a sled, and assign a server or a switch to an empty bay (click it, or
// drag the item there) or unassign it. Every change is one call of `Layout` and the page read again;
// a refusal is told in words. A rack that holds sleds, or a sled that holds items, is not removed.
//
// Params: `row` (a row id) shows that row's racks, `rack` (a rack id) one rack with its hosts,
// `edit` (true) the edit mode, `list` (true) the list of servers in place of the floor plan.
// Calls: Hosts.List, Releases.Status and Alerts.List as the Fleet page made them, then Layout.Get
// and Enrollment.ListRecords (the lab is shown without them when they fail, and says so); on a
// button, Layout.PutName, AddRow, RemoveRow, PutRack, RemoveRack, PutSled, RemoveSled, Place or Unplace.

import {
  h, action, allPages, describeError, enumLabel, errorView, field, fmtUnix, list, pill, table, trimmed,
} from "../view.js";
import {
  DEFAULT_RACK_HEIGHT, RACK_HEIGHTS, STATES, consoleWords, counts, crumbs, gearOf, inRack,
  layoutOf, layoutProblem, rackName, racksOf, roleWords, rowName, serversOf, shownNumber, sledsIn, stripPosition,
  subjectOf, usedU, worstState,
} from "../lab.js";
import { ApiError } from "../api.js";
import { board, tip } from "../sleds.js";
import { addSledPanel, assignPanel, pick, selectionPanel } from "../sledpanels.js";

// How far an arrow key scrolls the rack strip when the width of a rack is not known: one rack's
// width, in the browser's pixels; and the gap between two racks of the strip (the stylesheet's).
const STEP = 280;
const STRIP_GAP = 24;
// A pointer that moves less than this many pixels between press and release is a click.
const DRAG_SLOP = 4;

// The alerts strip: each alert of Alerts.List as the coordinator derived it (its kind, what
// happened, and when the coordinator first saw it) on one line, or the refusal when the call
// failed. The page derives no alert of its own.
function alertsStrip(reply, failure) {
  if (failure !== null) {
    return h("div", { class: "alert-strip bad", role: "status" },
      h("p", { class: "error", role: "alert" }, `The alerts could not be read: ${describeError(failure)}`));
  }
  const alerts = list(reply, "alerts");
  if (alerts.length === 0) {
    return h("div", { class: "alert-strip none", role: "status" }, h("span", { class: "sev ok", "aria-hidden": "true" }), "No alerts: nothing needs a person.");
  }
  const critical = alerts.some((alert) => alert.severity === "ALERT_SEVERITY_CRITICAL");
  return h("div", { class: `alert-strip ${critical ? "bad" : "warn"}`, role: "status" },
    h("strong", { class: "alert-count" }, `Alerts (${alerts.length})`),
    h("ul", {}, alerts.map((alert) => {
      const crit = alert.severity === "ALERT_SEVERITY_CRITICAL";
      return h("li", { class: "alert" },
        h("span", { class: `sev ${crit ? "bad" : "warn"}`, "aria-hidden": "true" }),
        h("strong", {}, enumLabel(alert.kind, "ALERT_KIND_").replaceAll("_", " ")),
        crit ? h("span", { class: "crit" }, "critical") : null,
        h("span", { class: "said" }, alert.message || ""),
        h("small", {}, `since ${fmtUnix(alert.firstObservedUnix)}`));
    })));
}

// Every host of Hosts.List, a page (at most 256 hosts) at a time.
function listHosts(api) {
  return allPages((pageToken) => api.hosts.list(
    pageToken === "" ? {} : { pageToken }), "hosts");
}

// Every inventory entry of Enrollment.ListRecords, a page at a time.
function listRecords(api) {
  return allPages((pageToken) => api.enrollment.listRecords(
    pageToken === "" ? { limit: 256 } : { limit: 256, pageToken }), "records");
}

// Whether a server matches the text typed in the list's filter: its name contains it, or its address
// is it or lies under it (`C0-R0-R0-S1` matches `C0-R0-R0-S1-S0`, not `C0-R0-R0-S10-S0`). No text: all.
export function matches(server, text) {
  const want = text.trim().toLowerCase();
  if (want === "") {
    return true;
  }
  const address = server.placement === null ? "" : server.placement.address.toLowerCase();
  return server.name.toLowerCase().includes(want) || address === want || address.startsWith(`${want}-`);
}

// The list view: every server in one compact table, with its address and where it stands, under a
// filter that takes a name or an address (the page is drawn again when it is applied).
function serversList(ctx, layout, servers, state, redraw) {
  const sorted = servers.filter((s) => matches(s, state.filter)).sort((x, y) => x.name.localeCompare(y.name));
  const rows = sorted.map((s) => h("tr", {},
    h("td", {},
      h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => (s.host !== null ? ctx.navigate("host", { nodeId: s.host }) : ctx.navigate("inventory", { record: s.id })) },
      }, s.name)),
    h("td", {}, pill(s.state, STATES.find(([x]) => x === s.state)[1])),
    h("td", { class: "mono" }, s.placement === null ? "-" : s.placement.address),
    h("td", {}, s.placement === null ? "Unplaced" : `${rackName(layout, s.placement.rack)}, U${s.placement.u}`),
    h("td", {}, s.group || "-"),
    h("td", {}, s.version || "-")));
  const form = { text: state.filter };
  const apply = () => {
    state.filter = form.text;
    redraw();
  };
  const input = field("Filter by name or address", "list-filter", form, "text", { placeholder: "C0-R0-R0-S1" });
  input.children[1].on.keydown = (e) => {
    if (e.key === "Enter") {
      apply();
    }
  };
  return [
    h("div", { class: "buttons" }, input, h("button", { type: "button", name: "apply-filter", on: { click: apply } }, "Filter")),
    table(["Server", "State", "Address", "Where", "Group", "Version"], rows),
  ];
}

// The pills of the states, each with its count.
function stateStrip(servers, placedOnly) {
  const by = counts(servers, placedOnly);
  return h("div", { class: "state-strip", role: "group", "aria-label": "servers by state" },
    STATES.map(([state, tone]) => pill(state, tone, ` ${by[state]}`)));
}

// The page's own state across redraws that read nothing: what is picked up, dragged or being assigned
// in the edit mode, the choices of the forms, and the filter of the list.
function makeState() {
  return {
    selected: null, target: null, drag: null, assign: null, moving: false, ghosts: new Map(), filter: "",
    newSled: { rack: "", height: "1", slots: "1", u: "" }, resize: null, move: null,
    name: null, newRackHeight: String(DEFAULT_RACK_HEIGHT),
  };
}

// The names of the servers standing in the racks `racks`, as a sentence part.
function standing(servers, racks) {
  const ids = new Set(racks.map((r) => r.id));
  return servers
    .filter((s) => s.placement !== null && ids.has(s.placement.rack))
    .map((s) => s.name);
}

// The whole of the page for one rendering: `data` is what was read, `app` the frame's context and
// `state` what survives a redraw.
function lab(app, data, state) {
  const { layout, servers } = data;
  // The network devices (switches) stand in the racks beside the servers: they take U, are picked up
  // and put down like a server, but have no state, host or group, so no count of servers has them.
  const gear = gearOf(layout);
  const all = servers.concat(gear);
  const edit = app.params.edit === true;
  // The edit mode puts servers only where a rack is in view: a row or a rack.
  const canPlace = edit && Boolean(app.params.row || app.params.rack);
  const go = (params) => app.navigate("home", Object.assign({}, params, edit ? { edit: true } : {}));
  const redraw = () => app.show(view());

  // One change of the layout: `call()` makes it, the page is read again and `said` tells what was
  // done; a refusal is told in words and the page stays as it is.
  const change = (label, name, call, said, extra) => action(app, label, name, async () => {
    try {
      await call();
    } catch (error) {
      return layoutProblem(error);
    }
    app.refresh();
    return typeof said === "function" ? said() : said;
  }, extra);

  // The changes of the layout the sleds make, each one call, then the page is read again from the
  // coordinator (it is the source of truth); a refusal is told in words and the page is read again too.
  const done = (said) => {
    state.selected = null;
    state.target = null;
    state.assign = null;
    state.moving = false;
    app.refresh();
    app.report(said);
  };
  const refused = (error) => {
    app.refresh();
    app.report(layoutProblem(error));
  };
  const itemOf = (key) => all.find((x) => x.key === key);
  const ops = {
    refuse: (words) => app.report(words),
    // Puts the sled `id` at the lowest U `u` of `rackId`: one PutSled with the sled's other fields as they are.
    async moveSled(id, rackId, u) {
      const sled = layout.sleds.find((x) => x.id === id);
      try {
        await app.api.layout.putSled({
          id, rack: rackId, u, heightU: sled.height, slots: sled.slots, label: sled.label, reason: `moved ${sled.address}`,
        });
      } catch (error) {
        refused(error);
        return;
      }
      done(`${sled.address} is at ${rackName(layout, rackId)}, U${u}.`);
    },
    // Puts the item `key` on the slot `slot` of the sled `sledId`: one Place (it moves it when it is placed).
    async place(key, sledId, slot) {
      const item = itemOf(key);
      const sled = layout.sleds.find((x) => x.id === sledId);
      try {
        await app.api.layout.place({ subject: subjectOf(item), sled: sledId, slot, reason: `placed ${item.name}` });
      } catch (error) {
        refused(error);
        return;
      }
      done(`${item.name} stands at ${sled.address}-S${slot}.`);
    },
    async unplace(key) {
      const item = itemOf(key);
      try {
        await app.api.layout.unplace({ subject: subjectOf(item), reason: `unplaced ${item.name}` });
      } catch (error) {
        refused(error);
        return;
      }
      done(`${item.name} is unplaced.`);
    },
  };
  Object.assign(ops, {
    // Adds an empty sled of `slots` slots, `height` U high, with its lowest U at `u` of `rackId`.
    async addSled(rackId, height, slots, u) {
      try {
        await app.api.layout.putSled({ id: "", rack: rackId, u, heightU: height, slots, label: "", reason: "add sled" });
      } catch (error) {
        return layoutProblem(error);
      }
      done(`A sled ${height}U high with ${slots} ${slots === 1 ? "slot" : "slots"} is added at ${rackName(layout, rackId)}, U${u}.`);
      return undefined;
    },
    // Gives the sled `id` the size, slots and label (its place stays).
    async resizeSled(id, height, slots, label) {
      const sled = layout.sleds.find((x) => x.id === id);
      try {
        await app.api.layout.putSled({
          id, rack: sled.rack, u: sled.u, heightU: height, slots, label, reason: `resized ${sled.address}`,
        });
      } catch (error) {
        return layoutProblem(error);
      }
      done(`${sled.address} is ${height}U with ${slots} ${slots === 1 ? "slot" : "slots"}.`);
      return undefined;
    },
    // Removes the sled `id`; the coordinator refuses while items stand on it, and the words name them.
    async removeSled(id, names) {
      const sled = layout.sleds.find((x) => x.id === id);
      try {
        await app.api.layout.removeSled({ id, reason: `removed ${sled.address}` });
      } catch (error) {
        return error instanceof ApiError && error.reason === "in_use"
          ? `${sled.address} still holds ${names.join(", ")}: unassign them first.`
          : layoutProblem(error);
      }
      done(`${sled.address} is removed.`);
      return undefined;
    },
  });
  const sleds = board({ layout, all, edit, state, ops, go: (view2, params) => app.navigate(view2, params), redraw });

  // The head of a rack: its number, U in use and, in the edit mode, resize, move and remove.
  function rackHead(rack) {
    const used = usedU(layout, rack.id);
    const form = { height: String(rack.height), row: rack.row };
    // On the strip the rack's number opens the rack alone; on the rack's own page it is only a label.
    const opens = !app.params.rack;
    const head = [
      opens
        ? h("button", {
          type: "button", class: "rack-no linkish", name: "open-rack", title: `Open ${rackName(layout, rack.id)}`,
          on: { click: () => go({ rack: rack.id }) },
        }, `Rack ${shownNumber(rack.number)}`)
        : h("span", { class: "rack-no" }, `Rack ${shownNumber(rack.number)}`),
      h("span", { class: "rack-use" }, `${used}/${rack.height} U`),
    ];
    if (!edit) {
      return h("div", { class: "rack-head" }, head);
    }
    const heights = Array.from(new Set(RACK_HEIGHTS.concat([rack.height]))).sort((a, b) => b - a);
    return h("div", { class: "rack-head edit" }, head,
      pick("rack-height", form, "height", heights.map((n) => [n, `${n}U`]), "Height"),
      change("Resize", "resize-rack", () => app.api.layout.putRack({
        id: rack.id, row: rack.row, heightU: Number(form.height), reason: "resize",
      }), () => `${rackName(layout, rack.id)} is now ${form.height}U.`),
      layout.rows.length > 1
        ? pick("rack-row", form, "row", layout.rows.map((r) => [r.id, rowName(layout, r.id)]), "Row")
        : null,
      layout.rows.length > 1
        ? change("Move", "move-rack", () => app.api.layout.putRack({
          id: rack.id, row: form.row, heightU: rack.height, reason: "move",
        }), "Rack moved.")
        : null,
      action(app, "Remove", "remove-rack", async () => {
        const held = sledsIn(layout, rack.id);
        if (held.length > 0) {
          const names = standing(all, [rack]);
          return `${rackName(layout, rack.id)} still holds ${held.length} ${held.length === 1 ? "sled" : "sleds"}${names.length > 0 ? ` with ${names.join(", ")}` : ""}: remove the sleds first.`;
        }
        try {
          await app.api.layout.removeRack({ id: rack.id, reason: "remove" });
        } catch (error) {
          return layoutProblem(error);
        }
        app.refresh();
        return `${rackName(layout, rack.id)} is removed.`;
      }, { class: "danger" }));
  }

  // One rack: head and elevation.
  function rackColumn(rack, big) {
    return h("div", { class: `rack${big ? " big" : ""}`, role: "group", "aria-label": rackName(layout, rack.id) },
      rackHead(rack),
      h("div", { class: "cabinet" }, sleds.elevation(rack, big)));
  }

  // The racks of a row as a strip: it scrolls by wheel, touch, dragging the background, the arrow
  // keys and the two arrow buttons, a rack at a time (it snaps to one). Above it the number of the
  // rack at its left edge; at an end with more beyond, an edge fades. The scroll handler keeps the
  // number, the buttons and the fades current on the elements themselves, so nothing is drawn again.
  function strip(racks) {
    const drag = { from: null, left: 0, moved: false };
    const pitchOf = (el) => (el.firstElementChild ? el.firstElementChild.offsetWidth + STRIP_GAP : STEP);
    const stripOf = (e) => e.currentTarget.closest(".strip-frame").querySelector(".rack-strip");
    const move = (by) => (e) => {
      const el = stripOf(e);
      el.scrollBy({ left: by * pitchOf(el), behavior: "smooth" });
    };
    // Brings the number, the buttons and the fades up to date: on a scroll, once the strip is in the
    // page and on each resize of the window (which changes how many racks fit).
    const sync = (e) => {
      const el = e.currentTarget;
      const frame = el.parentNode;
      const at = stripPosition({
        scrollLeft: el.scrollLeft, scrollWidth: el.scrollWidth, clientWidth: el.clientWidth,
        pitch: pitchOf(el), count: racks.length,
      });
      frame.querySelector(".strip-pos").textContent = `Rack ${(racks[at.first - 1] || racks[0]).number} of ${racks.length}`;
      frame.querySelector("[name=strip-prev]").disabled = at.atStart;
      frame.querySelector("[name=strip-next]").disabled = at.atEnd;
      frame.classList.toggle("at-start", at.atStart);
      frame.classList.toggle("at-end", at.atEnd);
    };
    return h("div", { class: "strip-frame at-start" },
      h("div", { class: "strip-bar" },
        h("button", { type: "button", name: "strip-prev", class: "strip-arrow", "aria-label": "previous rack", disabled: true, on: { click: move(-1) } }, "\u2039"),
        h("span", { class: "strip-pos", role: "status" }, `Rack ${racks[0].number} of ${racks.length}`),
        h("button", { type: "button", name: "strip-next", class: "strip-arrow", "aria-label": "next rack", on: { click: move(1) } }, "\u203a"),
        h("span", { class: "strip-hint" }, "scroll, drag or use the arrow keys")),
      h("div", {
        class: "rack-strip", tabindex: "0", role: "region", "aria-label": "racks, scroll sideways",
        on: {
          scroll: sync,
          mounted: sync,
          resize: sync,
          keydown: (e) => {
            const to = e.key === "ArrowRight" ? 1 : (e.key === "ArrowLeft" ? -1 : 0);
            if (to !== 0 && e.currentTarget && e.currentTarget.scrollBy) {
              e.preventDefault();
              e.currentTarget.scrollBy({ left: to * pitchOf(e.currentTarget), behavior: "smooth" });
            }
          },
          pointerdown: (e) => {
            if (e.target && e.target.closest && e.target.closest("button")) {
              return;
            }
            drag.from = e.clientX;
            drag.left = e.currentTarget.scrollLeft;
            drag.moved = false;
          },
          pointermove: (e) => {
            if (drag.from === null) {
              return;
            }
            const dx = e.clientX - drag.from;
            if (Math.abs(dx) > DRAG_SLOP) {
              drag.moved = true;
            }
            if (drag.moved) {
              e.currentTarget.scrollLeft = drag.left - dx;
            }
          },
          pointerup: () => {
            drag.from = null;
          },
        },
      }, racks.map((rack) => rackColumn(rack, false))));
  }

  // The servers and switches on no sled, quietly: a muted line with an outlined chip for each, a
  // button to its host or switch page (the edit mode: to pick it up, or drag it to an empty bay).
  function tray() {
    const unplaced = all.filter((s) => s.placement === null);
    return h("section", { class: "tray" },
      h("h3", {}, "Unplaced ", h("span", { class: "tray-count" }, `(${unplaced.length})`)),
      unplaced.length === 0
        ? h("p", { class: "note" }, "Every server stands on a sled.")
        : h("ul", { class: "chips" }, unplaced.map((s) => {
          const on = state.selected !== null && state.selected.kind === "item" && state.selected.id === s.key;
          return h("li", {},
            h("button", {
              type: "button", class: `chip s-${s.device === undefined ? s.state : "gear"}${on ? " picked" : ""}`,
              name: canPlace ? "pick-unplaced" : "open-unplaced", title: s.device === undefined ? tip(s) : `${s.name} | ${roleWords(s.device.role)}`,
              "aria-pressed": canPlace ? (on ? "true" : "false") : undefined,
              draggable: canPlace ? "true" : undefined,
              on: {
                click: () => {
                  if (canPlace) {
                    state.selected = on ? null : { kind: "item", id: s.key };
                    state.target = null;
                    state.assign = null;
                    redraw();
                  } else if (s.device !== undefined) {
                    app.navigate("switch", { id: s.id });
                  } else if (s.host !== null) {
                    app.navigate("host", { nodeId: s.host });
                  } else {
                    app.navigate("inventory", { record: s.id });
                  }
                },
                ...(canPlace ? sleds.draggable({ kind: "item", key: s.key }) : {}),
              },
            }, s.device === undefined ? h("span", { class: "led", "aria-hidden": "true" }) : null, s.name,
            h("span", { class: "plate-state" }, s.device === undefined ? ` ${s.state}` : ` ${roleWords(s.device.role)}`)));
        })),
      canPlace && unplaced.length > 0
        ? h("p", { class: "note" }, "Drag one onto an empty bay of a sled, or pick it and click the bay.")
        : null);
  }

  // What the edit mode shows above the racks: the sled or item picked up, or the bay to assign to.
  function moving() {
    if (!canPlace) {
      return null;
    }
    const panels = { app, layout, all, state, ops, redraw };
    return [selectionPanel(panels), assignPanel(panels)];
  }

  // The head: breadcrumbs (below the floor plan only), the title, the Floor / List switch of the
  // floor plan and the edit toggle.
  function head(title) {
    const inside = Boolean(app.params.row || app.params.rack);
    const listed = app.params.list === true && !inside;
    const view = (name, text, on) => h("button", {
      type: "button", name, class: on ? "seg-on" : "", "aria-pressed": on ? "true" : "false",
      on: { click: () => app.navigate("home", Object.assign({}, app.params, { list: name === "view-list" })) },
    }, text);
    return h("header", { class: "lab-head" },
      inside ? crumbs(layout, { row: app.params.row, rack: app.params.rack }, (view2, params) => (view2 === "home" ? go(params) : app.navigate(view2, params))) : null,
      h("div", { class: "lab-title" },
        h("h2", {}, title),
        h("div", { class: "lab-tools" },
          inside ? null : h("div", { class: "segmented", role: "group", "aria-label": "view" },
            view("view-floor", "Floor", !listed), view("view-list", "List", listed)),
          h("button", {
            type: "button", name: "edit-layout", class: edit ? "primary" : "", "aria-pressed": edit ? "true" : "false",
            on: { click: () => app.navigate("home", Object.assign({}, app.params, { edit: !edit })) },
          }, edit ? "Done editing" : "Edit layout"))));
  }

  // The rename of the cluster, in the edit mode.
  function renameCluster() {
    if (!edit) {
      return null;
    }
    const form = { name: layout.name };
    return h("div", { class: "buttons" },
      field("Cluster name", "cluster-name", form, "name", { maxlength: 32 }),
      action(app, "Save name", "save-name", async () => {
        const name = trimmed(form.name);
        if (name === "") {
          return "Give the cluster a name.";
        }
        try {
          await app.api.layout.putName({ name, reason: "rename" });
        } catch (error) {
          return layoutProblem(error);
        }
        app.refresh();
        return `The cluster is called ${name}.`;
      }));
  }

  // The floor plan: a band for each row, its racks as footprints.
  function floorPlan() {
    if (layout.rows.length === 0) {
      return h("div", { class: "empty-lab" },
        h("p", { class: "empty" }, "No lab layout yet. Add a row, then the racks in it, and place the servers."),
        change("Add a row", "add-row", () => app.api.layout.addRow({ reason: "add row" }), "A row is added."));
    }
    const bands = layout.rows.map((row) => {
      const racks = racksOf(layout, row.id);
      const standingHere = (list2) => list2.filter((sv) => sv.placement !== null && racks.some((r) => r.id === sv.placement.rack));
      const here = standingHere(servers);
      const total = here.length;
      const by = counts(here, false);
      const capacity = racks.reduce((n, r) => n + r.height, 0);
      const used = racks.reduce((n, r) => n + usedU(layout, r.id), 0);
      const open = h("button", {
        type: "button", class: `row-open s-${worstState(by)}`, name: "open-row",
        title: `${rowName(layout, row.id)}: ${racks.length} racks, ${total} servers`,
        on: { click: () => go({ row: row.id }) },
      },
      h("span", { class: "row-id" },
        h("span", { class: "row-no" }, rowName(layout, row.id)),
        h("span", { class: "row-count" }, `${total} servers`),
        h("span", { class: "row-sub" }, `${racks.length} racks, ${capacity === 0 ? 0 : Math.round((used * 100) / capacity)}% of U in use`),
        h("span", { class: "foot-dots" }, STATES
          .filter(([state]) => by[state] > 0)
          .map(([state]) => h("span", { class: `dot s-${state}`, title: `${by[state]} ${state}` }, `${by[state]} ${state}`)))),
      h("span", { class: "footprints" }, racks.length === 0
        ? h("span", { class: "note" }, "no racks")
        : racks.map((rack) => footprint(rack))),
      h("span", { class: "row-go", "aria-hidden": "true" }, "\u203a"));
      return h("section", { class: "lab-row" }, open,
        edit
          ? action(app, "Remove row", "remove-row", async () => {
            const names = standing(all, racks);
            if (racks.length > 0) {
              return `${rowName(layout, row.id)} still holds ${racks.length} racks${names.length > 0 ? ` with ${names.join(", ")}` : ""}: remove the racks first.`;
            }
            try {
              await app.api.layout.removeRow({ id: row.id, reason: "remove" });
            } catch (error) {
              return layoutProblem(error);
            }
            app.refresh();
            return `${rowName(layout, row.id)} is removed.`;
          }, { class: "danger" })
          : null);
    });
    return h("div", { class: "floor", role: "group", "aria-label": "floor plan" }, bands,
      edit ? change("Add row", "add-row", () => app.api.layout.addRow({ reason: "add row" }), "A row is added.") : null);
  }

  // A rack's footprint in the floor plan: a small elevation (a block of its height for each sled,
  // coloured by the state of what stands on it, at its U), the rack's number above and its U in use below.
  function footprint(rack) {
    const here = sledsIn(layout, rack.id);
    const used = usedU(layout, rack.id);
    const by = counts(inRack(all, rack.id).filter((s) => s.device === undefined), false);
    const round = (n) => Math.round(n * 100) / 100;
    const looks = (sled) => {
      const standing2 = all.filter((x) => x.placement !== null && x.placement.sled === sled.id);
      const states = counts(standing2.filter((x) => x.device === undefined), false);
      const worst = worstState(states);
      return worst !== "empty" ? worst : (standing2.length > 0 ? "gear" : "vacant");
    };
    return h("span", { class: `foot s-${worstState(by)}`, title: `${rackName(layout, rack.id)}: ${used}/${rack.height} U` },
      h("span", { class: "foot-no" }, `${shownNumber(rack.number)}`),
      h("span", { class: "mini", "aria-hidden": "true" }, here.map((sled) => h("span", {
        class: `seg s-${looks(sled)}`,
        vars: { "--b": round(((sled.u - 1) * 100) / rack.height), "--h": round((sled.height * 100) / rack.height) },
      }))),
      h("span", { class: "foot-use" }, `${used}/${rack.height}U`));
  }

  // The controls that add to the racks in view: a rack to the row (below) and, in a row or a rack, a sled.
  function addSled(racks) {
    return addSledPanel({ app, layout, state, ops, racks, redraw });
  }

  // Adds a rack to the row, at a height chosen from the list.
  function addRack(rowId) {
    return h("div", { class: "buttons" },
      pick("new-rack-height", state, "newRackHeight", RACK_HEIGHTS.map((n) => [n, `${n}U`]), "Height of the new rack"),
      change("Add rack", "add-rack", () => app.api.layout.putRack({
        row: rowId, heightU: Number(state.newRackHeight), reason: "add rack",
      }), "A rack is added."));
  }

  // One line under the title: how many servers, how many stand in racks and how many do not.
  function summary() {
    const placed = servers.filter((s) => s.placement !== null).length;
    return h("p", { class: "lab-sum" },
      `${servers.length} servers: ${placed} placed in ${layout.racks.length} racks, ${servers.length - placed} unplaced`
      + (gear.length === 0 ? "" : `; ${gear.length} ${gear.length === 1 ? "switch" : "switches"}`));
  }

  function view() {
    const rackId = app.params.rack;
    const rackAt = rackId ? layout.racks.find((r) => r.id === rackId) : undefined;
    const rowId = rackAt ? rackAt.row : app.params.row;
    const rowAt = rowId ? layout.rows.find((r) => r.id === rowId) : undefined;
    if ((rackId || rowId) && rowAt === undefined) {
      return h("section", { class: "page lab" },
        head("Lab"),
        h("p", { class: "empty" }, "That row or rack is not in the lab any more."),
        h("button", { type: "button", name: "to-lab", on: { click: () => app.navigate("home", {}) } }, "Back to the lab"));
    }
    const title = layout.name === "" ? "Lab" : layout.name;
    if (rackAt !== undefined) {
      const here = inRack(servers, rackAt.id);
      const network = inRack(gear, rackAt.id);
      return h("section", { class: "page lab" },
        head(`${rowName(layout, rowAt.id)} / Rack ${shownNumber(rackAt.number)}`),
        edit ? addSled([rackAt]) : null,
        moving(),
        h("div", { class: "rack-solo" }, rackColumn(rackAt, true),
          h("div", { class: "rack-hosts" },
            h("h3", {}, `Servers (${here.length})`),
            here.length === 0
              ? h("p", { class: "empty" }, "No server stands in this rack yet.")
              : table(["Server", "Address", "U", "State", "Group", "Version"], here.map((s) => h("tr", {},
                h("td", {}, h("button", {
                  type: "button", class: "linklike", name: "open-host",
                  on: { click: () => (s.host !== null ? app.navigate("host", { nodeId: s.host }) : app.navigate("inventory", { record: s.id })) },
                }, s.name)),
                h("td", { class: "mono" }, s.placement.address),
                h("td", {}, `U${s.placement.u}`),
                h("td", {}, pill(s.state, STATES.find(([x]) => x === s.state)[1])),
                h("td", {}, s.group || "-"),
                h("td", {}, s.version || "-"))))),
          network.length === 0 ? null : h("div", { class: "rack-hosts" },
            h("h3", {}, `Network (${network.length})`),
            table(["Switch", "Address", "U", "Network", "Console"], network.map((s) => h("tr", {},
              h("td", {}, h("button", {
                type: "button", class: "linklike", name: "open-gear-row",
                on: { click: () => app.navigate("switch", { id: s.id }) },
              }, s.name)),
              h("td", { class: "mono" }, s.placement.address),
              h("td", {}, `U${s.placement.u}`),
              h("td", {}, roleWords(s.device.role)),
              h("td", {}, consoleWords(s.device))))))),
        tray());
    }
    if (rowAt !== undefined) {
      const racks = racksOf(layout, rowAt.id);
      return h("section", { class: "page lab" },
        head(`${title} / ${rowName(layout, rowAt.id)}`),
        stateStrip(servers, true),
        edit ? addRack(rowAt.id) : null,
        edit ? addSled(racks) : null,
        moving(),
        racks.length === 0
          ? h("p", { class: "empty" }, "This row has no rack yet.")
          : strip(racks),
        tray());
    }
    return h("section", { class: "page lab" },
      head(title),
      renameCluster(),
      data.alerts,
      data.failure !== null
        ? h("p", { class: "error", role: "alert" }, `The lab could not be read: ${describeError(data.failure)}`)
        : [summary(), stateStrip(servers, false),
          ...(app.params.list === true ? [serversList(app, layout, servers, state, redraw)] : [floorPlan(), moving(), tray()])],
      data.notes);
  }

  return view();
}

export async function render(ctx) {
  let hosts;
  let status;
  try {
    hosts = await listHosts(ctx.api);
    status = await ctx.api.releases.status({});
  } catch (error) {
    return errorView("Lab", error);
  }
  let alerts = null;
  let alertFailure = null;
  try {
    alerts = await ctx.api.alerts.list({});
  } catch (error) {
    alertFailure = error;
  }
  let layout = layoutOf({});
  let records = [];
  let failure = null;
  try {
    layout = layoutOf(await ctx.api.layout.get({}));
    records = await listRecords(ctx.api);
  } catch (error) {
    failure = error;
  }
  const servers = serversOf(layout, hosts, records);
  const inside = Boolean(ctx.params.row || ctx.params.rack);
  const notes = inside || status.quiet !== true
    ? null
    : h("p", { class: "note" }, "The coordinator is in its quiet start: it orders nothing yet.");
  return lab(ctx, { layout, servers, failure, notes, alerts: inside ? null : alertsStrip(alerts, alertFailure) }, makeState());
}
