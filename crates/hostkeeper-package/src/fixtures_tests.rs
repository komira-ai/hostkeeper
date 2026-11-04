//! The committed attestation bundles under `tests/fixtures/`: each is byte for byte what
//! `testing::bundle_json` makes of its specification here, each parses, and each is accepted or
//! refused for the reason the fixtures' README names. The verifier is the real `GhVerifier` with
//! a fake `gh` that applies each flag it is given to the bundle's (unsigned) certificate.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::daemon::Bounds;
use hostkeeper_config::trust::TrustError;
use serde_json::Value;

use crate::bundle::{self, Facts};
use crate::der;
use crate::fetch::{FetchEnv, fetch_package};
use crate::gh::{FileTrust, GhVerifier, Ran, Runner, Spawn};
use crate::ports::{Attest, Verifier};
use crate::prove::{ProveEnv, prove};
use crate::testing::{
    BUILD_SIGNER_URI, BundleSpec, FakeClock, FakeFetcher, RUNNER_ENVIRONMENT,
    SOURCE_REPOSITORY_DIGEST, SOURCE_REPOSITORY_OWNER_URI, bundle_json, config, digest, listing,
    order, scratch, serve, sha256_hex, signers,
};
use crate::{NOT_ATTESTED, PackageFailure};

/// The bytes of `good.tar.zst`, and their SHA-256.
const ARCHIVE: &[u8] = b"the archive the fixture bundles attest\n";
const ARCHIVE_SHA256: &str = "0945ab7be8afba90be37c6f80edcac4bc03ef09e31a0c4d616b19541e50de412";
/// The SHA-256 of `other bytes than the archive\n`.
const OTHER_SHA256: &str = "06731cc028fadef66cec9b104b1753ff425ebec9c04d181fd36cc8d5ceb679fc";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
const WORKFLOW: &str = ".github/workflows/release.yml";

/// One fixture: its file, what it was made from, the reason the test expects (the README's last
/// column), and how many times `gh` runs for it.
struct Fixture {
    file: &'static str,
    spec: BundleSpec<'static>,
    reason: &'static str,
    gh_runs: u32,
}

fn good() -> BundleSpec<'static> {
    BundleSpec {
        organisation: "example-org",
        repository: "tool-a",
        workflow: WORKFLOW,
        source_ref: "refs/tags/v1.2.3",
        certificate_commit: COMMIT,
        provenance_commit: COMMIT,
        runner: "github-hosted",
        subject_name: "good.tar.zst",
        subject_sha256: ARCHIVE_SHA256,
    }
}

/// Every fixture. A new file needs a row here and in the README.
fn fixtures() -> Vec<Fixture> {
    let fixture = |file, spec, reason, gh_runs| Fixture {
        file,
        spec,
        reason,
        gh_runs,
    };
    vec![
        fixture("good.sigstore.json", good(), "accepted", 1),
        fixture(
            "other-bytes.sigstore.json",
            BundleSpec {
                subject_sha256: OTHER_SHA256,
                ..good()
            },
            "the digest check before gh",
            0,
        ),
        fixture(
            "other-workflow.sigstore.json",
            BundleSpec {
                workflow: ".github/workflows/nightly.yml",
                ..good()
            },
            "gh: --cert-identity-regex",
            1,
        ),
        fixture(
            "other-organisation.sigstore.json",
            BundleSpec {
                organisation: "other-org",
                ..good()
            },
            "gh: --owner",
            1,
        ),
        fixture(
            "self-hosted.sigstore.json",
            BundleSpec {
                runner: "self-hosted",
                ..good()
            },
            "gh: --deny-self-hosted-runners",
            1,
        ),
        fixture(
            "disallowed-ref.sigstore.json",
            BundleSpec {
                source_ref: "refs/heads/main",
                ..good()
            },
            "the ref refs/heads/main matches none of the trusted signer's refs",
            0,
        ),
        fixture(
            "commit-differs.sigstore.json",
            BundleSpec {
                provenance_commit: OTHER_COMMIT,
                ..good()
            },
            "gh: --source-digest",
            1,
        ),
    ]
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The configured signer of the fixtures' README.
fn configured() -> hostkeeper_config::daemon::TrustedSigners {
    signers("example-org", WORKFLOW, &["refs/tags/*"])
}

/// `text` with each `\` escape undone.
fn unquote(text: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    for c in text.chars() {
        if escaped | (c != '\\') {
            out.push(c);
            escaped = false;
        } else {
            escaped = true;
        }
    }
    out
}

/// The organisation and the workflow of a `--cert-identity-regex` value of `signer_identity`'s
/// form.
fn identity_parts(regex: &str) -> (String, String) {
    let rest = regex
        .strip_prefix(r"^https://github\.com/")
        .expect("the start of a signer URI");
    let (organisation, workflow) = rest.split_once("/[^/]+/").expect("any repository");
    let workflow = workflow
        .strip_suffix('@')
        .expect("the end of the workflow path");
    (unquote(organisation), unquote(workflow))
}

/// A `gh` that verifies nothing cryptographically but applies each flag it is given to the
/// bundle's certificate, in `gh`'s order, and exits 1 naming the first flag that does not hold.
struct FakeGh {
    runs: Rc<RefCell<u32>>,
    args: Rc<RefCell<Vec<String>>>,
}

impl Runner for FakeGh {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        *self.runs.borrow_mut() += 1;
        let args: Vec<String> = spawn
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        self.args.borrow_mut().clone_from(&args);
        let value = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|at| args.get(at + 1))
                .cloned()
                .unwrap_or_default()
        };
        let text = std::fs::read(value("--bundle"))?;
        let archive = std::fs::read(&args[2])?;
        let json: Value = serde_json::from_slice(&text).expect("a JSON bundle");
        let certificate = STANDARD
            .decode(
                json.pointer("/verificationMaterial/certificate/rawBytes")
                    .and_then(Value::as_str)
                    .expect("a certificate"),
            )
            .expect("base64");
        let field = |arc: u8| {
            der::utf8_extension(&certificate, &der::fulcio(arc))
                .unwrap_or_default()
                .to_owned()
        };
        let (organisation, workflow) = identity_parts(&value("--cert-identity-regex"));
        let signer = field(BUILD_SIGNER_URI);
        let identity = signer
            .strip_prefix(&format!("https://github.com/{organisation}/"))
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(repository, rest)| {
                !repository.is_empty() & rest.starts_with(&format!("{workflow}@"))
            });
        let hosted = args.iter().any(|arg| arg == "--deny-self-hosted-runners");
        let checks = [
            (
                "subject",
                bundle::read(&text).is_ok_and(|facts| facts.lists(&sha256_hex(&archive))),
            ),
            (
                "--owner",
                field(SOURCE_REPOSITORY_OWNER_URI)
                    == format!("https://github.com/{}", value("--owner")),
            ),
            ("--cert-identity-regex", identity),
            (
                "--deny-self-hosted-runners",
                !hosted | (field(RUNNER_ENVIRONMENT) == "github-hosted"),
            ),
            (
                "--source-ref",
                field(der::SOURCE_REPOSITORY_REF) == value("--source-ref"),
            ),
            (
                "--source-digest",
                field(SOURCE_REPOSITORY_DIGEST) == value("--source-digest"),
            ),
        ];
        Ok(match checks.iter().find(|(_, holds)| !holds) {
            None => Ran {
                code: Some(0),
                ..Ran::default()
            },
            Some((flag, _)) => Ran {
                code: Some(1),
                timed_out: false,
                stderr: format!("gh: {flag}\nmore detail").into_bytes(),
            },
        })
    }
}

/// Trusts every file: the fake `gh` and the paths it is given are not real files of a host.
struct TrustAll;

impl FileTrust for TrustAll {
    fn check(&self, _: &Path) -> Result<(), TrustError> {
        Ok(())
    }
}

/// A `GhVerifier` running the fake `gh`, and the fake's records.
struct Gh {
    verifier: GhVerifier,
    runs: Rc<RefCell<u32>>,
    args: Rc<RefCell<Vec<String>>>,
}

fn gh() -> Gh {
    let runs = Rc::new(RefCell::new(0));
    let args = Rc::new(RefCell::new(Vec::new()));
    let fake = FakeGh {
        runs: Rc::clone(&runs),
        args: Rc::clone(&args),
    };
    let verifier = GhVerifier::new(
        PathBuf::from("/opt/gh/bin/gh"),
        PathBuf::from("/opt/gh/trusted_root.jsonl"),
        PathBuf::from("/opt/gh/home"),
        Box::new(fake),
        Box::new(TrustAll),
    );
    Gh {
        verifier,
        runs,
        args,
    }
}

/// The reason one bundle is accepted or refused, and how many times `gh` ran: the prove step's
/// digest check first, then the real verifier with the fake `gh`.
fn verdict(archive: &Path, bundle: &Path, facts: &Facts) -> (String, u32) {
    if !facts.lists(ARCHIVE_SHA256) {
        return ("the digest check before gh".to_owned(), 0);
    }
    let gh = gh();
    let signers = configured();
    let sha256 = digest(ARCHIVE_SHA256);
    let deadline = FakeClock::at(1).stamp(100);
    let request = Attest {
        archive,
        bundle,
        sha256: &sha256,
        commit: &facts.commit,
        source_ref: &facts.source_ref,
        signers: &signers,
        deadline: &deadline,
    };
    let said = gh
        .verifier
        .verify(&request)
        .map_or_else(|refusal| refusal.to_string(), |_| "accepted".to_owned());
    let reason = said
        .strip_prefix("not attested by the trusted signer: ")
        .unwrap_or(&said)
        .to_owned();
    let ran = *gh.runs.borrow();
    (reason, ran)
}

/// The rows of the fixtures' README table: each file and its last column.
fn readme_rows() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixtures_dir().join("README.md")).expect("the README");
    let mut rows: Vec<(String, String)> = text
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| {
            let cells: Vec<&str> = rest.split(" | ").collect();
            let file = cells.first()?.strip_suffix('`')?;
            let reason = cells.last()?.strip_suffix(" |")?;
            Some((file.to_owned(), reason.to_owned()))
        })
        .collect();
    rows.sort();
    rows
}

/// Catches a fixture that no longer is what its README says, a README line missing for a new
/// fixture (or a fixture missing for a line), a file that differs from the generator's output,
/// a bundle the reader cannot parse, and a bad bundle refused for another reason than the one
/// it stands for: each bad one is refused by the check the README names (the digest and the
/// ref before `gh` runs, the rest by the flag `gh` is given for it), and the good one is
/// accepted.
#[test]
fn fixture_bundles_parse_and_the_bad_ones_are_rejected_for_the_reason_the_readme_names() {
    let dir = fixtures_dir();
    let archive = dir.join("good.tar.zst");
    assert_eq!(std::fs::read(&archive).expect("the archive"), ARCHIVE);
    assert_eq!(sha256_hex(ARCHIVE), ARCHIVE_SHA256);
    assert_eq!(sha256_hex(b"other bytes than the archive\n"), OTHER_SHA256);

    let mut table = Vec::new();
    for fixture in fixtures() {
        let path = dir.join(fixture.file);
        let text = std::fs::read_to_string(&path).expect("a fixture");
        assert_eq!(text, bundle_json(&fixture.spec), "{}", fixture.file);
        let facts = bundle::read(text.as_bytes()).expect("a fixture parses");
        assert_eq!(facts.commit.as_str(), fixture.spec.provenance_commit);
        assert_eq!(facts.source_ref.as_str(), fixture.spec.source_ref);
        assert_eq!(facts.subjects, [fixture.spec.subject_sha256]);
        let (reason, runs) = verdict(&archive, &path, &facts);
        assert_eq!(
            (reason.as_str(), runs),
            (fixture.reason, fixture.gh_runs),
            "{}",
            fixture.file
        );
        table.push((fixture.file.to_owned(), reason));
    }
    table.sort();
    assert_eq!(readme_rows(), table);
    let on_disk: Vec<String> = listing(&dir)
        .into_iter()
        .filter(|name| name.ends_with(".sigstore.json"))
        .collect();
    let named: Vec<String> = table.into_iter().map(|(file, _)| file).collect();
    assert_eq!(on_disk, named);
}

/// Fetches and proves the archive of `service` 1.2.3 whose bundle is the fixture `file`, with
/// the real verifier and the fake `gh`; returns the result and the arguments `gh` got.
fn prove_fixture(file: &str, service: &str) -> (Result<String, PackageFailure>, Vec<String>) {
    let scratch = scratch("fixture-prove");
    let url = format!("https://releases.example/{service}/{service}-1.2.3.tar.zst");
    let bundle = std::fs::read(fixtures_dir().join(file)).expect("the fixture");
    let fetcher = FakeFetcher::new()
        .route(&url, serve(ARCHIVE))
        .route(&format!("{url}.sigstore.json"), serve(&bundle));
    let clock = FakeClock::at(1);
    let bounds = Bounds::default();
    let channel = config("https://releases.example", &"0".repeat(64)).release_channel;
    let fetch_env = FetchEnv {
        fetcher: &fetcher,
        clock: &clock,
        bounds: &bounds,
        release_channel: &channel,
        packages_root: scratch.path(),
    };
    let staged = fetch_package(&fetch_env, &order(service, "1.2.3", ARCHIVE)).expect("fetched");
    let gh = gh();
    let signers = configured();
    let env = ProveEnv {
        fetcher: &fetcher,
        verifier: &gh.verifier,
        clock: &clock,
        bounds: &bounds,
        signers: &signers,
    };
    let proven = prove(&env, staged).map(|verified| verified.source_ref().as_str().to_owned());
    let args = gh.args.borrow().clone();
    (proven, args)
}

/// Catches design Q12 option (b) built by mistake: the good bundle, from the repository
/// `tool-a`, is accepted for the service `service-b`, and `gh` is asked for the organisation,
/// never for a repository or the service.
#[test]
fn no_repository_to_service_comparison_exists() {
    let (proven, args) = prove_fixture("good.sigstore.json", "service-b");
    assert_eq!(proven, Ok("refs/tags/v1.2.3".to_owned()));
    assert!(args.iter().any(|arg| arg == "--owner"), "{args:?}");
    let named = args
        .iter()
        .filter(|arg| {
            ["--repo", "--signer-repo"].contains(&arg.as_str())
                | (arg.contains("service-b") & !arg.starts_with('/'))
        })
        .count();
    assert_eq!(named, 0, "{args:?}");

    let (refused, _) = prove_fixture("other-organisation.sigstore.json", "service-b");
    assert_eq!(refused, Err(PackageFailure::release(NOT_ATTESTED)));
}
