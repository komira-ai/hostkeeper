// API tokens: the tokens a script calls the operator API with, by name. Calls: ApiTokens.List;
// on a button, ApiTokens.Create and, after a question, ApiTokens.Revoke.
//
// Create answers with the token, the one time it is ever told: the page shows it under the table
// until the operator says they have copied it or leaves the page, in this page's memory only and
// never in the status line. A token of the coordinator's tokens file is read-only: it has no
// Revoke button, because removing it means editing that file. A source the page does not know
// (absent, or one a newer coordinator adds) is read-only too: only a token of the store is
// revoked here.

import { h, action, enumLabel, errorView, field, fmtUnix, list, table, trimmed } from "../view.js";

const STORE = "API_TOKEN_SOURCE_STORE";
const FILE = "API_TOKEN_SOURCE_TOKENS_FILE";

// The words for where a token comes from.
function sourceLabel(source) {
  return source === FILE ? "tokens file (read-only)" : enumLabel(source, "API_TOKEN_SOURCE_");
}

function told(shown, again) {
  if (shown.secret === "") {
    return null;
  }
  return h("section", { class: "secret", role: "alert" },
    h("p", {}, `The token ${shown.who}. It is shown once and the coordinator keeps its digest only.`),
    h("p", {}, h("code", { name: "secret" }, shown.secret)),
    h("button", {
      type: "button",
      name: "dismiss-secret",
      on: {
        click: async () => {
          shown.secret = "";
          await again();
        },
      },
    }, "I have copied it"));
}

async function view(ctx, shown) {
  const again = async () => ctx.show(await view(ctx, shown));
  let listed = null;
  let failure = null;
  try {
    listed = await ctx.api.apiTokens.list({});
  } catch (error) {
    failure = error;
  }
  const secret = told(shown, again);
  if (failure !== null) {
    return h("div", { class: "page" }, secret, errorView("API tokens", failure));
  }
  const tokens = list(listed, "tokens");
  const form = { entry: "" };
  const rows = tokens.map((token) => {
    const { name } = token;
    return h("tr", {},
      h("td", {}, name),
      h("td", {}, sourceLabel(token.source)),
      h("td", {}, token.createdBy || "-"),
      h("td", {}, fmtUnix(token.createdUnix)),
      h("td", {}, token.source === STORE
        ? action(ctx, "Revoke", "revoke", async () => {
          if (!ctx.confirm(`Revoke ${name}? The token is refused from then on.`)) {
            return "Not revoked.";
          }
          await ctx.api.apiTokens.revoke({ name });
          await again();
          return `Revoked ${name}.`;
        })
        : null));
  });
  const fromFile = tokens.some((token) => token.source !== STORE);
  return h("section", { class: "page" },
    secret,
    h("h2", {}, "API tokens"),
    h("p", { class: "note" },
      "A token is for a script: it acts as the operator it is named for and cannot sign in. Make one for each script, so the audit says which one acted."),
    fromFile
      ? h("p", { class: "note" }, "A token of the tokens file cannot be revoked here: edit that file on the coordinator to remove it.")
      : null,
    h("h3", {}, `Tokens (${tokens.length})`),
    tokens.length === 0 ? h("p", {}, "No token.") : table(["Name", "Source", "Made by", "Made at", ""], rows),
    h("fieldset", {},
      h("legend", {}, "Make a token"),
      field("Name (lower-case letters, digits and -)", "token-name", form, "entry", { maxlength: 32 }),
      action(ctx, "Create", "create", async () => {
        const name = trimmed(form.entry);
        if (name === "") {
          return "Name the token.";
        }
        const reply = await ctx.api.apiTokens.create({ name });
        shown.who = name;
        shown.secret = typeof reply.secret === "string" ? reply.secret : "";
        await again();
        return `Made the token ${name}.`;
      })));
}

export async function render(ctx) {
  return view(ctx, { who: "", secret: "" });
}
