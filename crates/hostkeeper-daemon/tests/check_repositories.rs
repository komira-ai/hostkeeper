//! `hostkeeper check-repositories [--dir DIR]` (packages-repositories.md section 3.1): host
//! setup's check of a repositories directory with the daemon's own loader, as the daemon would
//! take it at `/etc/hostkeeper/repositories`, before the directory is put there. The files are
//! the test user's here; the command line judges them as root's, as the start does, which the
//! last tests show on a directory no user but root could pass.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::{self, Cli, Command, HostFiles, SystemdVars};
use hostkeeper_daemon::repositories::{
    self, CheckFlags, EXIT_REFUSED, REPOSITORIES_DIR, check, report, run_check,
};
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

/// A binary keyring's first bytes: an OpenPGP public-key packet header, then a zero and a high
/// byte.
const KEYRING: [u8; 6] = [0x99, 0x01, 0x0d, 0x04, 0x00, 0xff];

/// The keyring's file name, as host setup places it.
const KEYRING_FILE: &str = "komira.gpg";

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// Writes `bytes` at `dir/name`, mode 0644.
fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("the mode");
    path
}

/// The stanza of the channel `suite`, its keyring `<signed_dir>/komira.gpg`.
fn stanza(suite: &str, signed_dir: &Path) -> String {
    format!(
        "Types: deb\nURIs: https://packages.example.com/apt\nSuites: {suite}\nComponents: main\n\
         Signed-By: {}/{KEYRING_FILE}\n",
        signed_dir.display()
    )
}

/// A directory (mode 0755) named `name` under `parent`, holding what host setup stages: the
/// entries `komira-testing.sources` and `komira-stable.sources`, whose `Signed-By` names
/// `<signed_dir>/komira.gpg`, and that keyring.
fn staged(parent: &Path, name: &str, signed_dir: &Path) -> PathBuf {
    let dir = parent.join(name);
    fs::create_dir(&dir).expect("the directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    put(
        &dir,
        "komira-testing.sources",
        stanza("testing", signed_dir).as_bytes(),
    );
    put(
        &dir,
        "komira-stable.sources",
        stanza("stable", signed_dir).as_bytes(),
    );
    put(&dir, KEYRING_FILE, &KEYRING);
    dir
}

/// What `run_check` wrote and returned.
struct Run {
    status: u8,
    out: String,
    err: String,
}

fn run(dir: &Path, owner: u32) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let flags = CheckFlags {
        dir: dir.to_path_buf(),
    };
    let status = run_check(&flags, owner, &mut out, &mut err);
    Run {
        status,
        out: String::from_utf8(out).expect("UTF-8"),
        err: String::from_utf8(err).expect("UTF-8"),
    }
}

/// Proves a staged directory with the two channels' entries and their keyring passes: status 0,
/// one line per entry in name order with its name, its suite, the keyring path the daemon will
/// read and the SHA-256 of the keyring's bytes, and nothing on standard error.
///
/// Catches: a check that refuses what host setup places, or prints a digest of other bytes.
#[test]
fn a_staged_directory_of_the_two_channels_passes_and_lists_each_entry() {
    let scratch = scratch("check-pass");
    let dir = staged(scratch.path(), "staged", Path::new(REPOSITORIES_DIR));
    let ran = run(&dir, me());
    assert_eq!(ran.status, 0, "{}", ran.err);
    let digest = hex::encode(Sha256::digest(KEYRING));
    assert_eq!(
        ran.out,
        format!(
            "komira-stable suites=stable keyring={REPOSITORIES_DIR}/{KEYRING_FILE} sha256={digest}\n\
             komira-testing suites=testing keyring={REPOSITORIES_DIR}/{KEYRING_FILE} sha256={digest}\n"
        )
    );
    assert_eq!(ran.err, "");
}

/// Proves a directory that is missing or empty passes, as the start takes it (no repository),
/// and the line says so.
///
/// Catches: a check stricter than the start, which host setup would then believe over the
/// daemon.
#[test]
fn a_missing_or_empty_directory_passes_as_no_repository() {
    let scratch = scratch("check-none");
    let empty = scratch.path().join("empty");
    fs::create_dir(&empty).expect("the directory");
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o755)).expect("its mode");
    for dir in [empty, scratch.path().join("missing")] {
        let ran = run(&dir, me());
        assert_eq!(ran.status, 0, "{}", ran.err);
        assert_eq!(
            ran.out,
            format!(
                "{}: no repository entry; the daemon would announce no repository installer\n",
                dir.display()
            )
        );
        assert_eq!(ran.err, "");
    }
}

/// The defects host setup can stage, each made on a fresh staged directory `dir` whose
/// `Signed-By` names `signed_dir`.
type Defect = fn(dir: &Path, signed_dir: &Path);

/// The defects: the keyring the entries name is missing; an entry whose URI is not https://;
/// an entry that is not a regular file (a directory); a stray entry that is a link; an armored
/// keyring.
const DEFECTS: [(&str, Defect); 5] = [
    ("missing-keyring", |dir, _| {
        fs::remove_file(dir.join(KEYRING_FILE)).expect("the keyring goes");
    }),
    ("http-uri", |dir, signed_dir| {
        let text = stanza("testing", signed_dir).replace("https://", "http://");
        put(dir, "komira-testing.sources", text.as_bytes());
    }),
    ("directory-entry", |dir, _| {
        fs::create_dir(dir.join("komira-extra.sources")).expect("a directory entry");
    }),
    ("link-entry", |dir, _| {
        symlink(dir.join(KEYRING_FILE), dir.join("other.gpg")).expect("a link");
    }),
    ("armored-keyring", |dir, _| {
        put(
            dir,
            KEYRING_FILE,
            b"-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQENBF\n",
        );
    }),
];

/// The loader's text for each defect of [`DEFECTS`], in a staged directory `dir`.
fn refusal_text(defect: &str, dir: &Path) -> String {
    let testing = dir.join("komira-testing.sources");
    let stable = dir.join("komira-stable.sources");
    match defect {
        "missing-keyring" => format!(
            "{}: the keyring it names cannot be read: No such file or directory (os error 2)",
            stable.display()
        ),
        "http-uri" => format!(
            "{}: has the URI \"http://packages.example.com/apt\", neither https:// nor file:/",
            testing.display()
        ),
        "directory-entry" => format!(
            "{}: is not a regular file",
            dir.join("komira-extra.sources").display()
        ),
        "link-entry" => format!(
            "{}: neither a .sources file nor a keyring one names",
            dir.join("other.gpg").display()
        ),
        "armored-keyring" => format!(
            "{}: the keyring it names is not a binary OpenPGP keyring (it is empty or armored \
             text); apt reads armor only from a keyring named .asc",
            stable.display()
        ),
        other => panic!("no text for {other}"),
    }
}

/// Proves each defect is refused with the loader's message, in full, after the verb's name on
/// standard error, status 1 and nothing on standard output: a keyring the entries name that is
/// missing, a URI that is not https://, an entry that is not a regular file, a stray link, and
/// an armored keyring.
///
/// Catches: a check that passes a directory the daemon refuses to start on, or that says
/// something else than the loader.
#[test]
fn each_defect_is_refused_with_the_loaders_message() {
    let scratch = scratch("check-defects");
    for (name, defect) in DEFECTS {
        let dir = staged(scratch.path(), name, Path::new(REPOSITORIES_DIR));
        defect(&dir, Path::new(REPOSITORIES_DIR));
        let loader = check(&dir, me()).expect_err("the loader refuses it");
        assert_eq!(loader.to_string(), refusal_text(name, &dir), "{name}");
        let ran = run(&dir, me());
        assert_eq!(ran.status, EXIT_REFUSED, "{name}");
        assert_eq!(ran.out, "", "{name}");
        assert_eq!(
            ran.err,
            format!(
                "hostkeeper check-repositories: {}\n",
                refusal_text(name, &dir)
            ),
            "{name}"
        );
    }
}

/// Proves `--dir` resolves a `Signed-By` of `/etc/hostkeeper/repositories/<file>` to `<file>` of
/// the directory checked, which is not `/etc/hostkeeper/repositories`: the keyring read is the
/// staged one (its digest printed, and a staged directory without it refused naming the staged
/// entry), and a `Signed-By` naming the staging directory itself is refused, since the daemon
/// will not find the keyring there.
///
/// Catches: a check that reads `/etc/hostkeeper/repositories` instead of the staged files, or
/// that passes entries that only work where they were staged.
#[test]
fn dir_resolves_signed_by_to_the_directory_checked() {
    let scratch = scratch("check-dir");
    let dir = staged(scratch.path(), "staged", Path::new(REPOSITORIES_DIR));
    assert_ne!(dir, Path::new(REPOSITORIES_DIR));
    let loaded = check(&dir, me()).expect("the staged directory loads");
    let placed = Path::new(REPOSITORIES_DIR).join(KEYRING_FILE);
    assert_eq!(
        loaded.keyrings.get(&placed).map(Vec::as_slice),
        Some(&KEYRING[..])
    );
    let other = [0x98, 0x33, 0x04];
    put(&dir, KEYRING_FILE, &other);
    let ran = run(&dir, me());
    assert_eq!(ran.status, 0, "{}", ran.err);
    assert!(
        ran.out
            .contains(&format!("sha256={}\n", hex::encode(Sha256::digest(other)))),
        "{}",
        ran.out
    );

    let at_staging = staged(
        scratch.path(),
        "at-staging",
        &scratch.path().join("at-staging"),
    );
    let ran = run(&at_staging, me());
    assert_eq!(ran.status, EXIT_REFUSED);
    assert_eq!(
        ran.err,
        format!(
            "hostkeeper check-repositories: {}: has the Signed-By \"{}/{KEYRING_FILE}\", not an \
             absolute path of a keyring in its directory\n",
            at_staging.join("komira-stable.sources").display(),
            at_staging.display()
        )
    );
}

/// The host files of a Linux host under `root`, with `repositories` as its entries' directory.
fn host_files(root: &Path, repositories: &Path) -> HostFiles {
    HostFiles {
        repositories: Some(repositories.to_path_buf()),
        macos_pkg_signers: None,
        os_root: root.join("os"),
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        stores: root.join("no-stores.toml"),
        pki: root.join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    }
}

/// `text` with `dirs` written as `<DIR>`.
fn normalised(text: &str, dirs: &[&Path]) -> String {
    dirs.iter().fold(text.to_owned(), |text, dir| {
        text.replace(&dir.display().to_string(), "<DIR>")
    })
}

/// Proves the verb and the daemon's start agree on the same files: for the sound directory and
/// each defect, the start's load of a directory placed where its `Signed-By` names it and the
/// verb's check of the same files staged elsewhere naming `/etc/hostkeeper/repositories` both
/// accept, with the same entries, or both refuse, with the same reason once the directories are
/// written alike.
///
/// Catches: a verb whose rules drift from the start's, so that host setup passes a directory on
/// which the daemon then refuses to start, or refuses one it would start on.
#[test]
fn the_verb_and_the_start_agree_on_the_same_files() {
    let scratch = scratch("check-agree");
    let none: Defect = |_, _| {};
    let cases = std::iter::once(("sound", none)).chain(DEFECTS);
    for (name, defect) in cases {
        let host = scratch.path().join(format!("host-{name}"));
        fs::create_dir(&host).expect("the host root");
        let placed = host.join("repositories");
        let daemon_dir = staged(&host, "repositories", &placed);
        defect(&daemon_dir, &placed);
        let verb_dir = staged(
            scratch.path(),
            &format!("staged-{name}"),
            Path::new(REPOSITORIES_DIR),
        );
        defect(&verb_dir, Path::new(REPOSITORIES_DIR));

        let started = repositories::load(&host_files(&host, &placed), me());
        let checked = check(&verb_dir, me());
        match (started, checked) {
            (Ok(started), Ok(checked)) => {
                assert_eq!(name, "sound");
                assert_eq!(started.names(), checked.repositories.names());
                for entry in started.names() {
                    let (a, b) = (started.get(entry), checked.repositories.get(entry));
                    let (a, b) = (a.expect("in the start"), b.expect("in the check"));
                    assert_eq!(
                        (&a.uri, &a.suite, &a.components),
                        (&b.uri, &b.suite, &b.components)
                    );
                }
            }
            (Err(started), Err(checked)) => {
                let started = match started {
                    hostkeeper_daemon::start::StartError::Repositories { path, source } => {
                        assert_eq!(path, placed, "{name}");
                        source.to_string()
                    }
                    other => panic!("{name}: not a repositories refusal: {other}"),
                };
                assert_eq!(
                    normalised(&started, &[&placed]),
                    normalised(
                        &checked.to_string(),
                        &[&verb_dir, Path::new(REPOSITORIES_DIR)]
                    ),
                    "{name}"
                );
            }
            (started, checked) => {
                panic!("{name}: the start gave {started:?} and the check {checked:?}")
            }
        }
    }
}

/// A writer that refuses every write.
struct Closed;

impl io::Write for Closed {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Proves a report that cannot be written is a failure, status 1, said on standard error; and
/// that standard error refusing writes too changes nothing of the status.
///
/// Catches: a check that exits 0 with nothing printed, which host setup would read as a pass of
/// a directory with no entry.
#[test]
fn a_report_that_cannot_be_written_fails() {
    let scratch = scratch("check-closed");
    let dir = staged(scratch.path(), "staged", Path::new(REPOSITORIES_DIR));
    let flags = CheckFlags { dir: dir.clone() };
    let mut err = Vec::new();
    assert_eq!(run_check(&flags, me(), &mut Closed, &mut err), EXIT_REFUSED);
    assert_eq!(
        String::from_utf8(err).expect("UTF-8"),
        "hostkeeper check-repositories: the report cannot be written: closed\n"
    );
    assert_eq!(
        run_check(&flags, me(), &mut Closed, &mut Closed),
        EXIT_REFUSED
    );
    let loaded = check(&dir, me()).expect("loads");
    assert_eq!(report(&dir, &loaded).lines().count(), 2);
}

/// Proves the command line: `check-repositories --help` is clap's help, status 0;
/// `check-repositories` alone checks `/etc/hostkeeper/repositories`; `--dir DIR` names DIR; a
/// `--dir` without a value, an unknown flag and an argument are usage errors, status 2.
///
/// Catches: a node whose binary has no check (the help fails), or a default other than the
/// directory the daemon reads.
#[test]
fn the_command_line_of_check_repositories() {
    let parse = |words: &[&str]| {
        Cli::try_parse_from(["hostkeeper", "check-repositories"].iter().chain(words))
    };
    let help = parse(&["--help"]).expect_err("help is clap's");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    assert_eq!(help.exit_code(), 0);
    let flags = |cli: Cli| match cli.command {
        Command::CheckRepositories(flags) => flags.dir,
        other => panic!("not the check: {other:?}"),
    };
    assert_eq!(
        flags(parse(&[]).expect("parses")),
        Path::new(REPOSITORIES_DIR)
    );
    assert_eq!(
        flags(parse(&["--dir", "/staged"]).expect("parses")),
        Path::new("/staged")
    );
    for words in [&["--dir"][..], &["--bogus"], &["extra"]] {
        let refused = parse(words).expect_err("a usage error");
        assert_eq!(refused.exit_code(), 2, "{words:?}");
    }
}

/// Proves the command line judges the files as root's, as the start does, and runs in the
/// process started even from a packaged path: a directory that passes for its owner, the test
/// user, is refused naming root (uid 0) when the test runs as another user; a missing one
/// passes; neither seeds nor execs `current`.
///
/// Catches: a check that trusts files whoever owns them, or that execs another binary than the
/// one host setup chose.
#[test]
fn the_command_line_judges_root_ownership_in_process() {
    let scratch = scratch("check-cli");
    let dir = staged(scratch.path(), "staged", Path::new(REPOSITORIES_DIR));
    let image = scratch.path().join("hostkeeper");
    let execs = RefCell::new(Vec::new());
    let exec = |path: &Path, _: &[OsString]| {
        execs.borrow_mut().push(path.to_path_buf());
        io::Error::other("no exec in a test")
    };
    let code = |dir: &Path| {
        let args = ["hostkeeper", "check-repositories", "--dir"]
            .map(OsString::from)
            .into_iter()
            .chain([dir.as_os_str().to_owned()])
            .collect();
        let status = cli::run_from(args, &image, &[&image], SystemdVars::default(), &exec);
        format!("{status:?}")
    };
    let expected = if me() == 0 { 0 } else { EXIT_REFUSED };
    assert_eq!(code(&dir), format!("{:?}", ExitCode::from(expected)));
    if me() != 0 {
        assert_eq!(
            check(&dir, 0).expect_err("not root's").to_string(),
            format!("{}: is owned by uid {}, not 0", dir.display(), me())
        );
    }
    assert_eq!(
        code(&scratch.path().join("missing")),
        format!("{:?}", ExitCode::from(0))
    );
    assert!(execs.borrow().is_empty());
}
