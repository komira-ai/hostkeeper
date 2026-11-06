//! Tests of the `gh` verifier: the arguments and environment it gives `gh`, how it reads `gh`'s
//! verdict, the ref globs, the file trust check and the process runner.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;

use super::*;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_steps::{BootId, BootIdError};

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
const IDENTITY: &str =
    r"^https://github\.com/example-org/[^/]+/\.github/workflows/artifacts\.yml@refs/[^@]+$";

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
            archive: PathBuf::from("/srv/pkg/archive.tar.zst"),
            bundle: PathBuf::from("/srv/pkg/archive.bundle.json"),
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
            ".github/workflows/artifacts.yml",
            &["refs/heads/main"],
        ),
        "refs/heads/main",
    )
}

/// How the fake runner answers a spawn.
type Answer = Box<dyn Fn(&Spawn) -> io::Result<Ran>>;

/// A [`Runner`] that records each spawn and answers as the closure says.
struct FakeRunner {
    spawns: Rc<RefCell<Vec<Spawn>>>,
    answer: Answer,
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

/// The three paths a verifier is built with: `gh`, the trusted root and the scratch home.
type Paths = [&'static str; 3];

const PATHS: Paths = [
    "/opt/tools/gh",
    "/opt/tools/trusted_root.json",
    "/opt/state/gh-scratch",
];

fn rig_refusing(answer: impl Fn(&Spawn) -> io::Result<Ran> + 'static, refuse: &[&str]) -> Rig {
    rig_at(PATHS, answer, refuse)
}

fn rig_at(
    paths: Paths,
    answer: impl Fn(&Spawn) -> io::Result<Ran> + 'static,
    refuse: &[&str],
) -> Rig {
    let spawns = Rc::new(RefCell::new(Vec::new()));
    let runner = FakeRunner {
        spawns: Rc::clone(&spawns),
        answer: Box::new(answer),
    };
    let trust = FakeTrust {
        refuse: refuse.iter().map(PathBuf::from).collect(),
    };
    let verifier = GhVerifier::new(
        PathBuf::from(paths[0]),
        PathBuf::from(paths[1]),
        PathBuf::from(paths[2]),
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
    let mut args = os(&["attestation", "verify", "/srv/pkg/archive.tar.zst"]);
    args.extend(os(&["--bundle", "/srv/pkg/archive.bundle.json"]));
    args.extend(os(&[
        "--custom-trusted-root",
        "/opt/tools/trusted_root.json",
    ]));
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
    assert_eq!(spawn.program, PathBuf::from("/opt/tools/gh"));
    assert_eq!(spawn.cwd, PathBuf::from("/opt/state/gh-scratch"));
    assert_eq!(spawn.deadline, case.deadline);
    let want = os(&[
        "attestation",
        "verify",
        "/srv/pkg/archive.tar.zst",
        "--bundle",
        "/srv/pkg/archive.bundle.json",
        "--custom-trusted-root",
        "/opt/tools/trusted_root.json",
        "--owner",
        "example-org",
        "--cert-identity-regex",
        IDENTITY,
        "--cert-oidc-issuer",
        "https://token.actions.githubusercontent.com",
        "--source-ref",
        "refs/heads/main",
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
        pair("GH_CONFIG_DIR", "/opt/state/gh-scratch/config"),
        pair("GH_NO_UPDATE_NOTIFIER", "1"),
        pair("GH_PROMPT_DISABLED", "1"),
        pair("HOME", "/opt/state/gh-scratch"),
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
    let main = sample();
    let rig_main = rig(exit_zero);
    rig_main.verifier.verify(&main.request()).expect("attested");
    assert_eq!(rig_main.spawns.borrow().len(), 1);
    assert_eq!(rig_main.spawns.borrow()[0].args, expected_args(&main));

    for outside in ["refs/tags/v1.2.3", "refs/heads/topic", "refs/heads"] {
        let case = Case::new(
            signers(
                "example-org",
                ".github/workflows/artifacts.yml",
                &["refs/heads/main"],
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

    for allowed in ["refs/heads/main", "refs/heads/release/1"] {
        let case = Case::new(
            signers(
                "example-org",
                ".github/workflows/artifacts.yml",
                &["refs/heads/main", "refs/heads/release/*"],
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

    let both = signers("a.b", "w[1]*.yml", &["refs/heads/main"]);
    assert_eq!(
        signer_identity(&both),
        r"^https://github\.com/a\.b/[^/]+/w\[1\]\*\.yml@refs/[^@]+$"
    );
    let everything = signers(REGEX_META, REGEX_META, &["refs/heads/main"]);
    assert_eq!(
        signer_identity(&everything),
        format!("^https://github\\.com/{escaped}/[^/]+/{escaped}@refs/[^@]+$")
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
    for refused in ["/opt/tools/gh", "/opt/tools/trusted_root.json"] {
        let rig = rig_refusing(exit_zero, &[refused]);
        assert_eq!(
            rig.verifier.verify(&case.request()),
            Err(VerifyError::Unavailable),
            "{refused}"
        );
        assert_eq!(rig.spawns.borrow().len(), 0, "{refused}");
    }
}

/// The signer pattern, compiled by the `regex` crate, matches the whole signer URI of the
/// configured workflow in any repository of the organisation, and nothing else.
/// Catches a pattern that is only a prefix (a workflow named `artifacts.yml@other.example` passing as
/// `artifacts.yml`), an unanchored end, another organisation, a repository path of another
/// depth, and a suffix after the ref.
#[test]
fn the_signer_pattern_matches_the_complete_signer_uri_and_only_that() {
    let configured = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let pattern = regex::Regex::new(&signer_identity(&configured)).expect("a valid pattern");
    let base = "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml";
    for good in [
        format!("{base}@refs/heads/main"),
        format!("{base}@refs/tags/v1.2.3"),
        "https://github.com/example-org/other-tool/.github/workflows/artifacts.yml@refs/heads/x"
            .to_owned(),
    ] {
        assert!(pattern.is_match(&good), "{good}");
    }
    for bad in [
        "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml@other.example@refs/heads/main",
        "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml@refs/heads/main@x",
        "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml@",
        "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml@heads/main",
        "https://github.com/example-org/tool-a/.github/workflows/artifacts.yml",
        "https://github.com/other-org/tool-a/.github/workflows/artifacts.yml@refs/heads/main",
        "https://github.com/example-org/a/b/.github/workflows/artifacts.yml@refs/heads/main",
        "https://github.com/example-org//.github/workflows/artifacts.yml@refs/heads/main",
        "https://github.com/example-org/tool-a/.github/workflows/artifactsXyml@refs/heads/main",
        "https://github.com/example-org/tool-a/.github/workflows/release.yml@refs/heads/main",
        "xhttps://github.com/example-org/tool-a/.github/workflows/artifacts.yml@refs/heads/main",
    ] {
        assert!(!pattern.is_match(bad), "{bad}");
    }
    assert_eq!(IDENTITY, signer_identity(&configured));
}

/// A relative archive, bundle, `gh`, trusted root or home reaches `gh` as an absolute path
/// against the daemon's working directory, and the trust check is made on the absolute `gh` and
/// root.
/// Catches a relative path handed to a child that runs in another directory.
#[test]
fn relative_paths_reach_gh_as_absolute_paths() {
    let mut case = sample();
    case.archive = PathBuf::from("packages/svc/archive.tar.zst");
    case.bundle = PathBuf::from("packages/svc/.fetch-1-2.json");
    let relative = ["bin/gh", "etc/root.json", "state/home"];
    let rig = rig_at(relative, exit_zero, &[]);
    rig.verifier.verify(&case.request()).expect("attested");
    let absolute = |text: &str| std::path::absolute(text).expect("an absolute path");
    let spawns = rig.spawns.borrow();
    let spawn = &spawns[0];
    assert_eq!(spawn.program, absolute("bin/gh"));
    assert_eq!(spawn.cwd, absolute("state/home"));
    assert!(spawn.program.is_absolute() & spawn.cwd.is_absolute());
    assert_eq!(spawn.args[2], absolute("packages/svc/archive.tar.zst"));
    assert_eq!(spawn.args[4], absolute("packages/svc/.fetch-1-2.json"));
    assert_eq!(spawn.args[6], absolute("etc/root.json"));
    let home = absolute("state/home").display().to_string();
    assert!(spawn.env.contains(&("HOME".to_owned(), home.clone())));
    assert!(
        spawn
            .env
            .contains(&("GH_CONFIG_DIR".to_owned(), format!("{home}/config")))
    );

    let gh_abs = absolute("bin/gh").display().to_string();
    let refused = rig_at(relative, exit_zero, &[gh_abs.as_str()]);
    assert_eq!(
        refused.verifier.verify(&case.request()),
        Err(VerifyError::Unavailable)
    );
    assert_eq!(refused.spawns.borrow().len(), 0);
}

/// A path that cannot be made absolute (an empty one) gives no verdict and runs nothing.
/// Catches an empty path passed to the child, or reported as a rejection of the package.
#[test]
fn a_path_that_cannot_be_made_absolute_is_unavailable_and_nothing_runs() {
    for empty in 0..3 {
        let mut paths = PATHS;
        paths[empty] = "";
        let rig = rig_at(paths, exit_zero, &[]);
        assert_eq!(
            rig.verifier.verify(&sample().request()),
            Err(VerifyError::Unavailable),
            "path {empty}"
        );
        assert_eq!(rig.spawns.borrow().len(), 0, "path {empty}");
    }
    let mut no_archive = sample();
    no_archive.archive = PathBuf::new();
    let mut no_bundle = sample();
    no_bundle.bundle = PathBuf::new();
    for case in [no_archive, no_bundle] {
        let rig = rig(exit_zero);
        assert_eq!(
            rig.verifier.verify(&case.request()),
            Err(VerifyError::Unavailable)
        );
        assert_eq!(rig.spawns.borrow().len(), 0);
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

/// Makes `dir` a directory only its owner can write, whatever the umask.
fn protect(dir: &Path) {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("set the directory mode");
}

/// A regular file is trusted when its owner is the wanted uid and only the owner can write it.
fn trusted_file(dir: &Path, name: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    protect(dir);
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

/// A file in a directory the group or others can write is refused with the directory's mode, even
/// though the file itself is fine; the same file in a directory only its owner can write passes.
/// Catches a trusted file whose directory entry anyone else can replace.
#[test]
fn owned_by_refuses_a_file_in_a_directory_the_group_or_others_can_write() {
    let dir = scratch("gh-owned-parent");
    let path = trusted_file(dir.path(), "file", 0o644);
    let trust = OwnedBy { uid: uid_of(&path) };
    for mode in [0o777, 0o775, 0o757] {
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).expect("set the mode");
        let err = trust.check(&path).expect_err("a writable directory");
        assert_eq!(
            err.to_string(),
            format!("mode {mode:o} lets the group or others write it")
        );
        assert!(matches!(err, TrustError::Writable { mode: m } if m == mode));
    }
    for mode in [0o755, 0o700, 0o555] {
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).expect("set the mode");
        trust.check(&path).expect("a protected directory");
    }
    protect(dir.path());
}

/// The owner and mode judgment: the wanted owner or an extra one passes, another owner is
/// refused with both uids, and a group- or other-writable mode is refused with the mode.
/// Catches a directory owner other than the file's trusted uid or root accepted, and root refused.
#[test]
fn the_judgment_accepts_the_wanted_owner_or_root_for_a_directory_and_nobody_else() {
    judge(1000, 0o644, 1000, &[]).expect("the wanted owner");
    judge(1000, 0o755, 1000, &[0]).expect("the wanted owner of a directory");
    judge(0, 0o755, 1000, &[0]).expect("root owns the directory");
    let err = judge(0, 0o755, 1000, &[]).expect_err("root is not the file's owner here");
    assert_eq!(err.to_string(), "owned by uid 0, not the trusted uid 1000");
    let err = judge(7, 0o755, 1000, &[0]).expect_err("a third owner");
    assert!(matches!(
        err,
        TrustError::Owner {
            found: 7,
            wanted: 1000
        }
    ));
    let err = judge(0, 0o40775, 1000, &[0]).expect_err("a group-writable directory");
    assert!(matches!(err, TrustError::Writable { mode: 0o775 }));
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

/// A clock that reads 100 the first time it is looked at and 1000 ever after: the deadline passes
/// between the check before the start and the first poll.
struct PassesAfterOneLook {
    inner: FakeClock,
    looks: std::sync::atomic::AtomicU64,
}

impl Clock for PassesAfterOneLook {
    fn mono_secs(&self) -> u64 {
        match self.looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => 100,
            _ => 1000,
        }
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        self.inner.boot_id()
    }

    fn sleep_secs(&self, secs: u64) {
        self.inner.sleep_secs(secs);
    }
}

/// A deadline that has passed before the start is a timeout without anything started: the
/// program does not exist, yet no start error comes back, and no staging file is made.
/// Catches a child spawned (or a staging file created) after the deadline.
#[test]
fn a_deadline_that_has_passed_before_the_start_starts_nothing() {
    let dir = scratch("gh-run-expired");
    let clock = Arc::new(FakeClock::yielding(100));
    let runner = SystemRunner::new(clock.clone());
    for deadline in [100, 99, 0] {
        let mut spawn = self_spawn(dir.path(), &[], clock.stamp(deadline));
        spawn.program = dir.path().join("no-such-program");
        assert_eq!(
            runner.run(&spawn).expect("a timeout, not a start error"),
            Ran {
                code: None,
                timed_out: true,
                stderr: Vec::new(),
            }
        );
    }
    assert_eq!(clock.sleeps(), 0);
    assert_eq!(listing(dir.path()), Vec::<String>::new());
}

/// A deadline that passes after the start but before the first poll wins over a child that
/// exits at once: the run is a timeout, with no exit code.
/// Catches an exit observed at or after the deadline being taken for an answer in time.
#[test]
fn a_deadline_that_passes_before_the_first_poll_wins_over_the_childs_exit() {
    let dir = scratch("gh-run-expiry-wins");
    let clock = Arc::new(PassesAfterOneLook {
        inner: FakeClock::at(100),
        looks: std::sync::atomic::AtomicU64::new(0),
    });
    let runner = SystemRunner::new(clock.clone());
    let spawn = self_spawn(dir.path(), &["--list"], clock.inner.stamp(500));
    let ran = runner.run(&spawn).expect("the child ran");
    assert_eq!(ran.code, None);
    assert!(ran.timed_out);
    assert_eq!(staging_files(dir.path()), Vec::<String>::new());
    clock.sleep_secs(1);
    assert_eq!(clock.inner.sleeps(), 1);
}
