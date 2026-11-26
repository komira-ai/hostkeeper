//! The focus across a draw: `mount` (`ui/view.js`) makes new elements on every draw, and gives the
//! focus, and the caret, back to the control that had it, run in the JavaScript engine against the
//! fake document of `support`.

mod support;

use serde_json::json;
use support::Ui;

/// A driver that mounts `first` in a fake document, runs `before` (with `doc`, `named`, the
/// elements named so in document order, and `outside`, an element not under the root), mounts
/// `second`, and returns what `after` returns. `first` and `second` are expressions of `h`.
fn redraw(first: &str, before: &str, second: &str, after: &str) -> String {
    format!(
        r#"
import {{ elements, fakeDocument }} from "../test/kit.js";
import {{ h, mount }} from "./view.js";
export async function main() {{
  const doc = fakeDocument();
  const named = (name) => elements(doc.root).filter((e) => e.attributes["name"] === name);
  const outside = doc.createElement("input");
  mount(doc, doc.root, {first});
  {before}
  const old = doc.activeElement;
  mount(doc, doc.root, {second});
  {after}
}}"#
    )
}

const TWO_REASONS: &str = r#"h("section", {},
    h("input", { type: "text", name: "reason", value: "a" }),
    h("input", { type: "text", name: "reason", value: "kernel patch" }),
    h("button", { type: "button", name: "update" }, "Update"))"#;

#[test]
fn a_draw_gives_the_focus_and_the_caret_back_to_the_same_control_of_the_new_elements() {
    // Catches: the focus left on the element the draw took out of the page (in a browser, on the
    // body), the first control of a name given the focus when the operator was in the second, the
    // caret moved to the start or the end, and the page scrolled to the control.
    let mut ui = Ui::start();
    let out = ui.run(&redraw(
        TWO_REASONS,
        r#"named("reason")[1].focus();
  named("reason")[1].setSelectionRange(2, 5, "forward");"#,
        TWO_REASONS,
        r#"const now = doc.activeElement;
  return {
    isNew: now !== old,
    isSecond: now === named("reason")[1],
    selection: [now.selectionStart, now.selectionEnd, now.selectionDirection],
    focusedWith: now.focusedWith,
  };"#,
    ));
    assert_eq!(
        out,
        json!({
            "isNew": true,
            "isSecond": true,
            "selection": [2, 5, "forward"],
            "focusedWith": {"preventScroll": true},
        })
    );
}

#[test]
fn the_focus_is_given_back_only_to_a_control_that_is_still_there_and_the_caret_only_to_its_kind() {
    // Catches: a control of the new elements focused when the one that had the focus is gone, when
    // there are fewer of its name, when the focus was on an element with no name or on one outside
    // the page, or when nothing had it; and a caret set on a control of another kind (a browser
    // throws for a button or a date field).
    let mut ui = Ui::start();
    let one_reason = r#"h("section", {}, h("input", { type: "text", name: "reason" }))"#;
    let cases = [
        // The focused control is not in the new elements.
        (TWO_REASONS, r#"named("update")[0].focus();"#, one_reason),
        // The second of its name was focused; the new elements have one.
        (TWO_REASONS, r#"named("reason")[1].focus();"#, one_reason),
        // The focused element has no name.
        (
            r#"h("section", {}, h("input", { type: "text" }), h("input", { type: "text", name: "reason" }))"#,
            r#"doc.root.children[0].children[0].focus();"#,
            r#"h("section", {}, h("input", { type: "text" }), h("input", { type: "text", name: "reason" }))"#,
        ),
        // The focused element is not under the root, though it has a name the page has.
        (
            one_reason,
            r#"outside.setAttribute("name", "reason");
  outside.focus();"#,
            one_reason,
        ),
        // Nothing has the focus.
        (one_reason, "", one_reason),
    ];
    for (first, before, second) in cases {
        let out = ui.run(&redraw(
            first,
            before,
            second,
            r#"return { same: doc.activeElement === old,
           focusedUnder: elements(doc.root).some((e) => e === doc.activeElement) };"#,
        ));
        assert_eq!(
            out,
            json!({"same": true, "focusedUnder": false}),
            "{before}"
        );
    }
    let out = ui.run(&redraw(
        TWO_REASONS,
        r#"named("reason")[0].focus();
  named("reason")[0].setSelectionRange(1, 1, "none");"#,
        r#"h("section", {}, h("button", { type: "button", name: "reason" }, "Reason"))"#,
        r#"const now = doc.activeElement;
  return { isButton: now === named("reason")[0], selection: now.selectionStart === undefined };"#,
    ));
    assert_eq!(out, json!({"isButton": true, "selection": true}));
}

#[test]
fn a_mounted_hook_that_gives_the_focus_elsewhere_has_the_last_word() {
    // Catches: the focus given back after the page's own hooks ran, which would undo a page that
    // moves the focus on purpose (the rack forms' first field).
    let mut ui = Ui::start();
    let second = r#"h("section", {},
    h("input", { type: "text", name: "reason" }),
    h("input", { type: "text", name: "label", on: { mounted: (e) => e.currentTarget.focus() } }))"#;
    let out = ui.run(&redraw(
        TWO_REASONS,
        r#"named("reason")[0].focus();"#,
        second,
        r#"return doc.activeElement === named("label")[0];"#,
    ));
    assert_eq!(out, json!(true));
}
