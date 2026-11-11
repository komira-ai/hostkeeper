// Users: the people who sign in with a name and a password, and what an admin does with them.
// Calls: Users.List; on a button, Users.Create, Users.Disable, Users.Enable,
// Users.ResetPassword (each after a question where it ends a person's sessions or removes them)
// and Users.Delete.
//
// Create and Reset password answer with a temporary password, the one time it is ever told. The
// page shows it under the table until the operator says they have copied it or leaves the page;
// it is held in this page's memory only, never in the status line, and no reload of the list
// shows it again. A reply that says nothing of `enabled` or `mustChange` is the proto3 false.

import { h, action, errorView, field, fmtUnix, list, table, trimmed } from "../view.js";

// The temporary password just told, with whose it is, until it is dismissed.
function told(shown, again) {
  if (shown.secret === "") {
    return null;
  }
  return h("section", { class: "secret", role: "alert" },
    h("p", {}, `Temporary password of ${shown.who}. It is shown once and the coordinator keeps no copy.`),
    h("p", {}, h("code", { name: "secret" }, shown.secret)),
    h("p", {}, shown.expires),
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
    listed = await ctx.api.users.list({});
  } catch (error) {
    failure = error;
  }
  const secret = told(shown, again);
  if (failure !== null) {
    return h("div", { class: "page" }, secret, errorView("Users", failure));
  }
  const users = list(listed, "users");
  const form = { entry: "" };
  // What is told of a temporary password: whose it is, the password and, when the reply names
  // it, the second after which it no longer signs in.
  const tell = (name, reply) => {
    shown.who = name;
    shown.secret = typeof reply.password === "string" ? reply.password : "";
    const until = fmtUnix(reply.user && reply.user.passwordValidUntilUnix);
    shown.expires = until === "-"
      ? "The user must choose another password at the first sign-in."
      : `The user must choose another password at the first sign-in; this one stops working after ${until}.`;
  };
  const rows = users.map((user) => {
    const { name } = user;
    const enabled = user.enabled === true;
    return h("tr", {},
      h("td", {}, name),
      h("td", {}, enabled ? "yes" : "no"),
      h("td", {}, user.mustChange === true ? "yes" : "no"),
      h("td", {}, user.createdBy || "-"),
      h("td", {}, fmtUnix(user.createdUnix)),
      h("td", {}, fmtUnix(user.lastLoginUnix)),
      h("td", {}, fmtUnix(user.passwordValidUntilUnix)),
      h("td", {},
        enabled
          ? action(ctx, "Disable", "disable", async () => {
            if (!ctx.confirm(`Disable ${name}? Their sessions end now and they cannot sign in.`)) {
              return "Not disabled.";
            }
            await ctx.api.users.disable({ name });
            await again();
            return `Disabled ${name}.`;
          })
          : action(ctx, "Enable", "enable", async () => {
            await ctx.api.users.enable({ name });
            await again();
            return `Enabled ${name}.`;
          }),
        action(ctx, "Reset password", "reset-password", async () => {
          if (!ctx.confirm(`Reset the password of ${name}? Their sessions end now and the old password stops working.`)) {
            return "Not reset.";
          }
          const reply = await ctx.api.users.resetPassword({ name });
          tell(name, reply);
          await again();
          return `Reset the password of ${name}.`;
        }),
        action(ctx, "Delete", "delete", async () => {
          if (!ctx.confirm(`Delete ${name}? Their sessions end now. The audit entries that name them stay.`)) {
            return "Not deleted.";
          }
          await ctx.api.users.delete({ name });
          await again();
          return `Deleted ${name}.`;
        })));
  });
  return h("section", { class: "page" },
    secret,
    h("h2", {}, "Users"),
    h("p", { class: "note" },
      "A user signs in with a name and a password and is an operator, as an API token is. The last enabled user cannot be disabled or deleted, and the coordinator refuses a reset of the password of the user who asks."),
    h("h3", {}, `Users (${users.length})`),
    users.length === 0
      ? h("p", {}, "No user.")
      : table(["Name", "Enabled", "Must change password", "Made by", "Made at", "Last sign-in", "Temporary password valid until", ""], rows),
    h("fieldset", {},
      h("legend", {}, "Make a user"),
      field("Name (lower-case letters, digits and -)", "user-name", form, "entry", { maxlength: 32 }),
      action(ctx, "Create", "create", async () => {
        const name = trimmed(form.entry);
        if (name === "") {
          return "Name the user.";
        }
        const reply = await ctx.api.users.create({ name });
        tell(name, reply);
        await again();
        return `Made the user ${name}.`;
      })));
}

export async function render(ctx) {
  return view(ctx, { who: "", secret: "", expires: "" });
}
