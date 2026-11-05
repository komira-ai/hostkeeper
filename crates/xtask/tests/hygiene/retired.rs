//! The old design's directory of per-service declarations (`updater` with a `.d` suffix under
//! `/etc`) and the approval store inside it (`approved`) are gone: no tracked file names them
//! but the design history under `docs/design/`, which keeps the old design as it was. The path
//! is built from pieces here so that this file passes its own scan.

use crate::{repo_root, texts, tracked_files};

/// The tracked directory whose files keep the old design as history.
const HISTORY: &str = "docs/design/";

/// The declaration directory of the old design; the approval store is a file inside it.
fn retired() -> String {
    ["/etc/", "updater", ".d"].concat()
}

/// `<path>:<line>: names <retired path>` for each line of the `(path, text)` files that names
/// the retired path, the files under [`HISTORY`] left out.
fn retired_paths(files: &[(String, String)]) -> Vec<String> {
    let retired = retired();
    let mut found = Vec::new();
    for (path, text) in files.iter().filter(|(path, _)| !path.starts_with(HISTORY)) {
        for (index, line) in text.lines().enumerate() {
            if line.contains(&retired) {
                found.push(format!("{path}:{}: names {retired}", index + 1));
            }
        }
    }
    found
}

/// Catches: the old declaration directory or the approval store in it named again by a tracked
/// file outside the design history (a test, a fixture, a README, MIGRATION.md); and a scan that
/// reads nothing, or reads the history too: the scan reports a file that names either path, on
/// the line that names it, and leaves the same text under `docs/design/` alone.
#[test]
fn no_updater_d_or_approved_store_in_the_tree() {
    let root = repo_root();
    let files = texts(&root, tracked_files(&root));
    let retired = retired();
    assert!(
        files
            .iter()
            .any(|(path, text)| path.starts_with(HISTORY) && text.contains(&retired)),
        "the scan read no file of the design history that names {retired}"
    );
    assert_eq!(retired_paths(&files), Vec::<String>::new());

    let named = [
        (
            "README.md".to_string(),
            format!("# hostkeeper\n\nThe store is {retired}/approved.\n"),
        ),
        (
            "crates/x/tests/fixtures/x.toml".to_string(),
            format!("dir = \"{retired}\"\n"),
        ),
        (
            "docs/design/host-updates.md".to_string(),
            format!("Declarations live in {retired}.\n"),
        ),
        (
            "crates/x/src/lib.rs".to_string(),
            "//! /etc/updater and updater.d alone are other names.\n".to_string(),
        ),
    ];
    assert_eq!(
        retired_paths(&named),
        vec![
            format!("README.md:3: names {retired}"),
            format!("crates/x/tests/fixtures/x.toml:1: names {retired}"),
        ]
    );
}
