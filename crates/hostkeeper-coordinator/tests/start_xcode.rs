//! `serve`'s start over `--xcode-dir`, the directory of the private Xcode store: without the flag
//! there is no store and nothing is made, with a directory the store opens in it (and removes an
//! upload a stopped coordinator left there), and a directory that is not one stops the start,
//! naming the flag.

mod support;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use hostkeeper_coordinator::config::StartError;
use support::serve::{Files, build_given, exit_code, flags_of, refusal};

/// `args` with `--xcode-dir path` added.
fn with_xcode_dir(args: &[OsString], path: &Path) -> Vec<OsString> {
    let mut args = args.to_vec();
    args.extend([OsString::from("--xcode-dir"), path.as_os_str().to_owned()]);
    args
}

/// Proves: a `--xcode-dir` that does not exist, and one that is a file, each stop the start with
/// the text beginning `--xcode-dir: ` and the path, as `StartError::XcodeDir` and exit 1, and the
/// missing directory is not made.
///
/// Catches: a coordinator that starts with a store directory it cannot use, or that makes the
/// directory the operator was to make.
#[test]
fn an_xcode_dir_that_is_missing_or_a_file_stops_the_start_naming_the_flag() {
    let files = Files::new("start-xcode-dir-refused");
    files.put("a-file", "x", 0o644);
    let file = files.path("a-file");
    let missing = files.path("missing");
    for path in [&missing, &file] {
        let given = with_xcode_dir(&files.args(), path);
        let why = refusal(given.clone());
        assert!(matches!(why, StartError::XcodeDir(_)), "{why}");
        let prefix = format!("--xcode-dir: {}: ", path.display());
        assert!(why.to_string().starts_with(&prefix), "{why}");
        assert_eq!(exit_code(given), ExitCode::from(1), "{}", path.display());
    }
    assert!(!missing.exists(), "the start made the directory");
}

/// Proves: a start with an existing `--xcode-dir` succeeds, removes an upload file a stopped
/// coordinator left in it (a name starting `.upload-`, last written over a day ago) and keeps every other file; a start
/// without the flag touches no directory.
///
/// Catches: a start that does not open the store it was given, and one that removes more than a
/// half-written upload.
#[test]
fn a_start_with_an_xcode_dir_opens_the_store_there() {
    let files = Files::new("start-xcode-dir-opens");
    let dir = files.path("xcode");
    std::fs::create_dir(&dir).expect("the directory");
    let left = dir.join(format!(".upload-{}-aa-0", "a".repeat(64)));
    let old = |path: &std::path::Path| {
        std::fs::File::open(path)
            .expect("the upload opens")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(90_000))
            .expect("the time is set");
    };
    std::fs::write(&left, b"half").expect("a left upload");
    old(&left);
    let other = dir.join("notes.txt");
    std::fs::write(&other, b"keep").expect("another file");
    let parts = build_given(&flags_of(with_xcode_dir(&files.args(), &dir))).expect("a directory");
    assert!(!left.exists(), "the left upload stands");
    assert!(other.exists(), "an unrelated file was removed");
    // The parts hold the lock of the state files: the next start waits for them to go.
    drop(parts);
    std::fs::write(&left, b"half").expect("a left upload again");
    old(&left);
    let parts = build_given(&flags_of(files.args())).expect("no flag");
    assert!(
        left.exists(),
        "a start without the flag opened the directory"
    );
    drop(parts);
}
