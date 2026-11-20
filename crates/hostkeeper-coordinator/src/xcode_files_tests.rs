use std::path::Path;

use super::*;
use crate::testutil::scratch;
use crate::xcode_feed::parse;

/// A copy of the 12 newest entries of the Xcode Releases site's `data.json`.
const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/xcode/data.json");

/// Writes `text` as the file at `path`.
fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("a file is written");
}

/// Proves: the file of the list is the state file's name with `.xcode-releases` added.
///
/// Catches: a list written over another store file.
#[test]
fn the_file_is_beside_the_state_file() {
    assert_eq!(
        xcode_releases_path(Path::new("/var/lib/hostkeeper-coordinator/state.json")),
        Path::new("/var/lib/hostkeeper-coordinator/state.json.xcode-releases")
    );
}

/// Proves: a file that does not exist is an empty list; what is kept is read back as the 12
/// releases kept, and the file holds `{"releases": [...]}` with the four fields of each.
///
/// Catches: a missing file that is an error, and a field lost in the file.
#[test]
fn a_list_kept_is_read_back_and_no_file_is_an_empty_list() {
    let dir = scratch("xcode-files-roundtrip");
    let path = dir.path().join("state.json.xcode-releases");
    let releases = FileReleases::new(path.clone());
    assert_eq!(releases.known(), Ok(vec![]));
    let list = parse(FIXTURE).expect("the fixture parses").releases;
    releases.keep(&list).expect("the list is kept");
    assert_eq!(releases.known(), Ok(list));
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("the file")).expect("JSON");
    assert_eq!(file["releases"].as_array().map(Vec::len), Some(12));
    assert_eq!(
        file["releases"][2],
        serde_json::json!({
            "version": "27.2",
            "build": "27B52",
            "channel": "release",
            "requires_macos": "26.6",
        })
    );
}

/// Proves: a list kept replaces the one before whole.
///
/// Catches: a list that is appended to, so a release no longer in the list stays.
#[test]
fn a_list_kept_replaces_the_one_before() {
    let dir = scratch("xcode-files-replace");
    let releases = FileReleases::new(dir.path().join("list"));
    let list = parse(FIXTURE).expect("the fixture parses").releases;
    releases.keep(&list).expect("the list is kept");
    releases.keep(&list[..1]).expect("the list is kept");
    assert_eq!(releases.known(), Ok(list[..1].to_vec()));
}

/// Proves: a file that is not JSON, one that holds a channel or a version no release has, and a
/// path that is a directory are each an error that begins with the file's path and says why.
///
/// Catches: a damaged file read as an empty list, and an error that does not name the file.
#[test]
fn a_file_that_cannot_be_read_is_an_error_naming_it() {
    let dir = scratch("xcode-files-damaged");
    let path = dir.path().join("list");
    let at = path.display();
    let releases = FileReleases::new(path.clone());

    write(&path, "not json");
    let why = releases.known().expect_err("not JSON is not a list");
    assert!(why.starts_with(&format!("{at}: ")), "{why}");

    write(
        &path,
        r#"{"releases":[{"version":"27.2","build":"27B52","channel":"nightly","requires_macos":"26.6"}]}"#,
    );
    assert_eq!(
        releases.known(),
        Err(format!(
            "{at}: \"nightly\" is not a channel of a kept release"
        ))
    );

    write(
        &path,
        r#"{"releases":[{"version":"27.x","build":"27B52","channel":"release","requires_macos":"26.6"}]}"#,
    );
    assert_eq!(
        releases.known(),
        Err(format!(
            "{at}: \"27.x\" is not a version number: dot-separated decimal numbers"
        ))
    );

    let directory = FileReleases::new(dir.path().to_owned());
    let why = directory.known().expect_err("a directory is not a list");
    assert!(
        why.starts_with(&format!("{}: ", dir.path().display())),
        "{why}"
    );
}

/// Proves: a list that cannot be written is an error naming the file, and the file kept before
/// stands.
///
/// Catches: a failed write that is silent.
#[test]
fn a_list_that_cannot_be_written_is_an_error_naming_the_file() {
    let dir = scratch("xcode-files-unwritable");
    let missing = dir.path().join("no-such-directory").join("list");
    let why = FileReleases::new(missing.clone())
        .keep(&[])
        .expect_err("the directory is missing");
    assert!(why.contains(&missing.display().to_string()), "{why}");
}
