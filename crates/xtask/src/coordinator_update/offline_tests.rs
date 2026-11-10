//! The tests of `offline`: the pinned `gh` run offline with the daemon's environment and the
//! release signer's identity, the files it is trusted with, and what its answers mean.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::offline::*;
use crate::release::contract::{SIGNER_WORKFLOW, Signer};
use crate::release::fakes::{nth_commit, scratch};
use crate::release::ports::{Attestation, ExecSealed, PortError, Ran, Sealed, Verifier};

/// An [`ExecSealed`] that records each run and answers `answer`.
struct Recorder {
    runs: RefCell<Vec<Sealed>>,
    answer: Box<dyn Fn() -> std::io::Result<Ran>>,
}

impl Recorder {
    fn answering(answer: impl Fn() -> std::io::Result<Ran> + 'static) -> Recorder {
        Recorder {
            runs: RefCell::new(Vec::new()),
            answer: Box::new(answer),
        }
    }

    fn says(success: bool, stdout: &str, stderr: &[u8]) -> Recorder {
        let (stdout, stderr) = (stdout.as_bytes().to_vec(), stderr.to_vec());
        Recorder::answering(move || {
            Ok(Ran {
                success,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
            })
        })
    }
}

impl ExecSealed for Recorder {
    fn spawn_sealed(&self, run: &Sealed) -> std::io::Result<Ran> {
        self.runs.borrow_mut().push(run.clone());
        (self.answer)()
    }
}

/// A [`FileTrust`] that trusts every file but the one it names.
struct TrustAllBut(Option<PathBuf>);

impl FileTrust for TrustAllBut {
    fn check(&self, path: &Path) -> Result<(), String> {
        match &self.0 {
            Some(bad) if bad == path => Err("not this one".to_owned()),
            _ => Ok(()),
        }
    }
}

/// What `gh --format json` prints for one attestation by the release signer.
fn printed(digest: &str) -> String {
    json!([{
        "verificationResult": {
            "signature": {"certificate": {
                "buildSignerURI": format!("https://github.com/{SIGNER_WORKFLOW}@refs/heads/main"),
                "sourceRepositoryRef": "refs/heads/main",
                "sourceRepositoryDigest": nth_commit(7).to_string(),
            }},
            "statement": {"subject": [{"digest": {"sha256": digest}}]},
        }
    }])
    .to_string()
}

struct Setup {
    dir: crate::testdir::TestDir,
    gh: PathBuf,
    root: PathBuf,
    home: PathBuf,
}

fn setup(name: &str) -> Setup {
    let dir = scratch(name);
    Setup {
        gh: dir.join("bin/gh"),
        root: dir.join("lib/trusted_root.json"),
        home: dir.join("downloads/.gh"),
        dir,
    }
}

impl Setup {
    fn verify(
        &self,
        exec: &dyn ExecSealed,
        trust: &dyn FileTrust,
        bundle: Option<&Path>,
    ) -> Result<Vec<Attestation>, PortError> {
        let gh = OfflineGh {
            gh: &self.gh,
            trusted_root: &self.root,
            home: &self.home,
            exec,
            trust,
        };
        gh.verify(
            &Signer::hostkeeper(),
            &self.dir.join("image.tar"),
            bundle,
            &nth_commit(7),
        )
    }
}

#[test]
fn gh_runs_offline_with_the_daemons_environment_and_the_release_signers_identity() {
    // Proves: the pinned gh is run once, by absolute path, in its scratch home (made), with
    // exactly the daemon's verifier's environment (no token, the proxy that refuses every
    // connection) and the arguments that pin the release signer (repository, workflow, ref,
    // commit, hosted runner, issuer, predicate) to the bundle and the trusted root; what it
    // printed is read back for the caller to check again.
    let s = setup("cu-offline-run");
    let exec = Recorder::says(true, &printed("ab12"), b"");
    let bundle = s.dir.join("image.tar.sigstore.json");
    let attested = s
        .verify(&exec, &TrustAllBut(None), Some(&bundle))
        .expect("attested");
    assert_eq!(
        attested,
        vec![Attestation {
            signer_workflow: SIGNER_WORKFLOW.to_owned(),
            source_ref: "refs/heads/main".to_owned(),
            source_commit: nth_commit(7).to_string(),
            subjects: vec!["ab12".to_owned()],
        }]
    );
    let text = |p: &Path| p.display().to_string();
    assert_eq!(
        *exec.runs.borrow(),
        vec![Sealed {
            program: s.gh.clone(),
            args: [
                "attestation",
                "verify",
                &text(&s.dir.join("image.tar")),
                "--bundle",
                &text(&bundle),
                "--custom-trusted-root",
                &text(&s.root),
                "--cert-oidc-issuer",
                "https://token.actions.githubusercontent.com",
                "--predicate-type",
                "https://slsa.dev/provenance/v1",
                "--repo",
                "komira-ai/hostkeeper",
                "--signer-workflow",
                "komira-ai/hostkeeper/.github/workflows/artifacts.yml",
                "--source-ref",
                "refs/heads/main",
                "--source-digest",
                &nth_commit(7).to_string(),
                "--deny-self-hosted-runners",
                "--format",
                "json",
            ]
            .map(str::to_owned)
            .to_vec(),
            env: env(&s.home),
            cwd: s.home.clone(),
        }]
    );
    assert!(s.home.is_dir());
}

/// A [`hostkeeper_package::gh::Runner`] that is never run: the daemon's verifier is built only
/// to read its environment.
struct NeverRun;

impl hostkeeper_package::gh::Runner for NeverRun {
    fn run(
        &self,
        _: &hostkeeper_package::gh::Spawn,
    ) -> std::io::Result<hostkeeper_package::gh::Ran> {
        Err(std::io::Error::other("never run"))
    }
}

#[test]
fn the_environment_is_the_daemons_verifiers_word_for_word() {
    // Proves: the updater's gh gets the environment the daemon's verifier gives its gh, for the
    // same home: a change to one that the other does not follow fails here.
    let home = Path::new("/var/lib/hostkeeper-coordinator-update/.gh");
    let daemons = hostkeeper_package::gh::GhVerifier::new(
        PathBuf::from("/usr/lib/hostkeeper/gh"),
        PathBuf::from("/usr/lib/hostkeeper/trusted_root.json"),
        home.to_owned(),
        Box::new(NeverRun),
        Box::new(hostkeeper_package::gh::OwnedBy { uid: 0 }),
    );
    assert_eq!(env(home), daemons.env(home));
    assert_eq!(DEAD_PROXY, hostkeeper_package::gh::DEAD_PROXY);
    assert_eq!(ISSUER, hostkeeper_package::gh::ISSUER);
    assert_eq!(PREDICATE, hostkeeper_package::gh::PREDICATE);
    assert!(env(home).iter().all(|(name, _)| !name.contains("TOKEN")));
}

#[test]
fn nothing_runs_without_a_bundle_or_with_an_untrusted_gh_or_trusted_root() {
    // Proves: there is no online check (a request without a bundle runs nothing), and a gh or a
    // trusted root the trust check refuses runs nothing, each with the reason.
    let s = setup("cu-offline-refuse");
    let exec = Recorder::says(true, "[]", b"");
    let bundle = s.dir.join("b.json");
    assert_eq!(
        s.verify(&exec, &TrustAllBut(None), None),
        Err(PortError::new(
            "gh attestation verify (offline)",
            "the check is offline: it needs the bundle"
        ))
    );
    for (flag, bad) in [("--gh", &s.gh), ("--trusted-root", &s.root)] {
        assert_eq!(
            s.verify(&exec, &TrustAllBut(Some(bad.clone())), Some(&bundle)),
            Err(PortError::new(
                "gh attestation verify (offline)",
                format!("{flag} is not trusted: not this one")
            ))
        );
    }
    assert!(exec.runs.borrow().is_empty());
}

#[test]
fn a_refusal_of_gh_is_its_complaint_and_a_gh_that_cannot_run_or_answer_is_said() {
    // Proves: gh's non-zero exit is the check's failure, with every line of its complaint (gh
    // starts it with a blank line), cut and made printable; a silent failure, a gh that cannot
    // start, a reply that is not JSON, and a home that cannot be made each fail by name.
    let s = setup("cu-offline-gh");
    let bundle = s.dir.join("b.json");
    let what = "gh attestation verify (offline)";
    let check = |exec: &Recorder| s.verify(exec, &TrustAllBut(None), Some(&bundle));
    let refused = Recorder::says(
        false,
        "",
        b"\nError: verifying with issuer \"sigstore.dev\"\n",
    );
    assert_eq!(
        check(&refused),
        Err(PortError::new(
            what,
            "Error: verifying with issuer \"sigstore.dev\""
        ))
    );
    let two = Recorder::says(false, "", b"first\n\n  second \xff\n");
    assert_eq!(check(&two), Err(PortError::new(what, "first; second ?")));
    let long = Recorder::says(false, "", "x".repeat(500).as_bytes());
    assert_eq!(check(&long), Err(PortError::new(what, "x".repeat(300))));
    let silent = Recorder::says(false, "", b"");
    assert_eq!(
        check(&silent),
        Err(PortError::new(what, "it failed and said nothing"))
    );
    let missing = Recorder::answering(|| Err(std::io::Error::other("no such file")));
    assert_eq!(
        check(&missing),
        Err(PortError::new(
            what,
            format!("{} could not be started: no such file", s.gh.display())
        ))
    );
    let garbled = Recorder::says(true, "verified!", b"");
    let err = check(&garbled).expect_err("not JSON");
    assert!(
        err.to_string().starts_with(&format!("{what}: not JSON: ")),
        "{err}"
    );

    let s = setup("cu-offline-home");
    std::fs::write(s.dir.join("downloads"), "a file").expect("a file where the home goes");
    let ok = Recorder::says(true, "[]", b"");
    let err = s
        .verify(&ok, &TrustAllBut(None), Some(&bundle))
        .expect_err("no home");
    assert!(
        err.to_string()
            .starts_with(&format!("{what}: make {}: ", s.home.display())),
        "{err}"
    );
    assert!(ok.runs.borrow().is_empty());
}

#[test]
fn a_trusted_root_or_an_asset_whose_path_is_not_utf8_runs_nothing() {
    // Proves: gh is given every path as text; a trusted root or an archive whose path is not
    // UTF-8 is refused by name, and gh is not run.
    use std::os::unix::ffi::OsStrExt as _;
    let s = setup("cu-offline-utf8");
    let odd = s.dir.join(std::ffi::OsStr::from_bytes(b"r\xff.json"));
    let exec = Recorder::says(true, "[]", b"");
    let trust = TrustAllBut(None);
    let gh = OfflineGh {
        gh: &s.gh,
        trusted_root: &odd,
        home: &s.home,
        exec: &exec,
        trust: &trust,
    };
    let bundle = s.dir.join("b.json");
    let (signer, commit) = (Signer::hostkeeper(), nth_commit(7));
    assert_eq!(
        gh.verify(&signer, &s.dir.join("image.tar"), Some(&bundle), &commit),
        Err(PortError::new(
            "gh attestation verify (offline)",
            format!("the path {} is not UTF-8", odd.display())
        ))
    );
    let gh = OfflineGh {
        trusted_root: &s.root,
        ..gh
    };
    let err = gh
        .verify(&signer, &odd, Some(&bundle), &commit)
        .expect_err("not UTF-8");
    assert!(err.to_string().contains("is not UTF-8"), "{err}");
    assert!(exec.runs.borrow().is_empty());
}

#[test]
fn a_file_is_trusted_when_root_or_the_updater_owns_it_and_no_one_else_can_write_it() {
    // Proves the rule on owner and mode: root or the updater's own uid, and no write bit for the
    // group or others, for the file and for its directory alike.
    let me = OwnedByRootOr { uid: 1000 };
    let p = Path::new("/x/gh");
    assert_eq!(me.judge(p, 0, 0o100755), Ok(()));
    assert_eq!(me.judge(p, 1000, 0o100644), Ok(()));
    assert_eq!(
        me.judge(p, 1001, 0o100755),
        Err("/x/gh is owned by uid 1001, not by root or uid 1000".to_owned())
    );
    assert_eq!(
        me.judge(p, 0, 0o100775),
        Err("/x/gh is writable by its group or others (mode 775)".to_owned())
    );
    assert_eq!(
        me.judge(p, 1000, 0o40757),
        Err("/x/gh is writable by its group or others (mode 757)".to_owned())
    );
}

#[test]
fn the_trust_check_reads_the_file_and_its_directory_and_refuses_a_link() {
    // Proves, on files of this test's own user: a private file in a private directory is
    // trusted; a group-writable file, a group-writable directory, a link, a directory and a
    // missing file are not.
    let me = OwnedByRootOr {
        uid: rustix::process::geteuid().as_raw(),
    };
    let dir = scratch("cu-offline-trust");
    let lib = dir.join("lib");
    std::fs::create_dir(&lib).expect("a directory");
    let file = lib.join("gh");
    std::fs::write(&file, "gh").expect("a file");
    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    mode(&lib, 0o755);
    mode(&file, 0o755);
    assert_eq!(me.check(&file), Ok(()));
    mode(&file, 0o775);
    assert_eq!(
        me.check(&file),
        Err(format!(
            "{} is writable by its group or others (mode 775)",
            file.display()
        ))
    );
    mode(&file, 0o755);
    mode(&lib, 0o777);
    assert_eq!(
        me.check(&file),
        Err(format!(
            "{} is writable by its group or others (mode 777)",
            lib.display()
        ))
    );
    mode(&lib, 0o755);
    let link = lib.join("link");
    std::os::unix::fs::symlink(&file, &link).expect("a link");
    for not_a_file in [&link, &lib] {
        assert_eq!(
            me.check(not_a_file),
            Err(format!("{} is not a regular file", not_a_file.display()))
        );
    }
    let missing = lib.join("missing");
    let err = me.check(&missing).expect_err("missing");
    assert!(
        err.starts_with(&format!("{}: ", missing.display())),
        "{err}"
    );
}
