//! The colours of a server's state in the lab stylesheet (`ui/lab.css`): failed is red, offline is
//! grey and pending is amber, on a row, a rack footprint, its bars, a chip and a server's LED.

use hostkeeper_ui_tests::UiFile;

/// The declaration blocks of `css` as `(selector list, body)`, comments taken out.
fn rules(css: &str) -> Vec<(String, String)> {
    let mut plain = String::new();
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        plain.push_str(&rest[..start]);
        rest = &rest[start..];
        rest = &rest[rest.find("*/").map_or(rest.len(), |end| end + 2)..];
    }
    plain.push_str(rest);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = plain[at..].find('{') {
        let open = at + open;
        let selector = plain[at..open].trim().to_owned();
        if selector.starts_with('@') {
            // A media query: its rules follow inside, up to its own closing brace.
            at = open + 1;
            continue;
        }
        let Some(close) = plain[open..].find('}') else {
            break;
        };
        let close = open + close;
        out.push((selector, plain[open + 1..close].to_owned()));
        at = close + 1;
        // The closing brace of a media query, if this rule was its last.
        while plain[at..].trim_start().starts_with('}') {
            at += plain[at..].find('}').unwrap_or(0) + 1;
        }
    }
    out
}

fn lab_css() -> String {
    UiFile::named("lab.css")
        .expect("lab.css is held")
        .text
        .to_owned()
}

/// The value of custom property `name` where the stylesheet declares it.
fn colour(css: &str, name: &str) -> String {
    let needle = format!("{name}:");
    let at = css
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} is declared"));
    let tail = &css[at + needle.len()..];
    tail[..tail.find(';').expect("a declaration ends")]
        .trim()
        .to_owned()
}

#[test]
fn offline_is_grey_failed_is_red_and_pending_is_amber() {
    // Proves: the three state colours are three different values; every rule for an offline
    // element uses the offline colour (never the failed one), every rule for a pending one the
    // pending colour, and no rule shares a selector list between failed and offline.
    // Catches: an offline server or rack drawn in the red of a failure.
    let css = lab_css();
    let failed = colour(&css, "--led-failed");
    let offline = colour(&css, "--led-offline");
    let pending = colour(&css, "--led-pending");
    assert_ne!(failed, offline);
    assert_ne!(failed, pending);
    assert_ne!(offline, pending);
    let mut seen_offline = 0;
    let mut seen_pending = 0;
    for (selector, body) in rules(&css) {
        let offline_rule = selector.contains("s-offline");
        let pending_rule = selector.contains("s-pending");
        assert!(
            !(selector.contains("s-failed") && (offline_rule || pending_rule)),
            "{selector} shares a rule between failed and another state"
        );
        if offline_rule {
            seen_offline += 1;
            assert!(
                !body.contains("--led-failed") && !body.contains("--led-pending"),
                "{selector}: {body}"
            );
        }
        if pending_rule {
            seen_pending += 1;
            assert!(
                !body.contains("--led-failed") && !body.contains("--led-offline"),
                "{selector}: {body}"
            );
        }
    }
    // The row, the footprint, its bar and its dot, and the server on the rack, each have one.
    assert!(seen_offline >= 5, "{seen_offline} offline rules");
    assert!(seen_pending >= 5, "{seen_pending} pending rules");
}

#[test]
fn an_offline_led_is_a_hollow_ring_in_the_offline_colour() {
    // Proves: the LED of an offline server has no fill and a ring of `--led-offline`.
    // Catches: an offline LED that looks like a lit one.
    let css = lab_css();
    let (_, body) = rules(&css)
        .into_iter()
        .find(|(selector, _)| selector == ".s-offline > .led")
        .expect("a rule for the LED of an offline server");
    assert!(body.contains("background: transparent"), "{body}");
    assert!(
        body.contains("inset 0 0 0 1.5px var(--led-offline)"),
        "{body}"
    );
}

#[test]
fn the_phone_breadcrumbs_are_spaced_below_the_top_bar() {
    // Proves: below 900 px the breadcrumbs have padding above them.
    // Catches: breadcrumbs set tight under the top bar on a phone.
    let css = lab_css();
    let (_, body) = rules(&css)
        .into_iter()
        .find(|(selector, body)| selector == ".crumbs" && body.contains("padding-top"))
        .expect("a .crumbs rule with padding-top");
    assert!(body.contains("padding-top: 14px"), "{body}");
    let at = css.find(&body).expect("the rule is in the file");
    let media = css[..at].rfind("@media").expect("it sits in a media query");
    assert!(css[media..at].starts_with("@media (max-width: 899px)"));
}

#[test]
fn the_packages_table_rule_is_not_split_by_collapsed_borders() {
    // Proves: the packages tables use separate borders and the row buttons have one fixed height.
    // Catches: the rule under a row drawn half a pixel off in the cell of Edit and Remove.
    let css = UiFile::named("packages.css")
        .expect("packages.css is held")
        .text;
    let table = rules(css)
        .into_iter()
        .find(|(selector, _)| selector == ".packages table")
        .expect("a rule for the table")
        .1;
    assert!(table.contains("border-collapse: separate"), "{table}");
    let buttons = rules(css)
        .into_iter()
        .find(|(selector, _)| selector == ".packages td.row-actions button")
        .expect("a rule for the buttons of a row")
        .1;
    assert!(buttons.contains("height: 30px"), "{buttons}");
}

#[test]
fn the_danger_button_is_red_in_every_page_not_voided_by_an_undeclared_colour() {
    // Proves: no `button.danger` rule of the app stylesheet reads a custom property the stylesheet
    // never declares, so the red of `--bad` is not overridden by an empty value (black text).
    // Catches: a later `button.danger { color: var(--alert) }` that turned every Remove black.
    let css = UiFile::named("app.css").expect("app.css is held").text;
    for (selector, body) in rules(css) {
        if !selector.starts_with("button.danger") {
            continue;
        }
        for part in body.split("var(--").skip(1) {
            let name = &part[..part.find(')').expect("a var( closes")];
            assert!(
                css.contains(&format!("--{name}:")),
                "{selector} reads --{name}, which no rule declares"
            );
        }
    }
}
