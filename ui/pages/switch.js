// The switch page: one network device of the lab (docs/design/network-gear.md), opened from its
// faceplate in a rack or from the Inventory page. A header with its name and the network it carries
// (no light and no state: hostkeeper hears nothing from a switch) and a small elevation of the rack
// with its U lit, then its fields (Network, Console, Management MAC, Placement, Notes), Edit (the
// Inventory page, kind Switch, this switch's form) and Remove in a danger zone.
//
// Param: `id`, the device's id. Calls: Layout.Get; on Remove, Layout.RemoveDevice, after a confirm.

import { h, action, errorView, pill } from "../view.js";
import { consoleWords, crumbs, layoutOf, layoutProblem, roleWords } from "../lab.js";
import { miniRack } from "./host.js";
import { whereWords } from "./inventory_switch.js";

export async function render(ctx) {
  const id = ctx.params && typeof ctx.params.id === "string" ? ctx.params.id : "";
  let layout;
  try {
    layout = layoutOf(await ctx.api.layout.get({}));
  } catch (error) {
    return errorView("Switch", error);
  }
  const device = layout.devices.find((d) => d.id === id);
  if (device === undefined) {
    return h("section", { class: "page" },
      h("h2", {}, "Switch"),
      h("p", { class: "empty" }, "That switch is not in the inventory any more."),
      h("button", { type: "button", name: "to-inventory", on: { click: () => ctx.navigate("inventory", { kind: "switch" }) } }, "Back to the inventory"));
  }
  const held = layout.placements.find((p) => p.key === device.key);
  const rackAt = held === undefined ? undefined : layout.racks.find((r) => r.id === held.rack);
  const go = (view, params) => ctx.navigate(view, params);
  const consoleCell = device.consoleHost === ""
    ? "none"
    : [consoleWords(device), " ", h("button", {
      type: "button", class: "linklike", name: "open-console-host",
      on: { click: () => ctx.navigate("host", { nodeId: device.consoleHost }) },
    }, `open ${device.consoleHost}`)];
  const facts = [
    ["Network", roleWords(device.role)],
    ["Console", consoleCell],
    ["Management MAC", device.managementMac || "-"],
    ["Where", whereWords(layout, held)],
    ["Notes", device.notes ? h("pre", {}, device.notes) : "-"],
  ];
  return h("section", { class: "page hostpage gearpage" },
    crumbs(layout, { rack: rackAt === undefined ? "" : rackAt.id, leaf: device.name }, go),
    h("header", { class: "hostbar" },
      h("div", { class: "hb-main" },
        h("div", { class: "hb-title" },
          h("span", { class: "kicker" }, "Switch"),
          h("h2", {}, device.name),
          pill(roleWords(device.role), "info")),
        h("div", { class: "hb-meta" },
          [["Console", consoleWords(device)], ["Where", whereWords(layout, held)]]
            .map(([k, v]) => h("span", { class: "meta" }, h("small", {}, k), v)))),
      miniRack(layout, held, rackAt)),
    h("div", { class: "table-wrap facts" }, h("table", { class: "kv" },
      h("tbody", {}, facts.map(([k, v]) => h("tr", {}, h("th", {}, k), h("td", {}, v)))))),
    h("div", { class: "buttons" },
      action(ctx, "Edit", "edit-switch", async () => {
        ctx.navigate("inventory", { kind: "switch", edit: device.id });
        return undefined;
      }, { class: "primary" })),
    h("fieldset", { class: "danger-zone" },
      h("legend", {}, "Danger zone"),
      h("div", { class: "dz-row" },
        h("div", { class: "dz-text" },
          h("strong", {}, "Remove this switch"),
          h("p", { class: "note" }, "The switch leaves the inventory and its rack. Nothing on the network changes.")),
        action(ctx, "Remove this switch", "remove-switch", async () => {
          if (!ctx.confirm(`Remove ${device.name} from the inventory? It leaves its rack too.`)) {
            return "Not removed.";
          }
          try {
            await ctx.api.layout.removeDevice({ id: device.id, reason: "removed on the switch page" });
          } catch (error) {
            return layoutProblem(error);
          }
          ctx.navigate("inventory", { kind: "switch" });
          return `Removed ${device.name}.`;
        }, { class: "danger" }))));
}
