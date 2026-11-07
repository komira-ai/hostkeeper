use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;
use crate::testdir::TestDir;

/// A directory of dumps, mode `mode`, holding `files` (name, bytes).
fn dumps(name: &str, mode: u32, files: &[(&str, &str)]) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    let dir = TestDir::under(
        &exe.parent().expect("deps").join("xtask-db-backup-unit"),
        name,
    );
    for (file, text) in files {
        std::fs::write(dir.join(file), text).expect("write a file");
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("mode");
    dir
}

/// The files of `dir`, by name, with their text.
fn files(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| {
            let e = e.expect("an entry");
            let text = std::fs::read_to_string(e.path()).expect("read a file");
            (e.file_name().to_string_lossy().into_owned(), text)
        })
        .collect();
    out.sort();
    out
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut out: Vec<_> = pairs
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect();
    out.sort();
    out
}

/// A dump that writes `text`.
fn writes(text: &'static str) -> impl FnOnce(File) -> Result<(), Error> {
    move |mut out| {
        out.write_all(text.as_bytes()).expect("write the dump");
        Ok(())
    }
}

/// Proves: in an empty directory the first dump is `hostkeeper-db-1.dump`, holding exactly what
/// the dump wrote, with mode 0600, and nothing else is left there.
///
/// Catches: a dump others can read, a partial file left beside the dump, or numbering that does
/// not start at 1.
#[test]
fn the_first_dump_of_an_empty_directory_is_number_one_and_owner_only() {
    let dir = dumps("first", 0o700, &[]);
    let lines = backup(&dir, 28, writes("PGDMP one")).expect("a dump");
    assert_eq!(
        lines,
        [
            "db-backup: wrote hostkeeper-db-1.dump, 9 bytes",
            "db-backup: 1 dumps kept, the newest hostkeeper-db-1.dump",
        ]
    );
    assert_eq!(files(&dir), owned(&[("hostkeeper-db-1.dump", "PGDMP one")]));
    let mode = std::fs::metadata(dir.join("hostkeeper-db-1.dump"))
        .expect("the dump")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Proves: a dump is numbered one past the highest dump (not past the count of dumps), the
/// oldest dumps beyond `keep` are removed with the new one counted, a partial file an earlier
/// run left is removed, and files that are not dumps are left alone, including names that only
/// look like one (a leading zero, no number, another suffix).
///
/// Catches: a new dump that overwrites one already there after a gap, a rotation that keeps one
/// too many or removes the newest, or one that removes files it did not write.
#[test]
fn a_dump_is_numbered_past_the_highest_and_the_oldest_beyond_keep_are_removed() {
    let dir = dumps(
        "rotate",
        0o700,
        &[
            ("hostkeeper-db-2.dump", "two"),
            ("hostkeeper-db-5.dump", "five"),
            ("hostkeeper-db-9.dump", "nine"),
            ("hostkeeper-db-3.dump.partial", "cut short"),
            ("hostkeeper-db-07.dump", "a leading zero"),
            ("hostkeeper-db-.dump", "no number"),
            ("hostkeeper-db-x1.dump", "not a number"),
            ("hostkeeper-db-0.dump.partial", "not a partial dump's name"),
            ("hostkeeper-db-4.dump.old", "another suffix"),
            ("notes.txt", "the operator's"),
        ],
    );
    let lines = backup(&dir, 2, writes("ten")).expect("a dump");
    assert_eq!(
        lines,
        [
            "db-backup: removed hostkeeper-db-3.dump.partial, left by a run that did not finish",
            "db-backup: wrote hostkeeper-db-10.dump, 3 bytes",
            "db-backup: removed hostkeeper-db-2.dump",
            "db-backup: removed hostkeeper-db-5.dump",
            "db-backup: 2 dumps kept, the newest hostkeeper-db-10.dump",
        ]
    );
    assert_eq!(
        files(&dir),
        owned(&[
            ("hostkeeper-db-9.dump", "nine"),
            ("hostkeeper-db-10.dump", "ten"),
            ("hostkeeper-db-07.dump", "a leading zero"),
            ("hostkeeper-db-.dump", "no number"),
            ("hostkeeper-db-x1.dump", "not a number"),
            ("hostkeeper-db-0.dump.partial", "not a partial dump's name"),
            ("hostkeeper-db-4.dump.old", "another suffix"),
            ("notes.txt", "the operator's"),
        ])
    );
}

/// Proves: with `keep` 1 only the new dump stays.
///
/// Catches: a rotation off by one at its lower bound.
#[test]
fn keep_one_leaves_only_the_new_dump() {
    let dir = dumps("keep-one", 0o700, &[("hostkeeper-db-1.dump", "one")]);
    let lines = backup(&dir, 1, writes("two")).expect("a dump");
    assert_eq!(
        lines,
        [
            "db-backup: wrote hostkeeper-db-2.dump, 3 bytes",
            "db-backup: removed hostkeeper-db-1.dump",
            "db-backup: 1 dumps kept, the newest hostkeeper-db-2.dump",
        ]
    );
    assert_eq!(files(&dir), owned(&[("hostkeeper-db-2.dump", "two")]));
}

/// Proves: a dump that fails, and one that writes nothing, are refused with their exact text,
/// leave no file, and remove no older dump, even with more dumps than `keep`.
///
/// Catches: an empty or cut-short dump kept as if it were good, or failing runs that rotate the
/// good dumps away until none is left.
#[test]
fn a_failed_or_empty_dump_is_refused_and_removes_nothing() {
    let before = [
        ("hostkeeper-db-1.dump", "one"),
        ("hostkeeper-db-2.dump", "two"),
        ("hostkeeper-db-3.dump", "three"),
    ];
    let dir = dumps("refused", 0o700, &before);
    let failed = backup(&dir, 1, |mut out| {
        out.write_all(b"PGDMP half").expect("write");
        Err(Error::Failed {
            status: "exit status: 1".to_owned(),
        })
    })
    .expect_err("the dump failed");
    assert_eq!(
        failed.to_string(),
        "the dump command ended with exit status: 1; no dump was written"
    );
    assert_eq!(files(&dir), owned(&before));

    let empty = backup(&dir, 1, |_| Ok(())).expect_err("the dump wrote nothing");
    assert_eq!(
        empty.to_string(),
        "the dump command wrote nothing; no dump was written"
    );
    assert_eq!(files(&dir), owned(&before));
}

/// Proves: a directory open to group or others, a file, and a symbolic link to a good directory
/// are refused before the dump runs, with the exact text; a directory that is not there is
/// refused naming it.
///
/// Catches: dumps of every row of the database written where other users can read them, or
/// through a link into another directory.
#[test]
fn a_directory_open_to_others_or_not_a_directory_is_refused_before_the_dump() {
    let never = |_: File| -> Result<(), Error> { panic!("the dump must not run") };
    for mode in [0o750, 0o705, 0o701] {
        let dir = dumps("open", mode, &[]);
        let why = backup(&dir, 1, never).expect_err("refused");
        assert_eq!(
            why.to_string(),
            format!(
                "--into {}: it is open to group or others (mode {mode:04o}); a dump holds every \
                 row of the database, so its directory must be its owner's alone",
                dir.display()
            )
        );
        assert_eq!(files(&dir), owned(&[]));
    }

    let base = dumps("not-a-directory", 0o700, &[("file", "")]);
    std::fs::create_dir(base.join("real")).expect("a directory");
    std::fs::set_permissions(base.join("real"), std::fs::Permissions::from_mode(0o700))
        .expect("mode");
    std::os::unix::fs::symlink(base.join("real"), base.join("link")).expect("a link");
    for name in ["file", "link"] {
        let why = backup(&base.join(name), 1, never).expect_err("refused");
        assert_eq!(
            why.to_string(),
            format!("--into {}: not a directory", base.join(name).display())
        );
    }
    assert_eq!(files(&base.join("real")), owned(&[]));

    let missing = base.join("missing");
    let why = backup(&missing, 1, never).expect_err("refused");
    assert_eq!(
        why.to_string(),
        format!(
            "{}: No such file or directory (os error 2)",
            missing.display()
        )
    );
}

/// Proves: a directory whose highest dump has the largest number there is is refused, and keeps
/// its dumps.
///
/// Catches: a number that wraps round to an old dump's and overwrites it.
#[test]
fn a_directory_at_the_highest_number_is_refused() {
    let last = format!("hostkeeper-db-{}.dump", u64::MAX);
    let dir = dumps("highest", 0o700, &[(&last, "last")]);
    let why = backup(&dir, 1, writes("more")).expect_err("refused");
    assert_eq!(
        why.to_string(),
        format!(
            "--into {}: it holds {last}, the highest number a dump can have",
            dir.display()
        )
    );
    assert_eq!(files(&dir), owned(&[(&last, "last")]));
}

/// Proves: the verb with no command is refused, and one whose program is not there is refused
/// naming it, leaving no file.
///
/// Catches: a timer whose command lost its program reported as a good backup.
#[test]
fn run_refuses_no_command_and_a_program_that_cannot_start() {
    let dir = dumps("start", 0o700, &[]);
    let flags = |command: Vec<OsString>| Flags {
        into: dir.to_path_buf(),
        keep: 28,
        command,
    };
    assert_eq!(
        run(&flags(Vec::new())).expect_err("refused").to_string(),
        "no dump command was given after --; no dump was written"
    );
    let program = dir.join("no-such-program");
    let why = run(&flags(vec![program.clone().into(), "pg_dump".into()])).expect_err("refused");
    assert_eq!(
        why.to_string(),
        format!(
            "the dump command {} could not be started: No such file or directory (os error 2); \
             no dump was written",
            program.display()
        )
    );
    assert_eq!(files(&dir), owned(&[]));
}
