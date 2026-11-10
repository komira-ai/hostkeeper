//! The settings file: how its tables expand into flags, what is refused, and the trust the file
//! must have.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;
use crate::testing::scratch;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// Strings and integers become `--key value`, an array one pair per element in order, and an
/// absent `[prepare]` expands to nothing.
///
/// Catches an array collapsed to its first element, an integer dropped, and a `[prepare]` table
/// read into the flags of `serve`.
#[test]
fn a_string_an_integer_and_an_array_expand_to_flags() {
    let settings = parse(
        "[serve]\nmode = \"router\"\nuser-id = 65534\nreinstall = [\"web-1\", \"web-2\"]\n\
         [prepare]\nplatform = [\"linux-arm64\"]\ndownload-timeout-secs = 600\n",
    )
    .expect("a settings file");
    assert_eq!(
        settings.serve,
        args(&[
            "--mode",
            "router",
            "--reinstall",
            "web-1",
            "--reinstall",
            "web-2",
            "--user-id",
            "65534"
        ])
    );
    assert_eq!(
        settings.prepare,
        args(&[
            "--download-timeout-secs",
            "600",
            "--platform",
            "linux-arm64"
        ])
    );
    let bare = parse("[serve]\nmode = \"proxy\"\nreinstall = []\n").expect("no prepare table");
    assert_eq!(bare.serve, args(&["--mode", "proxy"]));
    assert!(bare.prepare.is_empty());
}

/// Each file that is not a settings file is refused with the line that says why.
///
/// Catches a key of another table, a value of a type no flag takes, and a missing `[serve]`
/// being read as a file of no flags.
#[test]
fn a_file_that_is_not_settings_is_refused_by_name() {
    let cases = [
        (
            "mode = \"proxy\"\n",
            "key \"mode\" is not a table of this file: only [serve] and [prepare]",
        ),
        ("[prepare]\ngh = \"/x\"\n", "there is no [serve] table"),
        ("serve = 1\n", "\"serve\" must be a table"),
        ("[serve]\nmode = \"proxy\"\n[prepare]\n", ""),
        (
            "[serve]\nbad = true\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve]\nbad = 1.5\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve.bad]\nx = 1\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve]\nbad = [true]\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve]\nbad = [[1]]\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve]\nbad = 1979-05-27\n",
            "[serve] \"bad\": a value is a string or an integer, or an array of them",
        ),
        (
            "[serve]\nmode = \"proxy\"\n[prepare]\nbad = false\n",
            "[prepare] \"bad\": a value is a string or an integer, or an array of them",
        ),
    ];
    for (text, why) in cases {
        match (parse(text), why) {
            (Ok(_), "") => {}
            (Err(e), why) => assert_eq!(e, why, "{text}"),
            (Ok(_), _) => panic!("{text} was accepted"),
        }
    }
    let error = parse("[serve\n").expect_err("not TOML");
    assert!(!error.is_empty());
    let error = parse("prepare = 3\n[serve]\nmode = \"proxy\"\n").expect_err("not a table");
    assert_eq!(error, "\"prepare\" must be a table");
}

/// The file is read with the trust of a config: its owner, its mode, UTF-8.
///
/// Catches a settings file that anyone could have written being obeyed as root's.
#[test]
fn the_file_must_be_the_owners_and_not_writable_by_others() {
    let dir = scratch("settings");
    let path = dir.path().join("pxe.toml");
    fs::write(&path, "[serve]\nmode = \"proxy\"\n").expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let owner = fs::metadata(&path).expect("meta").uid();
    let shown = path.display();
    assert_eq!(
        read(&path, owner).expect("trusted").serve,
        args(&["--mode", "proxy"])
    );
    let other = read(&path, owner + 1).expect_err("another owner");
    assert!(other.starts_with(&format!("{shown}: ")), "{other}");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).expect("mode");
    let writable = read(&path, owner).expect_err("group writable");
    assert!(writable.starts_with(&format!("{shown}: ")), "{writable}");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    fs::write(&path, [0xff, 0xfe]).expect("write");
    assert_eq!(read(&path, owner), Err(format!("{shown}: not UTF-8")));
    fs::write(&path, "[prepare]\n").expect("write");
    assert_eq!(
        read(&path, owner),
        Err(format!("{shown}: there is no [serve] table"))
    );
    let missing = read(&dir.path().join("none.toml"), owner).expect_err("missing");
    assert!(missing.contains("none.toml: "), "{missing}");
}
