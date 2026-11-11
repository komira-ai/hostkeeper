//! The reader of the ABM takeover: the file it writes, who can read it, and what it refuses.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::{Error, Flags, run};
use crate::testdir::TestDir;

fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-abm-takeover-unit"),
        name,
    )
}

fn write_parts(dir: &Path, name: &str) {
    for (part, text) in [
        ("consumer_key", "CK-value\n"),
        ("consumer_secret", "  CS-value  "),
        ("access_token", "AT-value\n\n"),
        ("access_secret", "AS-value"),
        ("access_token_expiry", "2027-10-31T22:01:36Z\n"),
    ] {
        std::fs::write(dir.join(format!("{part}.{name}")), text).expect("write a part");
    }
}

fn flags(dir: &Path, name: &str, out: &Path) -> Flags {
    Flags {
        dep_dir: dir.to_owned(),
        name: name.to_owned(),
        out: out.to_owned(),
    }
}

/// Proves: the five files become the one JSON object `Mdm.PutAbmToken` takes, each part trimmed
/// of the whitespace around it, in a file only its owner reads, and the line the verb prints
/// names the file and none of the values.
///
/// Catches: a part written with its newline, a token file other users can read, and a report that
/// repeats a secret.
#[test]
fn the_five_parts_become_the_json_the_upload_takes() {
    let dir = scratch("abm-takeover-ok");
    write_parts(&dir, "js-mdm");
    let out = dir.join("token.json");
    let lines = run(&flags(&dir, "js-mdm", &out)).expect("the token is written");
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).expect("read")).expect("json");
    assert_eq!(
        written,
        serde_json::json!({
            "consumer_key": "CK-value",
            "consumer_secret": "CS-value",
            "access_token": "AT-value",
            "access_secret": "AS-value",
            "access_secret_expiry": "2027-10-31T22:01:36Z",
        })
    );
    let mode = std::fs::metadata(&out)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with(&format!("wrote {}", out.display())));
    for value in ["CK-value", "CS-value", "AT-value", "AS-value", "2027-10-31"] {
        assert!(!lines[0].contains(value), "the report shows {value}");
    }
}

/// Proves: a missing file, an empty one and a name that cannot be a file suffix are refused with
/// the file named and no value shown; an output that exists is not overwritten and keeps its
/// bytes; and nothing is written when any part is refused.
///
/// Catches: a token assembled from the parts that were there, and an upload file clobbered.
#[test]
fn a_part_that_is_missing_or_empty_or_an_output_that_exists_is_refused() {
    let dir = scratch("abm-takeover-refused");
    write_parts(&dir, "js-mdm");
    let out = dir.join("token.json");

    std::fs::remove_file(dir.join("access_secret.js-mdm")).expect("remove");
    let missing = run(&flags(&dir, "js-mdm", &out)).expect_err("a part is missing");
    let Error::Part { path, why } = &missing else {
        panic!("not a part error: {missing:?}");
    };
    assert_eq!(
        path,
        &dir.join("access_secret.js-mdm").display().to_string()
    );
    assert_eq!(why, "entity not found");
    assert!(!out.exists());

    std::fs::write(dir.join("access_secret.js-mdm"), " \n").expect("write");
    let empty = run(&flags(&dir, "js-mdm", &out)).expect_err("a part is empty");
    assert_eq!(
        empty,
        Error::Part {
            path: dir.join("access_secret.js-mdm").display().to_string(),
            why: "is empty".to_owned(),
        }
    );
    assert!(!out.exists());

    for bad in ["", "a/b", "..", "x y", &"n".repeat(65)] {
        let refused = run(&flags(&dir, bad, &out)).expect_err("a bad name");
        assert_eq!(refused, Error::Name(bad.to_owned()));
    }

    std::fs::write(dir.join("access_secret.js-mdm"), "AS-value").expect("write");
    std::fs::write(&out, "keep").expect("write the output");
    let exists = run(&flags(&dir, "js-mdm", &out)).expect_err("the output exists");
    let Error::Out { path, .. } = &exists else {
        panic!("not an output error: {exists:?}");
    };
    assert_eq!(path, &out.display().to_string());
    assert_eq!(std::fs::read(&out).expect("read"), b"keep");
}
