//! The daemon's log directory and the newsyslog file that rotates its output files: what the pkg
//! installs, the text it installs, and what `check-macos-pkg` refuses.

use super::*;

/// The newsyslog file as it is committed: what the pkg installs for the daemon's output files.
const GOLDEN: &str = include_str!("../../../../tests/release/hostkeeper.newsyslog.conf");

/// The index in `good_entries` of the newsyslog file and of the log directory.
const NEWSYSLOG_AT: usize = 7;
const LOG_DIR_AT: usize = 10;

#[test]
fn the_payload_holds_the_log_directory_and_the_newsyslog_file_owned_by_root() {
    // Catches: a pkg that leaves launchd's StandardOutPath without its directory, so the
    // daemon's output is lost on a fresh Mac, or one that installs no rotation for it.
    let want = payload(BINARY.to_vec(), b"plist".to_vec());
    assert_eq!(want[LOG_DIR], Want::Dir);
    assert_eq!(
        want[NEWSYSLOG],
        Want::File {
            mode: 0o644,
            bytes: GOLDEN.as_bytes().to_vec()
        }
    );
    // Through macOS's links into `/private`, the two are the paths launchd and newsyslog use.
    assert_eq!(log_dir_path(), LOG_DIR);
    assert_eq!(format!("/{LOG_DIR}"), format!("/private{DAEMON_LOG_DIR}"));
    assert_eq!(DAEMON_LOG_DIR, "/var/log/hostkeeper");
    assert_eq!(newsyslog_path(), NEWSYSLOG);
    assert_eq!(
        format!("/{NEWSYSLOG}"),
        "/private/etc/newsyslog.d/hostkeeper.conf"
    );
    // The good payload of the read-back holds them where the payload map has them.
    let entries = good_entries(BINARY);
    assert_eq!(entries[LOG_DIR_AT], dir_entry(&format!("./{LOG_DIR}")));
    assert!(entries[NEWSYSLOG_AT].ends_with(GOLDEN.as_bytes()));
    assert_eq!(
        entries[BIN_AT],
        odc_entry(&format!("./{BIN}"), 0o100_755, 0, 0, BINARY)
    );
}

#[test]
fn a_path_under_etc_or_var_is_held_under_private_and_no_other_is() {
    // Catches: a payload directory `etc` or `var` that the installer would put in place of
    // macOS's link into `/private`, or a path moved under `private/` by a prefix that is not its
    // top directory.
    let cases = [
        ("/var/log/hostkeeper", "private/var/log/hostkeeper"),
        ("/etc/newsyslog.d/x.conf", "private/etc/newsyslog.d/x.conf"),
        ("/tmp/x", "tmp/x"),
        ("/var", "private/var"),
        ("/usr/local/bin/hostkeeper", "usr/local/bin/hostkeeper"),
        (
            "/Library/LaunchDaemons/x.plist",
            "Library/LaunchDaemons/x.plist",
        ),
        ("/variable/x", "variable/x"),
        ("/etcetera", "etcetera"),
        ("/private/var/x", "private/var/x"),
        ("/", ""),
    ];
    for (installed, want) in cases {
        assert_eq!(payload_of(installed), want, "{installed}");
    }
}

#[test]
fn the_newsyslog_file_is_the_golden_and_rotates_the_plists_output_files_by_size_only() {
    // Catches: a newsyslog entry for another path than the plist's StandardOutPath or
    // StandardErrorPath (the two would drift apart), a rotation by time, archives kept without
    // bound or not compressed, or a signal sent to a process by a pid file the daemon has not.
    assert_eq!(newsyslog_text(), GOLDEN);
    let plist = plist_text().expect("the plist");
    let value = plist::Value::from_reader_xml(plist.as_bytes()).expect("the plist parses");
    let dict = value.as_dictionary().expect("a dictionary");
    let path_of = |key: &str| {
        dict.get(key)
            .and_then(plist::Value::as_string)
            .expect("an output path")
            .to_owned()
    };
    let lines: Vec<Vec<&str>> = GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(
        lines,
        [
            vec![
                path_of("StandardOutPath").as_str(),
                "root:wheel",
                "640",
                "5",
                "10240",
                "*",
                "NJ"
            ],
            vec![
                path_of("StandardErrorPath").as_str(),
                "root:wheel",
                "640",
                "5",
                "10240",
                "*",
                "NJ"
            ],
        ]
    );
    let job = DaemonJob::standard();
    for path in [&job.stdout, &job.stderr] {
        assert_eq!(path.parent(), Some(Path::new(DAEMON_LOG_DIR)));
    }
    let mut problems = Vec::new();
    newsyslog_problems(GOLDEN.as_bytes(), &mut problems);
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn a_newsyslog_file_naming_other_paths_is_listed_and_comments_and_blanks_are_not_entries() {
    // Catches: the read-back accepting a newsyslog file that rotates another file than the
    // daemon's output (or only one of them, or one twice), or reading a comment as an entry.
    let out = "/var/log/hostkeeper/daemon.out.log";
    let err = "/var/log/hostkeeper/daemon.err.log";
    let not = |named: &[&str]| {
        vec![format!(
            "/private/etc/newsyslog.d/hostkeeper.conf: rotates {named:?}, not the daemon's \
             StandardOutPath and StandardErrorPath [{out:?}, {err:?}]"
        )]
    };
    let cases: [(String, Vec<String>); 7] = [
        (
            GOLDEN.replace(err, "/var/log/other.log"),
            not(&[out, "/var/log/other.log"]),
        ),
        (GOLDEN.replace(err, out), not(&[out, out])),
        (
            GOLDEN.lines().take(4).map(|l| format!("{l}\n")).collect(),
            not(&[out]),
        ),
        (
            format!("{GOLDEN}/var/log/system.log\t644\t5\t1000\t*\tJ\n"),
            not(&[out, err, "/var/log/system.log"]),
        ),
        ("# nothing but a comment\n\n".to_owned(), not(&[])),
        (
            format!("\n  # indented comment\n   {out} root:wheel 640 5 10240 * NJ\n\n{err}\n"),
            vec![],
        ),
        (format!("{err}\n{out}\n"), not(&[err, out])),
    ];
    for (n, (text, want)) in cases.into_iter().enumerate() {
        let mut problems = Vec::new();
        newsyslog_problems(text.as_bytes(), &mut problems);
        assert_eq!(problems, want, "case {n}: {text:?}");
    }
}

#[test]
fn check_refuses_a_payload_without_the_log_directory_or_the_newsyslog_file_naming_it() {
    // Catches: a pkg that passes its read-back while it lacks the log directory or the
    // newsyslog file, or holds either with another kind, mode, owner or text.
    let good = good_entries(BINARY);
    let without = |n: usize| [&good[..n], &good[n + 1..]].concat();
    let with = |n: usize, entry: Vec<u8>| {
        let mut entries = good.clone();
        entries[n] = entry;
        entries
    };
    let other = GOLDEN.replace(
        "/var/log/hostkeeper/daemon.err.log",
        "/var/log/hostkeeper/other.log",
    );
    let at = format!("/{NEWSYSLOG}");
    let cases: Vec<(Vec<Vec<u8>>, Vec<String>)> = vec![
        (
            without(LOG_DIR_AT),
            vec!["missing /private/var/log/hostkeeper in the payload".to_owned()],
        ),
        (
            without(NEWSYSLOG_AT),
            vec!["missing /private/etc/newsyslog.d/hostkeeper.conf in the payload".to_owned()],
        ),
        (
            with(
                NEWSYSLOG_AT,
                odc_entry(&format!("./{NEWSYSLOG}"), 0o100_644, 0, 0, other.as_bytes()),
            ),
            vec![
                format!(
                    "{at}: rotates [\"/var/log/hostkeeper/daemon.out.log\", \
                     \"/var/log/hostkeeper/other.log\"], not the daemon's StandardOutPath and \
                     StandardErrorPath [\"/var/log/hostkeeper/daemon.out.log\", \
                     \"/var/log/hostkeeper/daemon.err.log\"]"
                ),
                format!("{at}: not the newsyslog entries of the daemon's output files"),
            ],
        ),
        (
            with(
                NEWSYSLOG_AT,
                odc_entry(
                    &format!("./{NEWSYSLOG}"),
                    0o100_644,
                    0,
                    0,
                    GOLDEN.replace("10240", "999999").as_bytes(),
                ),
            ),
            vec![format!(
                "{at}: not the newsyslog entries of the daemon's output files"
            )],
        ),
        (
            with(
                NEWSYSLOG_AT,
                odc_entry(
                    &format!("./{NEWSYSLOG}"),
                    0o100_666,
                    0,
                    0,
                    GOLDEN.as_bytes(),
                ),
            ),
            vec![format!("{at}: mode 666, not 644")],
        ),
        (
            with(
                NEWSYSLOG_AT,
                odc_entry(
                    &format!("./{NEWSYSLOG}"),
                    0o100_644,
                    501,
                    20,
                    GOLDEN.as_bytes(),
                ),
            ),
            vec![format!(
                "{at}: owned by uid 501 and gid 20, not root and wheel (0 and 0)"
            )],
        ),
        (
            with(
                LOG_DIR_AT,
                odc_entry(&format!("./{LOG_DIR}"), 0o100_755, 0, 0, b""),
            ),
            vec!["/private/var/log/hostkeeper: of type 100000, not a directory".to_owned()],
        ),
        (
            with(
                LOG_DIR_AT,
                odc_entry(&format!("./{LOG_DIR}"), 0o040_777, 0, 0, b""),
            ),
            vec!["/private/var/log/hostkeeper: mode 777, not 755".to_owned()],
        ),
        (
            with(
                LOG_DIR_AT,
                odc_entry(&format!("./{LOG_DIR}"), 0o040_755, 501, 0, b""),
            ),
            vec![
                "/private/var/log/hostkeeper: owned by uid 501 and gid 0, not root and wheel (0 \
                 and 0)"
                    .to_owned(),
            ],
        ),
        // At `/var` itself, not under `/private`, the installer would replace macOS's link.
        (
            [
                good.clone(),
                vec![
                    dir_entry("./var"),
                    dir_entry("./var/log"),
                    dir_entry("./var/log/hostkeeper"),
                ],
            ]
            .concat(),
            ["./var", "./var/log", "./var/log/hostkeeper"]
                .map(|name| {
                    format!(
                        "/{} ({name:?}) is in the payload, which holds only the binary, the \
                         plist, the newsyslog file, the log directory and their directories, \
                         each once",
                        &name[2..]
                    )
                })
                .to_vec(),
        ),
    ];
    for (n, (entries, want)) in cases.into_iter().enumerate() {
        let expanded = Expanded::new(&format!("macos-pkg-check-logs-{n}"), &entries);
        assert_eq!(expanded.problems(), want, "case {n}");
    }
    assert!(Expanded::good("macos-pkg-check-logs-good").check().is_ok());
}
