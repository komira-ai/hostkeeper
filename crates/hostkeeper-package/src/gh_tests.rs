//! Tests of the `gh` verifier: the arguments and environment it gives `gh`, how it reads `gh`'s
//! verdict, the ref globs, the file trust check and the process runner.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

use super::*;

use hostkeeper_config::daemon::Sha256Hex;

use crate::ports::{CommitId, SourceRef};
use crate::testing::{FakeClock, commit_id, digest, listing, scratch, signers, source_ref};

/// The staging files left in `dir`. A child built with coverage instrumentation may also leave
/// its profile there, which is not a staging file.
fn staging_files(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|name| name.starts_with(".fetch-"))
        .collect()
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const IDENTITY: &str = r"^https://github\.com/example-org/[^/]+/\.github/workflows/release\.yml@";

/// What a test asks the verifier about.
struct Case {
    signers: TrustedSigners,
    archive: PathBuf,
    bundle: PathBuf,
    sha: Sha256Hex,
    commit: CommitId,
    source_ref: SourceRef,
    deadline: Stamp,
}

impl Case {
    fn new(signers: TrustedSigners, source_ref_text: &str) -> Case {
        Case {
            signers,
            archive: PathBuf::from("pkg/archive.tar.zst"),
            bundle: PathBuf::from("pkg/archive.bundle.json"),
            sha: digest(&"ab".repeat(32)),
            commit: commit_id(COMMIT),
            source_ref: source_ref(source_ref_text),
            deadline: FakeClock::at(10).stamp(500),
        }
    }

    fn request(&self) -> Attest<'_> {
        Attest {
            archive: &self.archive,
            bundle: &self.bundle,
            sha256: &self.sha,
            commit: &self.commit,
            source_ref: &self.source_ref,
            signers: &self.signers,
            deadline: &self.deadline,
        }
    }
}

fn sample() -> Case {
    Case::new(
        signers(
            "example-org",
            ".github/workflows/release.yml",
            &["refs/tags/*"],
        ),
        "refs/tags/v1.2.3",
    )
}

/// A [`Runner`] that records each spawn and answers as the closure says.
struct FakeRunner {
    spawns: Rc<RefCell<Vec<Spawn>>>,
    answer: Box<dyn Fn(&Spawn) -> io::Result<Ran>>,
}

impl Runner for FakeRunner {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        self.spawns.borrow_mut().push(spawn.clone());
        (self.answer)(spawn)
    }
}

/// A [`FileTrust`] that trusts every path except those it was told to refuse.
struct FakeTrust {
    refuse: Vec<PathBuf>,
}

impl FileTrust for FakeTrust {
    fn check(&self, path: &Path) -> Result<(), TrustError> {
        if self.refuse.iter().any(|p| p == path) {
            Err(TrustError::NotRegular)
        } else {
            Ok(())
        }
    }
}

/// A verifier over a fake runner, and the spawns the runner saw.
struct Rig {
    verifier: GhVerifier,
    spawns: Rc<RefCell<Vec<Spawn>>>,
}

fn rig_refusing(answer: impl Fn(&Spawn) -> io::Result<Ran> + 'static, refuse: &[&str]) -> Rig {
    let spawns = Rc::new(RefCell::new(Vec::new()));
    let runner = FakeRunner {
        spawns: Rc::clone(&spawns),
        answer: Box::new(answer),
    };
    let trust = FakeTrust {
        refuse: refuse.iter().map(PathBuf::from).collect(),
    };
    let verifier = GhVerifier::new(
        PathBuf::from("tools/gh"),
        PathBuf::from("tools/trusted_root.json"),
        PathBuf::from("state/home"),
        Box::new(runner),
        Box::new(trust),
    );
    Rig { verifier, spawns }
}

fn rig(answer: impl Fn(&Spawn) -> io::Result<Ran> + 'static) -> Rig {
    rig_refusing(answer, &[])
}

fn exit_zero(_: &Spawn) -> io::Result<Ran> {
    Ok(Ran {
        code: Some(0),
        ..Ran::default()
    })
}

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

/// The arguments `gh` should get for `case` under the sample rig, with the hosted-runner flag
/// as the case's signer says.
fn expected_args(case: &Case) -> Vec<OsString> {
    let mut args = os(&["attestation", "verify", "pkg/archive.tar.zst"]);
    args.extend(os(&["--bundle", "pkg/archive.bundle.json"]));
    args.extend(os(&["--custom-trusted-root", "tools/trusted_root.json"]));
    args.extend(os(&[
        "--owner",
        "example-org",
        "--cert-identity-regex",
        IDENTITY,
    ]));
    args.extend(os(&["--cert-oidc-issuer", ISSUER]));
    args.extend(os(&["--source-ref", case.source_ref.as_str()]));
    args.extend(os(&["--source-digest", COMMIT]));
    args.extend(os(&["--predicate-type", PREDICATE]));
    if case.signers.hosted_runner {
        args.extend(os(&["--deny-self-hosted-runners"]));
    }
    args
}

/// `gh` gets every argument of the configured signer in a fixed order, runs in the home with the
/// request's deadline, and exit 0 is the attested commit and ref.
/// Catches an argument dropped, reordered or taken from anywhere but the config and request.
#[test]
fn gh_is_asked_for_exactly_the_configured_signer() {
    let case = sample();
    let rig = rig(exit_zero);
    let attested = rig.verifier.verify(&case.request());
    assert_eq!(
        attested,
        Ok(Attested {
            commit: case.commit.clone(),
            source_ref: case.source_ref.clone(),
        })
    );
    let spawns = rig.spawns.borrow();
    assert_eq!(spawns.len(), 1);
    let spawn = &spawns[0];
    assert_eq!(spawn.program, PathBuf::from("tools/gh"));
    assert_eq!(spawn.cwd, PathBuf::from("state/home"));
    assert_eq!(spawn.deadline, case.deadline);
    let want = os(&[
        "attestation",
        "verify",
        "pkg/archive.tar.zst",
        "--bundle",
        "pkg/archive.bundle.json",
        "--custom-trusted-root",
        "tools/trusted_root.json",
        "--owner",
        "example-org",
        "--cert-identity-regex",
        r"^https://github\.com/example-org/[^/]+/\.github/workflows/release\.yml@",
        "--cert-oidc-issuer",
        "https://token.actions.githubusercontent.com",
        "--source-ref",
        "refs/tags/v1.2.3",
        "--source-digest",
        COMMIT,
        "--predicate-type",
        "https://slsa.dev/provenance/v1",
        "--deny-self-hosted-runners",
    ]);
    assert_eq!(spawn.args.len(), want.len());
    for (index, (got, want)) in spawn.args.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "argument {index}");
    }
}

/// `--deny-self-hosted-runners` is passed only when the config asks for hosted runners.
/// Catches a flag that is always passed, or never.
#[test]
fn hosted_runner_false_omits_the_self_hosted_denial() {
    let mut open = sample();
    open.signers.hosted_runner = false;
    let rig_open = rig(exit_zero);
    rig_open.verifier.verify(&open.request()).expect("attested");
    let mut want = expected_args(&open);
    assert_eq!(rig_open.spawns.borrow()[0].args, want);
    assert_eq!(want.last(), Some(&OsString::from(PREDICATE)));

    let hosted = sample();
    let rig_hosted = rig(exit_zero);
    rig_hosted
        .verifier
        .verify(&hosted.request())
        .expect("attested");
    let with_flag = expected_args(&hosted);
    want.push(OsString::from("--deny-self-hosted-runners"));
    assert_eq!(with_flag, want);
    assert_eq!(rig_hosted.spawns.borrow()[0].args, want);
}

/// `gh` gets a fixed environment and a proxy that refuses connections, and nothing inherited.
/// Catches a token, a real proxy or any host variable reaching `gh`.
#[test]
fn gh_gets_a_fixed_environment_and_a_dead_proxy() {
    let case = sample();
    let rig = rig(exit_zero);
    rig.verifier.verify(&case.request()).expect("attested");
    let mut env = rig.spawns.borrow()[0].env.clone();
    env.sort();
    let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
    let want = vec![
        pair("ALL_PROXY", "http://127.0.0.1:9"),
        pair("GH_CONFIG_DIR", "state/home/config"),
        pair("GH_NO_UPDATE_NOTIFIER", "1"),
        pair("GH_PROMPT_DISABLED", "1"),
        pair("HOME", "state/home"),
        pair("HTTPS_PROXY", "http://127.0.0.1:9"),
        pair("HTTP_PROXY", "http://127.0.0.1:9"),
        pair("NO_COLOR", "1"),
        pair("PATH", "/usr/bin:/bin"),
    ];
    assert_eq!(env, want);
}

/// A ref a glob matches runs `gh` once with that ref; a ref no glob matches is rejected with the
/// exact message and `gh` is never run; a second glob can match.
/// Catches a ref outside the config reaching `gh`, or only the first glob being consulted.
#[test]
fn each_configured_ref_pattern_is_matched_and_a_ref_outside_them_is_rejected_without_running_gh() {
    let tag = sample();
    let rig_tag = rig(exit_zero);
    rig_tag.verifier.verify(&tag.request()).expect("attested");
    assert_eq!(rig_tag.spawns.borrow().len(), 1);
    assert_eq!(rig_tag.spawns.borrow()[0].args, expected_args(&tag));

    for outside in ["refs/heads/main", "refs/tags"] {
        let case = Case::new(
            signers(
                "example-org",
                ".github/workflows/release.yml",
                &["refs/tags/*"],
            ),
            outside,
        );
        let rig_out = rig(exit_zero);
        assert_eq!(
            rig_out.verifier.verify(&case.request()),
            Err(VerifyError::Rejected(format!(
                "the ref {outside} matches none of the trusted signer's refs"
            )))
        );
        assert_eq!(rig_out.spawns.borrow().len(), 0);
    }

    for allowed in ["refs/heads/main", "refs/tags/v1"] {
        let case = Case::new(
            signers(
                "example-org",
                ".github/workflows/release.yml",
                &["refs/heads/main", "refs/tags/*"],
            ),
            allowed,
        );
        let rig_two = rig(exit_zero);
        rig_two.verifier.verify(&case.request()).expect("attested");
        assert_eq!(rig_two.spawns.borrow().len(), 1);
    }
}

/// The glob table: `*` stays within one component, matches any run including none, backtracks,
/// and everything else is literal.
/// Catches a `*` that crosses a `/`, a missing backtrack and a leftover pattern accepted.
#[test]
fn ref_matches_follows_the_glob_rules() {
    let table = [
        ("refs/tags/*", "refs/tags/v1.2.3", true),
        ("refs/tags/*", "refs/tags", false),
        ("refs/tags/*", "refs/tags/a/b", false),
        ("refs/tags/*", "refs/heads/main", false),
        ("refs/tags/v*.3", "refs/tags/v1.2.3", true),
        ("refs/tags/v*.3", "refs/tags/v1.2.4", false),
        ("refs/heads/main", "refs/heads/main", true),
        ("refs/heads/main", "refs/heads/mainx", false),
        ("a", "b", false),
        ("a", "", false),
        ("ab", "a", false),
        ("a", "ab", false),
        ("", "", true),
        ("*", "", true),
        ("*a", "b", false),
        ("a*", "a", true),
        ("**", "x", true),
        ("a*b*c", "axxbyyc", true),
        ("a*b*c", "axxbyy", false),
        ("*.yml", "x.yml.yml", true),
        ("refs/*/x", "refs/y/x", true),
        ("refs/*", "refs/", true),
        ("refs//x", "refs//x", true),
    ];
    for (pattern, text, want) in table {
        assert_eq!(ref_matches(pattern, text), want, "{pattern} against {text}");
    }
}

/// Every regular-expression metacharacter is preceded by one backslash and nothing else changes,
/// in the quoting and in the identity built from the config.
/// Catches a workflow or organisation interpolated into the pattern unescaped.
#[test]
fn the_workflow_value_is_escaped_not_interpolated() {
    let mut escaped = String::new();
    for c in REGEX_META.chars() {
        escaped.push('\\');
        escaped.push(c);
    }
    assert_eq!(REGEX_META.chars().count(), 14);
    assert_eq!(quote_meta(REGEX_META), escaped);
    assert_eq!(
        quote_meta(&format!("a{REGEX_META}b")),
        format!("a{escaped}b")
    );
    assert_eq!(quote_meta("release-1_a/b"), "release-1_a/b");
    assert_eq!(quote_meta(""), "");

    let both = signers("a.b", "w[1]*.yml", &["refs/tags/*"]);
    assert_eq!(
        signer_identity(&both),
        r"^https://github\.com/a\.b/[^/]+/w\[1\]\*\.yml@"
    );
    let everything = signers(REGEX_META, REGEX_META, &["refs/tags/*"]);
    assert_eq!(
        signer_identity(&everything),
        format!("^https://github\\.com/{escaped}/[^/]+/{escaped}@")
    );
}

/// The four ways a run can end without success are told apart.
/// Catches a start failure reported as a verdict, a timeout reported as a rejection, a signal
/// taken for a verdict, and a complaint passed on raw or uncut.
#[test]
fn a_gh_that_cannot_start_is_unavailable_a_gh_killed_at_the_deadline_is_timed_out_and_a_nonzero_exit_is_rejected()
 {
    let case = sample();
    let missing = rig(|_| Err(io::Error::from(io::ErrorKind::NotFound)));
    assert_eq!(
        missing.verifier.verify(&case.request()),
        Err(VerifyError::Unavailable)
    );

    let slow = rig(|_| {
        Ok(Ran {
            code: None,
            timed_out: true,
            stderr: b"killed".to_vec(),
        })
    });
    assert_eq!(
        slow.verifier.verify(&case.request()),
        Err(VerifyError::TimedOut)
    );

    let slow_exit = rig(|_| {
        Ok(Ran {
            code: Some(0),
            timed_out: true,
            stderr: Vec::new(),
        })
    });
    assert_eq!(
        slow_exit.verifier.verify(&case.request()),
        Err(VerifyError::TimedOut)
    );

    let long = format!("gh: \u{7}bell {}\nsecond line", "y".repeat(200));
    let failed = rig(move |_| {
        Ok(Ran {
            code: Some(1),
            timed_out: false,
            stderr: long.clone().into_bytes(),
        })
    });
    let want = format!("gh: ?bell {}", "y".repeat(150));
    assert_eq!(want.chars().count(), 160);
    assert_eq!(
        failed.verifier.verify(&case.request()),
        Err(VerifyError::Rejected(want))
    );

    let silent = rig(|_| {
        Ok(Ran {
            code: Some(2),
            ..Ran::default()
        })
    });
    assert_eq!(
        silent.verifier.verify(&case.request()),
        Err(VerifyError::Rejected(String::new()))
    );

    let signalled = rig(|_| Ok(Ran::default()));
    assert_eq!(
        signalled.verifier.verify(&case.request()),
        Err(VerifyError::Unavailable)
    );
}

/// An untrusted `gh`, or an untrusted trusted root, gives no verdict and runs nothing.
/// Catches a `gh` or root run without the trust check, or a refusal reported as a rejection.
#[test]
fn an_untrusted_gh_or_trusted_root_is_unavailable_and_nothing_runs() {
    let case = sample();
    for refused in ["tools/gh", "tools/trusted_root.json"] {
        let rig = rig_refusing(exit_zero, &[refused]);
        assert_eq!(
            rig.verifier.verify(&case.request()),
            Err(VerifyError::Unavailable),
            "{refused}"
        );
        assert_eq!(rig.spawns.borrow().len(), 0, "{refused}");
    }
}

/// The complaint is the first line, printable ASCII only, at most 160 characters.
/// Catches later lines leaking in, control or non-UTF-8 bytes passing through, and no cut.
#[test]
fn complaint_keeps_one_short_printable_line() {
    assert_eq!(complaint(b""), "");
    assert_eq!(complaint(&[b'z'; 200]), "z".repeat(160));
    assert_eq!(complaint(b"first line\nsecond line\n"), "first line");
    assert_eq!(complaint(b"windows line\r\nnext"), "windows line");
    assert_eq!(complaint(b"\xff\xfeabc"), "??abc");
    assert_eq!(complaint(b"tab\there \x1b[0m"), "tab?here ?[0m");
}

/// A regular file is trusted when its owner is the wanted uid and only the owner can write it.
fn trusted_file(dir: &Path, name: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"content").expect("write the file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("set the mode");
    path
}

fn uid_of(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").uid()
}

/// A regular file of the wanted owner that others cannot write passes, executable or not.
/// Catches a good file refused.
#[test]
fn owned_by_accepts_a_regular_file_nobody_else_can_write() {
    let dir = scratch("gh-owned-ok");
    for mode in [0o644, 0o755, 0o600] {
        let path = trusted_file(dir.path(), &format!("file-{mode:o}"), mode);
        let trust = OwnedBy { uid: uid_of(&path) };
        trust.check(&path).expect("a trusted file");
    }
}

/// A file the group or others can write is refused with its mode.
/// Catches the group-write or other-write bit being ignored.
#[test]
fn owned_by_refuses_a_file_the_group_or_others_can_write() {
    let dir = scratch("gh-owned-writable");
    for mode in [0o664, 0o646] {
        let path = trusted_file(dir.path(), &format!("file-{mode:o}"), mode);
        let trust = OwnedBy { uid: uid_of(&path) };
        let err = trust.check(&path).expect_err("a writable file");
        assert_eq!(
            err.to_string(),
            format!("mode {mode:o} lets the group or others write it")
        );
        assert!(matches!(err, TrustError::Writable { mode: m } if m == mode));
    }
}

/// A file of another owner is refused with both uids.
/// Catches ownership not being compared.
#[test]
fn owned_by_refuses_a_file_of_another_owner() {
    let dir = scratch("gh-owned-owner");
    let path = trusted_file(dir.path(), "file", 0o644);
    let uid = uid_of(&path);
    let err = OwnedBy { uid: uid + 1 }
        .check(&path)
        .expect_err("another owner");
    assert_eq!(
        err.to_string(),
        format!("owned by uid {uid}, not the trusted uid {}", uid + 1)
    );
    assert!(matches!(err, TrustError::Owner { .. }));
}

/// A directory, a link to a good file and a missing path are refused.
/// Catches a directory taken for a file, a link followed, and a missing file trusted.
#[test]
fn owned_by_refuses_a_directory_a_link_and_a_missing_path() {
    let dir = scratch("gh-owned-kinds");
    let good = trusted_file(dir.path(), "good", 0o644);
    let trust = OwnedBy { uid: uid_of(&good) };

    let err = trust.check(dir.path()).expect_err("a directory");
    assert_eq!(err.to_string(), "not a regular file");
    assert!(matches!(err, TrustError::NotRegular));

    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&good, &link).expect("make the link");
    let err = trust.check(&link).expect_err("a link");
    assert!(matches!(err, TrustError::Io(_)));

    let err = trust
        .check(&dir.path().join("missing"))
        .expect_err("a missing path");
    assert!(matches!(err, TrustError::Io(ref e) if e.kind() == io::ErrorKind::NotFound));
}

/// A `Spawn` of the test binary itself in `cwd`.
fn self_spawn(cwd: &Path, args: &[&str], deadline: Stamp) -> Spawn {
    Spawn {
        program: std::env::current_exe().expect("the test binary"),
        args: os(args),
        env: Vec::new(),
        cwd: cwd.to_path_buf(),
        deadline,
    }
}

/// The child the runner tests start and kill: it waits until it is killed.
#[test]
#[ignore = "the child the runner tests start and kill: it waits until it is killed"]
fn a_child_that_waits_until_it_is_killed() {
    loop {
        std::thread::park();
    }
}

/// A child that exits by itself is reported with its exit code, and the staging file is gone.
/// Catches a run that waits for the deadline, loses the code, or leaves its staging file.
#[test]
fn a_child_that_exits_is_reported_with_its_code_and_leaves_no_staging_file() {
    let dir = scratch("gh-run-exit");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    let spawn = self_spawn(dir.path(), &["--list"], clock.stamp(u64::MAX / 2));
    let ran = runner.run(&spawn).expect("the child ran");
    assert_eq!(ran.code, Some(0));
    assert!(!ran.timed_out);
    assert_eq!(staging_files(dir.path()), Vec::<String>::new());
}

/// A child that fails keeps its standard error and a non-zero code.
/// Catches standard error not being captured or a failure reported as success.
#[test]
fn a_child_that_fails_has_its_standard_error_kept() {
    let dir = scratch("gh-run-fail");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    let spawn = self_spawn(
        dir.path(),
        &["--no-such-flag-for-this-test"],
        clock.stamp(u64::MAX / 2),
    );
    let ran = runner.run(&spawn).expect("the child ran");
    assert!(!ran.timed_out);
    let code = ran.code.expect("an exit code");
    assert_ne!(code, 0);
    assert!(!ran.stderr.is_empty());
    assert_eq!(staging_files(dir.path()), Vec::<String>::new());
}

/// A child still running at the deadline is killed and reported as timed out, after the runner
/// slept on the injected clock.
/// Catches a child left running, a timeout reported with a code, and a loop that never sleeps.
#[test]
fn a_child_still_running_at_the_deadline_is_killed() {
    let dir = scratch("gh-run-kill");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    let spawn = self_spawn(
        dir.path(),
        &[
            "--exact",
            "gh::tests::a_child_that_waits_until_it_is_killed",
            "--ignored",
        ],
        clock.stamp(103),
    );
    let ran = runner.run(&spawn).expect("the child ran");
    assert_eq!(ran.code, None);
    assert!(ran.timed_out);
    assert!(clock.sleeps() >= 1);
    assert_eq!(staging_files(dir.path()), Vec::<String>::new());
}

/// A program that does not exist is an error of kind `NotFound`, and no staging file is left.
/// Catches a start failure reported as a finished run.
#[test]
fn a_program_that_does_not_exist_cannot_be_started() {
    let dir = scratch("gh-run-missing");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    let mut spawn = self_spawn(dir.path(), &[], clock.stamp(200));
    spawn.program = dir.path().join("no-such-program");
    let err = runner.run(&spawn).expect_err("no such program");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert_eq!(staging_files(dir.path()), Vec::<String>::new());
}

/// A working directory that does not exist stops the run before anything starts.
/// Catches a staging failure ignored.
#[test]
fn a_missing_working_directory_is_an_error() {
    let dir = scratch("gh-run-nocwd");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    let spawn = self_spawn(&dir.path().join("absent"), &["--list"], clock.stamp(200));
    let err = runner.run(&spawn).expect_err("no working directory");
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}
