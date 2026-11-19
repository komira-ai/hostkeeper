use super::*;

/// The SHA-256 of the empty text and of `abc` (FIPS 180-2's first example).
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn file(path: &'static str, text: &'static str) -> UiFile {
    UiFile { path, text }
}

fn pin(path: &str, sha256: &str, line: usize) -> Pin {
    Pin {
        path: path.to_owned(),
        sha256: sha256.to_owned(),
        line,
    }
}

fn whats(found: &[Finding]) -> Vec<String> {
    found.iter().map(ToString::to_string).collect()
}

#[test]
fn a_digest_is_sha256_in_lower_case_hex() {
    assert_eq!(sha256_hex(b""), EMPTY);
    assert_eq!(sha256_hex(b"abc"), ABC);
}

#[test]
fn only_paths_under_the_vendor_directory_are_vendored() {
    assert!(is_vendored("vendor/xterm/xterm.js"));
    for path in [
        "main.js",
        "pages/vendor/x.js",
        "vendors/x.js",
        "Vendor/x.js",
    ] {
        assert!(!is_vendored(path), "{path}");
    }
}

#[test]
fn pins_read_sha256sum_lines_and_skip_comments_and_blank_lines() {
    let text = format!(
        "# xterm.js 5.5.0 (MIT)\n{ABC}  vendor/xterm/xterm.js\n\n# noVNC\n{EMPTY}  vendor/novnc/core/rfb.js\n"
    );
    assert_eq!(
        pins(&text),
        Ok(vec![
            pin("vendor/xterm/xterm.js", ABC, 2),
            pin("vendor/novnc/core/rfb.js", EMPTY, 5),
        ])
    );
    assert_eq!(pins(""), Ok(vec![]));
    assert_eq!(pins(PINS).map(|p| p.len()), Ok(0));
}

#[test]
fn a_pin_line_of_another_shape_is_refused_with_its_line() {
    // Catches: a short, long or upper-case digest taken as a pin, one space or a tab for the two,
    // a path with white space in it or none at all, and a `sha256sum -b` star read as part of the
    // path.
    let upper = ABC.to_ascii_uppercase();
    let short = &ABC[..63];
    let long = format!("{ABC}0");
    let not_hex = format!("{}g", &ABC[..63]);
    for bad in [
        format!("{ABC} vendor/x.js"),
        format!("{ABC}\tvendor/x.js"),
        format!("{upper}  vendor/x.js"),
        format!("{short}  vendor/x.js"),
        format!("{long}  vendor/x.js"),
        format!("{not_hex}  vendor/x.js"),
        format!("{ABC}  "),
        format!("{ABC}  vendor/a b.js"),
        format!("{ABC}  vendor/x.js "),
        format!("{ABC}   vendor/x.js"),
        format!(" {ABC}  vendor/x.js"),
        "vendor/x.js".to_owned(),
    ] {
        let text = format!("# one\n{bad}\n");
        assert_eq!(
            pins(&text),
            Err(PinError::Malformed {
                line: 2,
                text: bad.clone()
            }),
            "{bad}"
        );
    }
    let star = format!("{ABC}  *vendor/x.js");
    assert_eq!(
        pins(&star),
        Err(PinError::Outside {
            line: 1,
            path: "*vendor/x.js".to_owned()
        })
    );
    assert_eq!(
        PinError::Malformed {
            line: 3,
            text: "x".to_owned()
        }
        .to_string(),
        "crates/hostkeeper-ui-tests/vendor.sha256:3: not `<SHA-256 in 64 lower-case hex digits>  vendor/<path>`: \"x\""
    );
}

#[test]
fn a_pin_outside_the_vendor_directory_or_of_no_file_is_refused() {
    // Catches: a pin that would let an app file (exempt from nothing) or a file outside ui/ be
    // pinned, and a path that climbs out of the directory or names the directory itself.
    for path in [
        "main.js",
        "vendor/",
        "vendor",
        "vendor/x/",
        "vendor//x.js",
        "vendor/./x.js",
        "vendor/../main.js",
        "vendor/x/../../main.js",
        "/vendor/x.js",
        "pages/vendor/x.js",
    ] {
        assert_eq!(
            pins(&format!("{ABC}  {path}")),
            Err(PinError::Outside {
                line: 1,
                path: path.to_owned()
            }),
            "{path}"
        );
    }
    assert_eq!(
        PinError::Outside {
            line: 1,
            path: "main.js".to_owned()
        }
        .to_string(),
        "crates/hostkeeper-ui-tests/vendor.sha256:1: \"main.js\" is not a file below vendor/"
    );
}

#[test]
fn a_file_pinned_twice_is_refused_at_its_second_line() {
    // Catches: two pins of one file, which would let the second (or either) digest pass.
    let text = format!("{ABC}  vendor/x.js\n{EMPTY}  vendor/y.js\n{EMPTY}  vendor/x.js\n");
    let refused = pins(&text);
    assert_eq!(
        refused,
        Err(PinError::Twice {
            line: 3,
            path: "vendor/x.js".to_owned()
        })
    );
    assert_eq!(
        refused.map_err(|e| e.to_string()),
        Err("crates/hostkeeper-ui-tests/vendor.sha256:3: vendor/x.js is pinned twice".to_owned())
    );
}

#[test]
fn vendored_files_that_match_their_pins_pass() {
    let files = [
        file("main.js", "anything"),
        file("vendor/a.js", "abc"),
        file("vendor/lib/b.css", ""),
    ];
    let pins = [
        pin("vendor/lib/b.css", EMPTY, 4),
        pin("vendor/a.js", ABC, 2),
    ];
    assert!(unpinned(&files, &pins).is_empty());
    assert!(unpinned(&[file("main.js", "x")], &[]).is_empty());
}

#[test]
fn a_changed_unpinned_or_missing_vendored_file_is_found() {
    // Catches: a vendored file edited without its pin (one byte added), a file added to the
    // directory with no pin, and a pin left behind for a file removed, each named exactly.
    let files = [
        file("main.js", "not vendored, so not pinned"),
        file("vendor/a.js", "abc "),
        file("vendor/new.js", "x"),
    ];
    let pins = [pin("vendor/a.js", ABC, 2), pin("vendor/gone.js", EMPTY, 7)];
    let a_js = sha256_hex(b"abc ");
    assert_eq!(
        whats(&unpinned(&files, &pins)),
        [
            format!(
                "crates/hostkeeper-ui-tests/vendor.sha256:2: vendor/a.js has SHA-256 {a_js}, not the {ABC} pinned"
            ),
            "vendor/new.js:1: a vendored file no line of crates/hostkeeper-ui-tests/vendor.sha256 pins"
                .to_owned(),
            "crates/hostkeeper-ui-tests/vendor.sha256:7: pins vendor/gone.js, which ui/ does not hold"
                .to_owned(),
        ]
    );
}

#[test]
fn a_pin_of_an_app_file_is_not_a_file_of_the_directory() {
    // Catches: a pin matched against a file of another directory with the same name.
    let files = [file("x.js", "abc")];
    assert_eq!(
        whats(&unpinned(&files, &[pin("vendor/x.js", ABC, 1)])),
        ["crates/hostkeeper-ui-tests/vendor.sha256:1: pins vendor/x.js, which ui/ does not hold"]
    );
}
