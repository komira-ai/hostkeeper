// Home: the lab. The first view is the floor plan, an overhead grid of the lab's rows, each a band
// of rack footprints with their U in use and the state of the servers in them. A click on a row
// opens its racks as front elevations side by side in a strip that scrolls (wheel, drag and arrow
// keys, arrow buttons and one rack at a time on a phone, with the number of the rack in view and
// a fade at an end that has more); a click on a server opens its host page. Above the floor plan,
// the conditions that need a person as one compact strip; below it, the servers not placed yet.
// `list` (true) shows every server in one compact list instead of the floor plan.
// Breadcrumbs: Lab > Row > Rack > Host.
//
// The layout is the coordinator's own small model (rows 1, 2, 3, racks numbered in their row and
// as many U high as they are made, placements of a server in a rack): `lab.js` reads it. A hostkeeper
// runs one lab; the cluster has a name, shown on top and renamed in the edit mode. The edit mode
// (`Edit layout`, `edit` in the page's params) adds the controls: add a row, add a rack (its height
// from a list), resize, move to another row and remove a rack or a row, rename the cluster, and
// move a server: pick it up (or drag it) and put it on a free U of any rack in view, or take it out
// of its rack. Every change is one call of `Layout` and the page read again; a refusal is told in
// words. A row or a rack that still holds servers is not removed; the refusal names them.
//
// Params: `row` (a row id) shows that row's racks, `rack` (a rack id) one rack with its hosts,
// `edit` (true) the edit mode, `list` (true) the list of servers in place of the floor plan.
// Calls: Hosts.List, Releases.Status and Alerts.List as the Fleet page made them, then Layout.Get
// and Enrollment.ListRecords (the lab is shown without them when they fail, and says so); on a
// button, Layout.PutName, AddRow, RemoveRow, PutRack, RemoveRack, Place or Unplace.

import {
  h, action, allPages, describeError, enumLabel, errorView, field, fmtUnix, list, pill, table, trimmed,
} from "../view.js";
import {
  DEFAULT_RACK_HEIGHT, RACK_HEIGHTS, SERVER_HEIGHTS, STATES, counts, crumbs, inRack, layoutOf, layoutProblem,
  rackName, racksOf, rowName, serversOf, stripPosition, subjectOf, worstState,
} from "../lab.js";

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

// The list view: every server in one compact table, with where it stands.
function serversList(ctx, layout, servers) {
  const sorted = [...servers].sort((x, y) => x.name.localeCompare(y.name));
  const rows = sorted.map((s) => h("tr", {},
    h("td", {},
      h("button", {
        type: "button", class: "linklike", name: "open-host",
        on: { click: () => (s.host !== null ? ctx.navigate("host", { nodeId: s.host }) : ctx.navigate("inventory", { record: s.id })) },
      }, s.name)),
    h("td", {}, pill(s.state, STATES.find(([x]) => x === s.state)[1])),
    h("td", {}, s.placement === null ? "Unplaced" : `${rackName(layout, s.placement.rack)}, U${s.placement.u}`),
    h("td", {}, s.group || "-"),
    h("td", {}, s.version || "-")));
  return table(["Server", "State", "Where", "Group", "Version"], rows);
}

// The pills of the states, each with its count.
function stateStrip(servers, placedOnly) {
  const by = counts(servers, placedOnly);
  return h("div", { class: "state-strip", role: "group", "aria-label": "servers by state" },
    STATES.map(([state, tone]) => pill(state, tone, ` ${by[state]}`)));
}

// What a server's tooltip says: its name, group, version and state.
function tip(server) {
  const parts = [server.name];
  if (server.group !== "") {
    parts.push(`group ${server.group}`);
  }
  if (server.version !== "") {
    parts.push(`version ${server.version}`);
  }
  parts.push(server.state);
  return parts.join(" | ");
}

// The page's own state across redraws that read nothing: the server picked up in the edit mode.
function makeState() {
  return { picked: null, height: "1", name: null, newRackHeight: String(DEFAULT_RACK_HEIGHT) };
}

// A select of `[value, label]` options whose choice is kept in `state[key]`.
function pick(name, state, key, options, label) {
  const choices = options.map(([value, text]) => h("option", { value: String(value), selected: String(state[key]) === String(value) }, text));
  const control = h("select", { name }, choices);
  control.on = {
    change: (e) => {
      state[key] = e.target.value;
      for (const choice of choices) {
        choice.attrs.selected = choice.attrs.value === String(state[key]);
      }
    },
  };
  return h("label", {}, label, control);
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

  // Places `server`, picked up, with its lowest U at `u` of the rack `rackId`.
  async function put(server, rackId, u) {
    const height = server.placement === null ? Number(state.height) : server.placement.height;
    try {
      await app.api.layout.place({
        subject: subjectOf(server), rack: rackId, heightU: height, u,
        reason: `placed ${server.name}`,
      });
    } catch (error) {
      app.report(layoutProblem(error));
      return;
    }
    state.picked = null;
    app.refresh();
    app.report(`${server.name} is at ${rackName(layout, rackId)}, U${u}.`);
  }

  // The U of a rack as a column of slots: a server over its U, a blank panel over a run of free
  // U (in the edit mode, one button for each free U, to drop or put a server on).
  function elevation(rack, big) {
    const here = inRack(servers, rack.id);
    const parts = [];
    for (let u = rack.height; u >= 1; u -= 1) {
      parts.push(h("span", { class: u % 5 === 0 || u === 1 ? "u-num five" : "u-num", "aria-hidden": "true", vars: { "--r": rack.height - u + 1 } }, u));
    }
    const at = new Map(here.map((s) => [s.placement.u + s.placement.height - 1, s]));
    const covered = new Set();
    for (const s of here) {
      for (let u = s.placement.u; u < s.placement.u + s.placement.height; u += 1) {
        covered.add(u);
      }
    }
    // A run of free U, top down, is one blank panel (the edit mode: a slot for each U instead).
    let run = 0;
    let runTop = 0;
    const flush = () => {
      if (run > 0) {
        parts.push(h("span", { class: "blank", "aria-hidden": "true", vars: { "--r": rack.height - runTop + 1, "--s": run } }));
      }
      run = 0;
    };
    const dropOn = (u) => (state.picked === null ? undefined : put(servers.find((x) => x.key === state.picked), rack.id, u));
    for (let u = rack.height; u >= 1; u -= 1) {
      const s = at.get(u);
      if (s !== undefined) {
        flush();
        parts.push(serverPlate(s, rack));
      } else if (!covered.has(u)) {
        if (edit) {
          parts.push(h("button", {
            type: "button", class: "slot", name: "slot", title: `${rackName(layout, rack.id)}, U${u}`,
            "aria-label": `free U${u} of ${rackName(layout, rack.id)}`,
            vars: { "--r": rack.height - u + 1 },
            on: {
              click: () => dropOn(u),
              dragover: (e) => e.preventDefault(),
              drop: (e) => {
                e.preventDefault();
                return dropOn(u);
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
    return h("div", { class: `chassis${big ? " big" : ""}`, vars: { "--units": rack.height } }, parts);
  }

  // A server's faceplate over its U: bezel, status LED, name. A click opens its host (the edit
  // mode picks it up instead); hover says name, group, version and state.
  function serverPlate(server, rack) {
    const p = server.placement;
    const picked = state.picked === server.key;
    const open = () => {
      if (server.host !== null && server.gone !== true) {
        app.navigate("host", { nodeId: server.host });
      } else if (server.gone !== true) {
        app.navigate("inventory", { record: server.id });
      }
    };
    return h("button", {
      type: "button",
      class: `server s-${server.state}${p.height >= 2 ? " tall" : ""}${picked ? " picked" : ""}`,
      name: edit ? "pick-server" : "open-server",
      title: tip(server),
      "aria-pressed": edit ? (picked ? "true" : "false") : undefined,
      draggable: edit ? "true" : undefined,
      vars: { "--r": rack.height - (p.u + p.height - 1) + 1, "--s": p.height },
      on: {
        click: () => {
          if (!edit) {
            open();
            return;
          }
          state.picked = picked ? null : server.key;
          state.height = String(p.height);
          redraw();
        },
        dragstart: () => {
          state.picked = server.key;
        },
      },
    },
    h("span", { class: "ear left", "aria-hidden": "true" }),
    h("span", { class: "led", "aria-hidden": "true" }),
    h("span", { class: "plate-id" },
      h("span", { class: "plate-name" }, server.name),
      p.height >= 2 && server.group !== "" ? h("span", { class: "plate-meta" }, server.group) : null),
    h("span", { class: "plate-state" }, server.state),
    h("span", { class: "bezel", "aria-hidden": "true" }),
    h("span", { class: "ear right", "aria-hidden": "true" }));
  }

  // The head of a rack: its number, U in use and, in the edit mode, resize, move and remove.
  function rackHead(rack) {
    const here = inRack(servers, rack.id);
    const used = here.reduce((n, s) => n + s.placement.height, 0);
    const form = { height: String(rack.height), row: rack.row };
    // On the strip the rack's number opens the rack alone; on the rack's own page it is only a label.
    const opens = !app.params.rack;
    const head = [
      opens
        ? h("button", {
          type: "button", class: "rack-no linkish", name: "open-rack", title: `Open ${rackName(layout, rack.id)}`,
          on: { click: () => go({ rack: rack.id }) },
        }, `Rack ${rack.number}`)
        : h("span", { class: "rack-no" }, `Rack ${rack.number}`),
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
        ? pick("rack-row", form, "row", layout.rows.map((r) => [r.id, `Row ${r.number}`]), "Row")
        : null,
      layout.rows.length > 1
        ? change("Move", "move-rack", () => app.api.layout.putRack({
          id: rack.id, row: form.row, heightU: rack.height, reason: "move",
        }), "Rack moved.")
        : null,
      action(app, "Remove", "remove-rack", async () => {
        const names = standing(servers, [rack]);
        if (names.length > 0) {
          return `${rackName(layout, rack.id)} still holds ${names.join(", ")}: move them out first.`;
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
      h("div", { class: "cabinet" }, elevation(rack, big)));
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
      frame.querySelector(".strip-pos").textContent = `Rack ${at.first} of ${racks.length}`;
      frame.querySelector("[name=strip-prev]").disabled = at.atStart;
      frame.querySelector("[name=strip-next]").disabled = at.atEnd;
      frame.classList.toggle("at-start", at.atStart);
      frame.classList.toggle("at-end", at.atEnd);
    };
    return h("div", { class: "strip-frame at-start" },
      h("div", { class: "strip-bar" },
        h("button", { type: "button", name: "strip-prev", class: "strip-arrow", "aria-label": "previous rack", disabled: true, on: { click: move(-1) } }, "\u2039"),
        h("span", { class: "strip-pos", role: "status" }, `Rack 1 of ${racks.length}`),
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

  // The servers not in a rack, quietly: a muted line with an outlined chip for each, a button to
  // its host (the edit mode: to pick it up).
  function tray() {
    const unplaced = servers.filter((s) => s.placement === null);
    return h("section", { class: "tray" },
      h("h3", {}, "Unplaced ", h("span", { class: "tray-count" }, `(${unplaced.length})`)),
      unplaced.length === 0
        ? h("p", { class: "note" }, "Every server stands in a rack.")
        : h("ul", { class: "chips" }, unplaced.map((s) => h("li", {},
          h("button", {
            type: "button", class: `chip s-${s.state}${state.picked === s.key ? " picked" : ""}`,
            name: canPlace ? "pick-unplaced" : "open-unplaced", title: tip(s),
            on: {
              click: () => {
                if (canPlace) {
                  state.picked = state.picked === s.key ? null : s.key;
                  redraw();
                } else if (s.host !== null) {
                  app.navigate("host", { nodeId: s.host });
                } else {
                  app.navigate("inventory", { record: s.id });
                }
              },
            },
          }, h("span", { class: "led", "aria-hidden": "true" }), s.name, h("span", { class: "plate-state" }, ` ${s.state}`))))),
      canPlace && unplaced.length > 0
        ? h("div", { class: "buttons" },
          pick("tray-height", state, "height", SERVER_HEIGHTS.map((n) => [n, `${n}U`]), "Height of the one picked"),
          h("span", { class: "note" }, "Pick a server, then a free U of a rack."))
        : null);
  }

  // The banner of a move in progress: which server, and what can be done with it.
  function moving() {
    const server = servers.find((s) => s.key === state.picked);
    if (!canPlace || server === undefined) {
      return null;
    }
    return h("div", { class: "moving", role: "status" },
      h("strong", {}, server.name),
      server.placement === null ? " is not placed: click a free U of a rack." : ` is at ${rackName(layout, server.placement.rack)}, U${server.placement.u}: click a free U to move it.`,
      h("button", { type: "button", name: "cancel-move", on: { click: () => { state.picked = null; redraw(); } } }, "Cancel"),
      server.placement === null ? null : action(app, "Take out of its rack", "unplace", async () => {
        try {
          await app.api.layout.unplace({ subject: subjectOf(server), reason: `unplaced ${server.name}` });
        } catch (error) {
          return layoutProblem(error);
        }
        state.picked = null;
        app.refresh();
        return `${server.name} is unplaced.`;
      }));
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
        change("Add a row", "add-row", () => app.api.layout.addRow({ reason: "add row" }), "Row 1 is added."));
    }
    const bands = layout.rows.map((row) => {
      const racks = racksOf(layout, row.id);
      const here = servers.filter((sv) => sv.placement !== null && racks.some((r) => r.id === sv.placement.rack));
      const total = here.length;
      const by = counts(here, false);
      const capacity = racks.reduce((n, r) => n + r.height, 0);
      const used = here.reduce((n, sv) => n + sv.placement.height, 0);
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
            const names = standing(servers, racks);
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

  // A rack's footprint in the floor plan: a small elevation (a block of its height for each server,
  // coloured by state, at its U), the rack's number above and its U in use below.
  function footprint(rack) {
    const here = inRack(servers, rack.id);
    const used = here.reduce((n, s) => n + s.placement.height, 0);
    const by = counts(here, false);
    const round = (n) => Math.round(n * 100) / 100;
    return h("span", { class: `foot s-${worstState(by)}`, title: `${rackName(layout, rack.id)}: ${used}/${rack.height} U` },
      h("span", { class: "foot-no" }, `${rack.number}`),
      h("span", { class: "mini", "aria-hidden": "true" }, here.map((s) => h("span", {
        class: `seg s-${s.state}`,
        vars: { "--b": round(((s.placement.u - 1) * 100) / rack.height), "--h": round((s.placement.height * 100) / rack.height) },
      }))),
      h("span", { class: "foot-use" }, `${used}/${rack.height}U`));
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
      `${servers.length} servers: ${placed} placed in ${layout.racks.length} racks, ${servers.length - placed} unplaced`);
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
      return h("section", { class: "page lab" },
        head(`${rowName(layout, rowAt.id)} / Rack ${rackAt.number}`),
        moving(),
        h("div", { class: "rack-solo" }, rackColumn(rackAt, true),
          h("div", { class: "rack-hosts" },
            h("h3", {}, `Servers (${here.length})`),
            here.length === 0
              ? h("p", { class: "empty" }, "No server stands in this rack yet.")
              : table(["Server", "U", "Height", "State", "Group", "Version"], here.map((s) => h("tr", {},
                h("td", {}, h("button", {
                  type: "button", class: "linklike", name: "open-host",
                  on: { click: () => (s.host !== null ? app.navigate("host", { nodeId: s.host }) : app.navigate("inventory", { record: s.id })) },
                }, s.name)),
                h("td", {}, `U${s.placement.u}`),
                h("td", {}, `${s.placement.height}U`),
                h("td", {}, pill(s.state, STATES.find(([x]) => x === s.state)[1])),
                h("td", {}, s.group || "-"),
                h("td", {}, s.version || "-")))))),
        tray());
    }
    if (rowAt !== undefined) {
      const racks = racksOf(layout, rowAt.id);
      return h("section", { class: "page lab" },
        head(`${title} / ${rowName(layout, rowAt.id)}`),
        stateStrip(servers, true),
        edit ? addRack(rowAt.id) : null,
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
          ...(app.params.list === true ? [serversList(app, layout, servers)] : [floorPlan(), moving(), tray()])],
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
