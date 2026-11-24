//! The outputs of a run (`--output`), its `--env` and its `--summary`, against the fake farm:
//! which files come back, where, with which mode, and how a result that does not hold them, a
//! blob that is not its digest or a file that cannot be written ends the run.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::fake::{Answer, answered};
use super::farm_tests::{Rig, exited, failure};
use super::outputs::{self, OutputError, PART};
use super::reapi::v2::{ActionResult, Digest, OutputDirectory, OutputFile, OutputSymlink};
use super::tests::scratch;
use super::tree::digest_of;
use super::{Args, Error};

fn file(path: &str, digest: &Digest, executable: bool) -> OutputFile {
    OutputFile {
        path: path.to_owned(),
        digest: Some(digest.clone()),
        is_executable: executable,
        ..OutputFile::default()
    }
}

fn wanted(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

/// A passing answer whose result holds `files`.
fn with_files(files: Vec<OutputFile>) -> Answer {
    let mut response = exited(0, None, None);
    if let Some(result) = response.result.as_mut() {
        result.output_files = files;
    }
    Answer::Updates(vec![answered("op/out", &response)])
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// The paths below `dir` whose names end in [`PART`].
fn parts(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_owned()];
    while let Some(at) = todo.pop() {
        for entry in std::fs::read_dir(&at).expect("list").flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if path.to_string_lossy().ends_with(PART) {
                found.push(path.display().to_string());
            }
        }
    }
    found
}

/// Catches an output left on the Mac (not written back), written to another path than the one
/// it was asked by, its executable bit lost or a plain file made executable, a parent directory
/// not made, a temporary file left beside it, the outputs not the Command's `output_paths`, an
/// `--env` that never reached the Command, and a summary that replaces the file instead of
/// appending to it (a job's summary holds its other steps' too).
#[test]
fn each_output_is_written_into_the_checkout_at_its_path_with_its_mode() {
    let rig = Rig::new();
    let program = vec![7_u8; 3 << 20];
    let program_digest = rig.put(&program);
    let notes_digest = rig.put(b"notes\n");
    let empty_digest = digest_of(b"");
    rig.state.queue(with_files(vec![
        file("target/release/hk", &program_digest, true),
        file("target/notes.txt", &notes_digest, false),
        file("target/empty", &empty_digest, false),
    ]));
    let dir = scratch("outputs");
    let root = dir.join("checkout");
    let summary = dir.join("summary.md");
    std::fs::write(&summary, "earlier step\n").expect("write");
    let args = Args {
        outputs: wanted(&["target/release/hk", "target/notes.txt", "target/empty"]),
        env: wanted(&["RELEASE_COMMIT=0123"]),
        summary: Some(summary.clone()),
        ..rig.args(&dir.join("scratch"))
    };
    let (result, _, _) = rig.run_args(&args, &root);
    assert_eq!(
        result.expect("ok"),
        [
            "ok: `cargo test -p hello` passed on a farm Mac",
            "mac-farm: wrote the output target/empty (0 bytes)",
            "mac-farm: wrote the output target/notes.txt (6 bytes)",
            "mac-farm: wrote the output target/release/hk (3145728 bytes)",
        ]
    );
    let hk = root.join("target/release/hk");
    assert_eq!(std::fs::read(&hk).expect("hk"), program);
    assert_eq!(mode(&hk), 0o755);
    let notes = root.join("target/notes.txt");
    assert_eq!(std::fs::read(&notes).expect("notes"), b"notes\n");
    assert_eq!(mode(&notes), 0o644);
    assert!(
        std::fs::read(root.join("target/empty"))
            .expect("empty")
            .is_empty()
    );
    assert_eq!(parts(&root), Vec::<String>::new());
    let (_, command) = rig.executed(0);
    assert_eq!(
        command.output_paths,
        ["target/empty", "target/notes.txt", "target/release/hk"]
    );
    assert!(
        command
            .environment_variables
            .iter()
            .any(|v| v.name == "RELEASE_COMMIT" && v.value == "0123")
    );
    let text = std::fs::read_to_string(&summary).expect("summary");
    assert!(
        text.starts_with("earlier step\n### mac-farm: `cargo test -p hello`\n\n- mac-farm: "),
        "{text}"
    );
    assert!(
        text.ends_with(
            "- ok: `cargo test -p hello` passed on a farm Mac\n\
             - mac-farm: wrote the output target/empty (0 bytes)\n\
             - mac-farm: wrote the output target/notes.txt (6 bytes)\n\
             - mac-farm: wrote the output target/release/hk (3145728 bytes)\n\n"
        ),
        "{text}"
    );
}

/// Catches outputs read from a command that failed (its files are not a build's), and a
/// failure not recorded in the summary.
#[test]
fn a_command_that_failed_writes_no_output_and_the_summary_says_why() {
    let rig = Rig::new();
    let digest = rig.put(b"half a program");
    let mut response = exited(101, None, None);
    if let Some(result) = response.result.as_mut() {
        result.output_files = vec![file("target/hk", &digest, true)];
    }
    rig.state
        .queue(Answer::Updates(vec![answered("op/f", &response)]));
    let dir = scratch("failed-outputs");
    let root = dir.join("checkout");
    let summary = dir.join("summary.md");
    let args = Args {
        outputs: wanted(&["target/hk"]),
        summary: Some(summary.clone()),
        ..rig.args(&dir.join("scratch"))
    };
    let (result, _, _) = rig.run_args(&args, &root);
    assert_eq!(
        failure(result),
        "`cargo test -p hello` exited 101 on a farm Mac"
    );
    assert!(!root.join("target").exists());
    let text = std::fs::read_to_string(&summary).expect("summary");
    assert!(
        text.ends_with("- failed: `cargo test -p hello` exited 101 on a farm Mac\n\n"),
        "{text}"
    );
}

/// Catches a summary that cannot be written passing as if it were (the job would lose its
/// record), the command's own verdict notwithstanding.
#[test]
fn a_summary_that_cannot_be_written_fails_the_run() {
    let rig = Rig::new();
    rig.state.queue(with_files(Vec::new()));
    let dir = scratch("summary-dir");
    let args = Args {
        summary: Some(dir.to_path_buf()),
        ..rig.args(&dir.join("scratch"))
    };
    let (result, _, _) = rig.run_args(&args, &dir.join("checkout"));
    let failed = failure(result);
    assert!(
        failed.starts_with(&format!("cannot append to {}: ", dir.display())),
        "{failed}"
    );
}

/// Catches each result that does not hold the asked outputs as files taken as one that does:
/// an output missing, an output that is a directory or a symlink (by REAPI 2.1's field or the
/// older ones), a file no output names, a file named twice, and a file with no digest; and a
/// directory or symlink nobody asked for refused (it is the build's business).
#[test]
fn a_result_that_does_not_hold_the_outputs_as_files_is_refused() {
    let d = digest_of(b"x");
    let result = |files: Vec<OutputFile>| ActionResult {
        output_files: files,
        ..ActionResult::default()
    };
    let asked = wanted(&["a", "b"]);
    assert_eq!(
        outputs::files(&asked, &result(vec![file("a", &d, false)])),
        Err(OutputError::Missing("b".to_owned()))
    );
    let dir = ActionResult {
        output_directories: vec![OutputDirectory {
            path: "b".to_owned(),
            ..OutputDirectory::default()
        }],
        ..result(vec![file("a", &d, false)])
    };
    assert_eq!(
        outputs::files(&asked, &dir),
        Err(OutputError::NotAFile("b".to_owned(), "directory"))
    );
    let link = |path: &str| OutputSymlink {
        path: path.to_owned(),
        target: "elsewhere".to_owned(),
        ..OutputSymlink::default()
    };
    #[allow(
        deprecated,
        reason = "REAPI before 2.1 answers a symlink output in these two fields"
    )]
    let links = [
        ActionResult {
            output_symlinks: vec![link("b")],
            ..result(vec![file("a", &d, false)])
        },
        ActionResult {
            output_file_symlinks: vec![link("b")],
            ..result(vec![file("a", &d, false)])
        },
        ActionResult {
            output_directory_symlinks: vec![link("b")],
            ..result(vec![file("a", &d, false)])
        },
    ];
    for links in links {
        assert_eq!(
            outputs::files(&asked, &links),
            Err(OutputError::NotAFile("b".to_owned(), "symlink"))
        );
    }
    assert_eq!(
        outputs::files(
            &asked,
            &result(vec![file("a", &d, false), file("c", &d, false)])
        ),
        Err(OutputError::Unasked("c".to_owned()))
    );
    assert_eq!(
        outputs::files(
            &asked,
            &result(vec![file("a", &d, false), file("a", &d, true)])
        ),
        Err(OutputError::Twice("a".to_owned()))
    );
    let no_digest = OutputFile {
        path: "a".to_owned(),
        ..OutputFile::default()
    };
    assert_eq!(
        outputs::files(&asked, &result(vec![no_digest])),
        Err(OutputError::NoDigest {
            path: "a".to_owned()
        })
    );
    let others = ActionResult {
        output_directories: vec![OutputDirectory {
            path: "target".to_owned(),
            ..OutputDirectory::default()
        }],
        output_symlinks: vec![link("target/link")],
        ..result(vec![file("b", &d, true), file("a", &d, false)])
    };
    assert_eq!(
        outputs::files(&asked, &others),
        Ok(BTreeMap::from([
            ("a".to_owned(), (d.clone(), false)),
            ("b".to_owned(), (d.clone(), true)),
        ]))
    );
}

/// Catches an output the farm cannot serve, or serves as other bytes than its digest (the
/// wrong blob, or the right one cut short), written into the checkout anyway; and a failure
/// that leaves the outputs read before it in place (a half-written set of programs).
#[test]
fn an_output_that_is_not_its_digest_is_not_written() {
    let rig = Rig::new();
    let first = rig.put(b"first");
    let gone = digest_of(b"never stored");
    rig.state.queue(with_files(vec![
        file("a/first", &first, false),
        file("b/gone", &gone, false),
    ]));
    let dir = scratch("not-digest");
    let root = dir.join("checkout");
    let args = Args {
        outputs: wanted(&["a/first", "b/gone"]),
        ..rig.args(&dir.join("scratch"))
    };
    let (result, _, _) = rig.run_args(&args, &root);
    let failed = failure(result);
    assert!(
        failed.starts_with(
            "the output b/gone could not be read from the farm: ByteStream Read: NotFound: "
        ),
        "{failed}"
    );
    assert!(!root.join("a/first").exists());
    let lie = digest_of(b"the real bytes");
    rig.state
        .blobs
        .lock()
        .expect("blobs")
        .insert(lie.hash.clone(), b"other bytes!!!".to_vec());
    rig.state
        .queue(with_files(vec![file("a/lie", &lie, false)]));
    let args = Args {
        outputs: wanted(&["a/lie"]),
        ..rig.args(&dir.join("scratch"))
    };
    let (result, _, _) = rig.run_args(&args, &root);
    assert_eq!(
        result,
        Err(Error::Output(OutputError::Digest {
            path: "a/lie".to_owned(),
            got: format!("{}/14", digest_of(b"other bytes!!!").hash),
            want: format!("{}/14", lie.hash),
        }))
    );
    assert!(!root.join("a/lie").exists());
}

/// Catches an output whose directory or file cannot be made, or whose place is taken by a
/// directory, reported as written.
#[test]
fn an_output_that_cannot_be_written_fails_the_run() {
    let rig = Rig::new();
    let digest = rig.put(b"bytes");
    let dir = scratch("unwritable");
    let root = dir.join("checkout");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("blocked"), "a file where a directory goes").expect("write");
    std::fs::create_dir_all(root.join("taken/inside")).expect("mkdir");
    std::fs::create_dir_all(root.join("part")).expect("mkdir");
    std::fs::create_dir_all(root.join(format!("part/x{PART}"))).expect("mkdir");
    for (path, starts) in [
        ("blocked/x", "cannot create "),
        ("taken", "cannot rename "),
        ("part/x", "cannot create "),
    ] {
        rig.state
            .queue(with_files(vec![file(path, &digest, false)]));
        let args = Args {
            outputs: wanted(&[path]),
            ..rig.args(&dir.join("scratch"))
        };
        let (result, _, _) = rig.run_args(&args, &root);
        let failed = failure(result);
        assert!(failed.starts_with(starts), "{path}: {failed}");
    }
}

/// A writer that takes nothing.
struct Refusing;

impl std::io::Write for Refusing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("the disk is full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Catches a blob that could not be written where it streams taken as written.
#[test]
fn a_blob_that_cannot_be_written_where_it_streams_fails() {
    let rig = Rig::new();
    let farm = rig.farm("");
    let digest = rig.put(b"bytes");
    let got = rig.runtime.block_on(farm.read_into(&digest, &mut Refusing));
    assert_eq!(
        got.expect_err("refused").to_string(),
        "a blob read from the farm could not be written: the disk is full"
    );
}
