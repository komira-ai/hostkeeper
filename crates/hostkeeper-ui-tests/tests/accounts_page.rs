//! The Users page and the API tokens page, run in the JavaScript engine against the fake gateway
//! of `support`: the rows they show, the call each button sends with the body the proto reads,
//! the question each destructive button asks first, and the one telling of a password or a token.

mod support;

use serde_json::{Value, json};
use support::Ui;
use support::pages::{assert_shows, bodies, m, page_driver};

fn three_users() -> Value {
    json!({"users": [
        {"name": "admin", "enabled": true, "createdBy": "coordinator", "createdUnix": "1800000000",
         "lastLoginUnix": "1800000100"},
        {"name": "bob", "mustChange": true, "createdBy": "admin", "createdUnix": "1800000200",
         "passwordValidUntilUnix": "1800003800"},
        {"name": "carol", "createdBy": "admin", "createdUnix": "1800000300"},
    ]})
}

fn created(name: &str, password: &str) -> Value {
    json!({"user": {"name": name, "enabled": true, "mustChange": true, "createdBy": "admin",
                    "createdUnix": "1800000400", "passwordValidUntilUnix": "1800003600"},
           "password": password})
}

#[test]
fn the_users_page_shows_each_user_and_reads_a_missing_enabled_as_off() {
    // Catches: a user whose canonical JSON leaves out `enabled` or `mustChange` (the false of
    // proto3) shown as enabled or as one who must change, a Disable button on a user who is off,
    // an Enable button on one who is on, an unset time shown as a time, and a user's row with a
    // button missing.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[]",
        r#"
const buttons = nodes(tree).filter((n) => n.tag === "tr").slice(1)
  .map((row) => nodes(row).filter((n) => n.tag === "button").map((b) => b.attrs["name"]));
return { rows: rows(tree), buttons };
"#,
    ));
    assert_eq!(bodies(&ui), [(m("Users/List"), json!({}))]);
    assert_eq!(
        out["out"]["rows"],
        json!([
            [
                "Name",
                "Enabled",
                "Must change password",
                "Made by",
                "Made at",
                "Last sign-in",
                "Temporary password valid until",
                ""
            ],
            [
                "admin",
                "yes",
                "no",
                "coordinator",
                "2027-01-15 08:00:00 UTC",
                "2027-01-15 08:01:40 UTC",
                "-",
                "Disable Reset password Delete"
            ],
            [
                "bob",
                "no",
                "yes",
                "admin",
                "2027-01-15 08:03:20 UTC",
                "-",
                "2027-01-15 09:03:20 UTC",
                "Enable Reset password Delete"
            ],
            [
                "carol",
                "no",
                "no",
                "admin",
                "2027-01-15 08:05:00 UTC",
                "-",
                "-",
                "Enable Reset password Delete"
            ],
        ])
    );
    assert_eq!(
        out["out"]["buttons"],
        json!([
            ["disable", "reset-password", "delete"],
            ["enable", "reset-password", "delete"],
            ["enable", "reset-password", "delete"],
        ])
    );
    assert_shows(&out, &["Users (3)", "Make a user"]);
}

#[test]
fn a_user_is_made_under_the_typed_name_and_the_password_is_shown_once() {
    // Catches: an empty name sent, a name sent untrimmed, the password put in the status line
    // (which every redraw keeps), a password that stays after "I have copied it" or after the
    // page is drawn again, and a list not asked for again after the user is made.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    ui.reply(
        &m("Users/Create"),
        created("dave", "Zq3-pw-of-24-characters"),
    );
    ui.reply(&m("Users/List"), three_users());
    ui.reply(&m("Users/List"), three_users());
    ui.reply(&m("Users/List"), three_users());
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[]",
        r#"
await click(tree, "create");
type(tree, "user-name", "  dave ");
await click(tree, "create");
const told = seen.shown[0];
const after = text(told);
await click(told, "dismiss-secret");
const dismissed = text(seen.shown[1]);
const fresh = text(await render(ctx));
return { after, dismissed, fresh, secretTag: named(told, "secret")[0].tag };
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Users/List"), json!({})),
            (m("Users/Create"), json!({"name": "dave"})),
            (m("Users/List"), json!({})),
            (m("Users/List"), json!({})),
            (m("Users/List"), json!({})),
        ]
    );
    let seen = &out["seen"];
    assert_eq!(
        seen["reports"],
        json!(["Name the user.", "Made the user dave."])
    );
    let after = out["out"]["after"].as_str().expect("the page after Create");
    assert!(after.contains("Temporary password of dave."), "{after}");
    assert!(after.contains("Zq3-pw-of-24-characters"), "{after}");
    assert!(
        after.contains("this one stops working after 2027-01-15 09:00:00 UTC."),
        "{after}"
    );
    assert_eq!(out["out"]["secretTag"], "code");
    for later in ["dismissed", "fresh"] {
        let page = out["out"][later].as_str().expect("a page");
        assert!(!page.contains("Zq3-pw"), "{later} still shows it: {page}");
        assert!(page.contains("Users (3)"), "{later}: {page}");
    }
    assert!(!seen["reports"].to_string().contains("Zq3-pw"));
}

#[test]
fn a_refused_create_is_reported_and_tells_no_password() {
    // Catches: a refusal (a name already a user's, a token's or the tokens file's) taken as
    // success, a password shown for a user that was not made, and a list asked for again after
    // a change that did not happen.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    ui.refuse(&m("Users/Create"), 409, "ALREADY_EXISTS", "name_taken");
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[]",
        r#"
type(tree, "user-name", "bob");
await click(tree, "create");
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Users/List"), json!({})),
            (m("Users/Create"), json!({"name": "bob"})),
        ]
    );
    assert_eq!(out["seen"]["reports"], json!(["ALREADY_EXISTS name_taken"]));
    assert_eq!(out["seen"]["shown"], json!([]));
}

#[test]
fn reset_password_asks_first_and_tells_the_new_password_once() {
    // Catches: a reset sent without the operator's yes, one that names another user, the new
    // password missing from the page, and the caller's own name (which the coordinator refuses)
    // shown as a success.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    ui.reply(
        &m("Users/ResetPassword"),
        json!({"user": {"name": "bob", "mustChange": true}, "password": "New-24-character-secret-1"}),
    );
    ui.reply(&m("Users/List"), three_users());
    ui.refuse(
        &m("Users/ResetPassword"),
        412,
        "FAILED_PRECONDITION",
        "own_password",
    );
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[false, true, true]",
        r#"
await click(tree, "reset-password", 1);
await click(tree, "reset-password", 1);
const told = text(seen.shown[0]);
await click(tree, "reset-password", 0);
return told;
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Users/List"), json!({})),
            (m("Users/ResetPassword"), json!({"name": "bob"})),
            (m("Users/List"), json!({})),
            (m("Users/ResetPassword"), json!({"name": "admin"})),
        ]
    );
    let ask = |name: &str| {
        format!(
            "Reset the password of {name}? Their sessions end now and the old password stops working."
        )
    };
    assert_eq!(
        out["seen"]["questions"],
        json!([ask("bob"), ask("bob"), ask("admin")])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not reset.",
            "Reset the password of bob.",
            "FAILED_PRECONDITION own_password"
        ])
    );
    let told = out["out"].as_str().expect("the page after the reset");
    assert!(told.contains("Temporary password of bob."), "{told}");
    assert!(told.contains("New-24-character-secret-1"), "{told}");
    assert!(
        !told.contains("stops working after"),
        "a reply with no deadline names none: {told}"
    );
}

#[test]
fn disable_and_delete_ask_first_and_enable_does_not() {
    // Catches: a disable or a delete sent without the operator's yes (each ends a person's
    // sessions), a question that does not name the user, an enable that asks, a call that names
    // another user, and a refusal of the last enabled user taken as success.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    ui.reply(&m("Users/Disable"), json!({"user": {"name": "admin"}}));
    ui.reply(&m("Users/List"), three_users());
    ui.reply(
        &m("Users/Enable"),
        json!({"user": {"name": "bob", "enabled": true}}),
    );
    ui.reply(&m("Users/List"), three_users());
    ui.reply(&m("Users/Delete"), json!({}));
    ui.reply(&m("Users/List"), three_users());
    ui.refuse(
        &m("Users/Delete"),
        412,
        "FAILED_PRECONDITION",
        "last_enabled",
    );
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[false, true, false, true, true]",
        r#"
await click(tree, "disable", 0);
await click(tree, "disable", 0);
await click(tree, "enable", 0);
await click(tree, "delete", 1);
await click(tree, "delete", 1);
await click(tree, "delete", 0);
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("Users/List"), json!({})),
            (m("Users/Disable"), json!({"name": "admin"})),
            (m("Users/List"), json!({})),
            (m("Users/Enable"), json!({"name": "bob"})),
            (m("Users/List"), json!({})),
            (m("Users/Delete"), json!({"name": "bob"})),
            (m("Users/List"), json!({})),
            (m("Users/Delete"), json!({"name": "admin"})),
        ]
    );
    let disable = "Disable admin? Their sessions end now and they cannot sign in.";
    let delete = |name: &str| {
        format!("Delete {name}? Their sessions end now. The audit entries that name them stay.")
    };
    assert_eq!(
        out["seen"]["questions"],
        json!([
            disable,
            disable,
            delete("bob"),
            delete("bob"),
            delete("admin")
        ])
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not disabled.",
            "Disabled admin.",
            "Enabled bob.",
            "Not deleted.",
            "Deleted bob.",
            "FAILED_PRECONDITION last_enabled"
        ])
    );
}

#[test]
fn a_password_just_told_stays_when_the_list_cannot_be_read_again() {
    // Catches: a password lost because the list failed right after the user was made (the only
    // time it is ever told), and a failed list not shown as the refusal it is.
    let mut ui = Ui::start();
    ui.reply(&m("Users/List"), three_users());
    ui.reply(
        &m("Users/Create"),
        created("erin", "Kept-24-character-secret-2"),
    );
    ui.refuse(&m("Users/List"), 503, "UNAVAILABLE", "store_unreachable");
    let out = ui.run(&page_driver(
        "users",
        "{}",
        "[]",
        r#"
type(tree, "user-name", "erin");
await click(tree, "create");
return text(seen.shown[0]);
"#,
    ));
    let page = out["out"].as_str().expect("the page after Create");
    assert!(page.contains("Kept-24-character-secret-2"), "{page}");
    assert!(page.contains("UNAVAILABLE store_unreachable"), "{page}");
}

#[test]
fn the_users_page_says_why_it_could_not_load() {
    // Catches: a failed list shown as an empty list, which would tell an admin there is no user.
    let mut ui = Ui::start();
    ui.refuse(&m("Users/List"), 401, "UNAUTHENTICATED", "unauthorized");
    let out = ui.run(&page_driver("users", "{}", "[]", ""));
    assert_shows(&out, &["Users", "UNAUTHENTICATED unauthorized"]);
    assert!(!support::pages::shown(&out).contains("No user."));
}

fn three_tokens() -> Value {
    json!({"tokens": [
        {"name": "ops", "source": "API_TOKEN_SOURCE_TOKENS_FILE"},
        {"name": "build-script", "source": "API_TOKEN_SOURCE_STORE", "createdBy": "admin",
         "createdUnix": "1800000000"},
        {"name": "odd", "createdBy": "admin"},
    ]})
}

#[test]
fn the_tokens_page_revokes_only_tokens_of_the_store_and_says_how_to_remove_the_others() {
    // Catches: a Revoke button on a token of the tokens file or on one whose source is not
    // known (the coordinator would refuse it, and the page would promise what it cannot do), a
    // missing note about editing the file, and a token or digest shown.
    let mut ui = Ui::start();
    ui.reply(&m("ApiTokens/List"), three_tokens());
    let out = ui.run(&page_driver(
        "tokens",
        "{}",
        "[]",
        r#"
return rows(tree);
"#,
    ));
    assert_eq!(bodies(&ui), [(m("ApiTokens/List"), json!({}))]);
    assert_eq!(
        out["out"],
        json!([
            ["Name", "Source", "Made by", "Made at", ""],
            ["ops", "tokens file (read-only)", "-", "-", ""],
            [
                "build-script",
                "store",
                "admin",
                "2027-01-15 08:00:00 UTC",
                "Revoke"
            ],
            ["odd", "-", "admin", "-", ""],
        ])
    );
    assert_shows(
        &out,
        &[
            "Tokens (3)",
            "edit that file on the coordinator to remove it",
            "cannot sign in",
        ],
    );
}

#[test]
fn an_api_token_is_made_under_the_typed_name_and_shown_once() {
    // Catches: an empty name sent, a name sent untrimmed, the token put in the status line, a
    // token that stays after "I have copied it" or after the page is drawn again, and a refused
    // name taken as success.
    let mut ui = Ui::start();
    ui.reply(&m("ApiTokens/List"), json!({}));
    ui.reply(
        &m("ApiTokens/Create"),
        json!({"token": {"name": "ci-bot", "source": "API_TOKEN_SOURCE_STORE", "createdBy": "admin"},
               "secret": "hkt_example-secret-not-a-real-one"}),
    );
    ui.reply(&m("ApiTokens/List"), three_tokens());
    ui.reply(&m("ApiTokens/List"), three_tokens());
    ui.reply(&m("ApiTokens/List"), json!({}));
    ui.refuse(&m("ApiTokens/Create"), 409, "ALREADY_EXISTS", "name_taken");
    let out = ui.run(&page_driver(
        "tokens",
        "{}",
        "[]",
        r#"
const empty = text(tree);
await click(tree, "create");
type(tree, "token-name", " ci-bot ");
await click(tree, "create");
const told = seen.shown[0];
const after = text(told);
await click(told, "dismiss-secret");
const dismissed = text(seen.shown[1]);
const fresh = text(await render(ctx));
type(tree, "token-name", "ops");
await click(tree, "create");
return { empty, after, dismissed, fresh };
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/Create"), json!({"name": "ci-bot"})),
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/Create"), json!({"name": "ops"})),
        ]
    );
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Name the token.",
            "Made the token ci-bot.",
            "ALREADY_EXISTS name_taken"
        ])
    );
    let after = out["out"]["after"].as_str().expect("the page after Create");
    assert!(
        after.contains("hkt_example-secret-not-a-real-one"),
        "{after}"
    );
    assert!(after.contains("The token ci-bot."), "{after}");
    assert!(
        out["out"]["empty"]
            .as_str()
            .expect("empty")
            .contains("No token.")
    );
    for later in ["dismissed", "fresh"] {
        let page = out["out"][later].as_str().expect("a page");
        assert!(
            !page.contains("hkt_example"),
            "{later} still shows it: {page}"
        );
    }
    assert!(!out["seen"]["reports"].to_string().contains("hkt_"));
}

#[test]
fn revoke_asks_first_and_names_the_listed_token() {
    // Catches: a revoke sent without the operator's yes, a body that names another token, a
    // question that does not name the token, and a refused revoke taken as success.
    let mut ui = Ui::start();
    ui.reply(&m("ApiTokens/List"), three_tokens());
    ui.reply(&m("ApiTokens/Revoke"), json!({}));
    ui.reply(&m("ApiTokens/List"), three_tokens());
    ui.refuse(&m("ApiTokens/Revoke"), 404, "NOT_FOUND", "unknown_token");
    let out = ui.run(&page_driver(
        "tokens",
        "{}",
        "[false, true, true]",
        r#"
await click(tree, "revoke", 0);
await click(tree, "revoke", 0);
await click(tree, "revoke", 0);
"#,
    ));
    assert_eq!(
        bodies(&ui),
        [
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/Revoke"), json!({"name": "build-script"})),
            (m("ApiTokens/List"), json!({})),
            (m("ApiTokens/Revoke"), json!({"name": "build-script"})),
        ]
    );
    let ask = "Revoke build-script? The token is refused from then on.";
    assert_eq!(out["seen"]["questions"], json!([ask, ask, ask]));
    assert_eq!(
        out["seen"]["reports"],
        json!([
            "Not revoked.",
            "Revoked build-script.",
            "NOT_FOUND unknown_token"
        ])
    );
}
