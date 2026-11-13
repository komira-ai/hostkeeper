// Inventory with its Kind: Server | Switch. The Server kind is the page of `inventory.js`, unchanged: four
// fields for each server. The Switch kind is `inventory_switch.js`: the lab's network devices. This
// module only draws the choice of kind (the page's `kind` parameter, Server when it is absent) under
// the title and hands the rest to the page of the kind chosen.

import { h } from "../view.js";
import * as servers from "./inventory.js";
import * as switches from "./inventory_switch.js";

export async function render(ctx) {
  const kind = ctx.params && ctx.params.kind === "switch" ? "switch" : "server";
  const body = kind === "switch" ? await switches.render(ctx) : await servers.render(ctx);
  const choose = (name, text, value) => h("button", {
    type: "button", name, class: kind === value ? "seg-on" : "", "aria-pressed": kind === value ? "true" : "false",
    on: {
      click: () => ctx.navigate("inventory", value === "switch" ? { kind: "switch" } : {}),
    },
  }, text);
  const control = h("div", { class: "kinds" },
    h("div", { class: "segmented", role: "group", "aria-label": "kind" },
      choose("kind-server", "Server", "server"), choose("kind-switch", "Switch", "switch")));
  if (Array.isArray(body.children)) {
    body.children.splice(1, 0, control);
  }
  return body;
}
