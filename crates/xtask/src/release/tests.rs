use std::cell::Cell;
use std::io::Read;
use std::path::Path;

use clap::Parser;

use super::*;
use crate::release::fakes::{
    FakeHistory, FakeStore, FakeVerifier, Fakes, nth_commit, scratch, write_assets,
};
use crate::release::ports::{Exec, Release, Run, Said};
use crate::testdir::TestDir;

const REPO: &str = contract::REPO;
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const STAMP: &str = "hostkeeper 0.1.2+0123456789ab";

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: Verb,
}

/// The verb the arguments after the program name spell, or clap's refusal.
pub(super) fn parse(args: &[&str]) -> Result<Verb, clap::Error> {
    Args::try_parse_from(std::iter::once("release").chain(args.iter().copied())).map(|a| a.verb)
}

/// The verb the arguments after the program name spell.
pub(super) fn verb(args: &[&str]) -> Verb {
    parse(args).expect("parse the argv")
}

pub(super) fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

pub(super) fn empty() -> Fakes {
    Fakes::over(FakeHistory::default())
}

/// Runs `v` and tells whether it asked for its adapters.
pub(super) fn connects(v: Verb) -> (Result<Vec<String>, Error>, bool) {
    let connected = Cell::new(false);
    let out = run(v, || {
        connected.set(true);
        empty()
    });
    (out, connected.get())
}

/// A history of one commit holding `VERSION` 0.1, and that commit.
fn one_commit() -> (FakeHistory, Commit) {
    let mut h = FakeHistory::default();
    let c = h.commit(None, &[("VERSION", Some("0.1\n"))]);
    (h, c)
}

/// A directory of the assets of `ver`.
fn assets(name: &str, ver: &Version) -> TestDir {
    let dir = scratch(name);
    write_assets(&dir, ver, "x");
    dir
}

/// Adds a successful push run on `main` of each required workflow for `commit`.
fn green(fakes: &Fakes, commit: &Commit) {
    for workflow in contract::REQUIRED_WORKFLOWS {
        fakes.runs.push(workflow, commit, "success");
    }
}

/// The writes of `store` that start with `prefix`, in order.
fn writes_with(store: &FakeStore, prefix: &str) -> Vec<String> {
    store
        .writes
        .borrow()
        .iter()
        .filter(|w| w.starts_with(prefix))
        .cloned()
        .collect()
}

fn publish_verb(dir: &Path, ver: &Version, commit: &Commit) -> Verb {
    verb(&[
        "publish",
        text(dir),
        "--repo",
        REPO,
        "--version",
        &ver.to_string(),
        "--commit",
        commit.as_str(),
    ])
}

fn run_of(id: u64, event: &str, branch: &str, sha: &str, status: &str, end: Option<&str>) -> Run {
    Run {
        id,
        event: event.to_owned(),
        branch: branch.to_owned(),
        head_sha: sha.to_owned(),
        status: status.to_owned(),
        conclusion: end.map(str::to_owned),
    }
}

#[test]
fn stamp_prints_the_stamp_without_asking_for_adapters() {
    // Catches: `stamp` printing another text than the contract's stamp, or building the real
    // adapters (git and gh) for a verb that reads no port.
    let (out, connected) = connects(verb(&["stamp", "--version", "0.1.2", "--commit", COMMIT]));
    assert_eq!(out, Ok(vec![STAMP.to_owned()]));
    assert!(!connected);
}

#[test]
fn set_version_compare_images_and_netboot_ask_for_no_adapters() {
    // Catches: `set-version`, `compare-images` or `netboot` building the real adapters, so that
    // they would need git and gh on the runner.
    let d = scratch("release-offline");
    let (out, connected) = connects(verb(&[
        "set-version",
        "--version",
        "0.1.9",
        "--root",
        text(&d),
    ]));
    assert!(matches!(out, Err(Error::Io { .. })), "{out:?}");
    assert!(!connected);

    std::fs::write(d.join("a"), b"same").expect("write");
    std::fs::write(d.join("b"), b"same").expect("write");
    let (out, connected) = connects(verb(&[
        "compare-images",
        text(&d.join("a")),
        text(&d.join("b")),
    ]));
    assert!(out.is_ok(), "{out:?}");
    assert!(!connected);

    // `netboot` reads the pins and nothing else.
    let (out, connected) = connects(verb(&[
        "netboot",
        "--pins",
        text(&d.join("absent.toml")),
        "--version",
        "0.1.9",
        "--dir",
        text(&d),
    ]));
    assert!(matches!(out, Err(Error::Io { .. })), "{out:?}");
    assert!(!connected);
}

#[test]
fn a_verb_that_reads_a_port_asks_for_the_adapters() {
    // Catches: the adapters never built for a verb that needs them (the closure not called).
    let (out, connected) = connects(verb(&["locate", "--repo", REPO, "--commit", COMMIT]));
    assert_eq!(
        out,
        Err(Error::Failed(format!(
            "{COMMIT} is not on main: a release is cut only from a commit on main"
        )))
    );
    assert!(connected);
}

#[test]
fn verbs_that_read_no_port_run_with_the_ports_lent_back() {
    // Catches: `run_with` failing a verb that reads no port, instead of handing it back to `run`.
    let fakes = empty();
    let ports = fakes.ports();
    let stamp = verb(&["stamp", "--version", "0.1.2", "--commit", COMMIT]);
    assert_eq!(run_with(stamp, &ports), Ok(vec![STAMP.to_owned()]));

    let d = scratch("release-lent");
    let set = verb(&["set-version", "--version", "0.1.9", "--root", text(&d)]);
    assert!(matches!(run_with(set, &ports), Err(Error::Io { .. })));

    std::fs::write(d.join("a"), b"same").expect("write");
    std::fs::write(d.join("b"), b"same").expect("write");
    let compare = verb(&["compare-images", text(&d.join("a")), text(&d.join("b"))]);
    assert!(run_with(compare, &ports).is_ok());

    let pins = d.join("pins.toml");
    let netboot = verb(&[
        "netboot",
        "--pins",
        text(&pins),
        "--version",
        "0.1.9",
        "--dir",
        text(&d),
    ]);
    assert!(matches!(run_with(netboot, &ports), Err(Error::Io { .. })));

    let absent = d.join("absent");
    let check = verb(&[
        "check-macos-pkg",
        text(&absent),
        "--version",
        "0.1.9",
        "--binary",
        text(&d.join("a")),
        "--gh",
        text(&d.join("a")),
        "--trusted-root",
        text(&d.join("a")),
    ]);
    assert!(matches!(run_with(check, &ports), Err(Error::Io { .. })));
}

#[test]
fn version_of_a_shallow_clone_is_refused() {
    // Catches: the version counted on a shallow clone, which holds only part of the history.
    let (mut h, c) = one_commit();
    h.shallow = true;
    let out = run(verb(&["version", "--commit", c.as_str()]), || {
        Fakes::over(h)
    });
    assert_eq!(out, Err(Error::Failed(version::SHALLOW.to_owned())));
}

#[test]
fn version_prints_the_version_and_appends_it_to_the_output_file() {
    // Catches: `--commit`, `--version-file` or `--output` not reaching the verb: no line, the
    // wrong number, or nothing appended for the workflow.
    let (h, c) = one_commit();
    let d = scratch("release-version");
    let output = d.join("out");
    let out = run(
        verb(&["version", "--commit", c.as_str(), "--output", text(&output)]),
        || Fakes::over(h),
    );
    assert_eq!(out, Ok(vec!["version=0.1.1".to_owned()]));
    assert_eq!(
        std::fs::read_to_string(&output).expect("output"),
        "version=0.1.1\n"
    );
}

#[test]
fn plan_with_the_freeze_set_releases_nothing() {
    // Catches: `--freeze` not reaching the plan, so a frozen repository still releases.
    let out = run(
        verb(&[
            "plan", "--repo", REPO, "--commit", COMMIT, "--freeze", "true",
        ]),
        empty,
    );
    let lines = [
        "no release: RELEASE_FREEZE is set",
        "release=false",
        "matrix={\"include\":[]}",
    ];
    assert_eq!(out, Ok(lines.iter().map(|l| (*l).to_owned()).collect()));
}

#[test]
fn check_and_publish_refuse_an_empty_directory_and_publish_nothing() {
    // Catches: a directory without the assets passing the check, or the store written to before
    // the check.
    let d = scratch("release-empty");
    let check = verb(&["check", text(&d), "--version", "0.1.2", "--commit", COMMIT]);
    assert!(matches!(
        run(check, empty),
        Err(Error::Refused(lines)) if !lines.is_empty()
    ));

    let fakes = empty();
    let ver: Version = "0.1.2".parse().expect("version");
    let commit: Commit = COMMIT.parse().expect("commit");
    let out = run_with(publish_verb(&d, &ver, &commit), &fakes.ports());
    assert!(matches!(out, Err(Error::Refused(_))), "{out:?}");
    assert_eq!(fakes.store.writes.borrow().clone(), Vec::<String>::new());
}

#[test]
fn check_binary_accepts_the_exact_stamp() {
    // Catches: `check-binary` not reading the binary through the port, or refusing the exact
    // stamp the build prints.
    let mut fakes = empty();
    fakes.run_version.said = Said {
        success: true,
        stdout: format!("{STAMP}\n"),
        stderr: String::new(),
    };
    let out = run(
        verb(&["check-binary", "target/hostkeeper", "0.1.2", COMMIT]),
        || fakes,
    );
    assert_eq!(out, Ok(vec![format!("target/hostkeeper: {STAMP}")]));
}

#[test]
fn answer_without_an_output_only_returns_the_lines() {
    // Catches: a file written when no `--output` was given.
    let lines = vec!["a=1".to_owned()];
    assert_eq!(answer(None, lines.clone()), Ok(lines));
}

#[test]
fn answer_appends_to_the_output_file() {
    // Catches: a second answer replacing the first (the workflow output file is appended to),
    // or a missing newline between them.
    let d = scratch("release-answer");
    let path = d.join("out");
    let first = vec!["a=1".to_owned(), "b=2".to_owned()];
    let second = vec!["c=3".to_owned()];
    assert_eq!(answer(Some(&path), first.clone()), Ok(first));
    assert_eq!(answer(Some(&path), second.clone()), Ok(second));
    assert_eq!(
        std::fs::read_to_string(&path).expect("output"),
        "a=1\nb=2\nc=3\n"
    );
}

#[test]
fn answer_to_a_directory_is_an_io_error() {
    // Catches: a failed write passed over, so a workflow output is lost without an error.
    let d = scratch("release-answer-dir");
    let path: &Path = &d;
    let why = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect_err("a directory cannot be opened for append")
        .to_string();
    assert_eq!(
        answer(Some(path), vec!["a=1".to_owned()]),
        Err(Error::Io {
            what: format!("append to {}", path.display()),
            why,
        })
    );
}

#[test]
fn file_sha256_hashes_the_bytes_of_the_file() {
    // Catches: a digest of the path or of part of the file.
    let d = scratch("release-sha");
    std::fs::write(d.join("abc"), b"abc").expect("write");
    assert_eq!(
        file_sha256(&d.join("abc")),
        Ok("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_owned())
    );
}

#[test]
fn file_sha256_of_a_missing_file_is_an_io_error_naming_it() {
    // Catches: a missing file hashed as empty.
    let d = scratch("release-sha-missing");
    let path = d.join("missing");
    let why = std::fs::File::open(&path)
        .expect_err("no such file")
        .to_string();
    assert_eq!(
        file_sha256(&path),
        Err(Error::Io {
            what: format!("read {}", path.display()),
            why,
        })
    );
}

#[test]
fn file_sha256_of_a_directory_is_an_io_error_from_the_read() {
    // Catches: a read that fails after the file opened ending the hash early, which would give
    // the digest of the bytes read so far instead of an error.
    let d = scratch("release-sha-dir");
    let path: &Path = &d;
    let why = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut [0_u8; 1]))
        .expect_err("a directory cannot be read")
        .to_string();
    assert_eq!(
        file_sha256(path),
        Err(Error::Io {
            what: format!("read {}", path.display()),
            why,
        })
    );
}

#[test]
fn lent_ports_answer_for_the_verb_that_reads_them() {
    // Catches: ports lent back to `run` that are not the ones lent (an empty set, or a copy that
    // lost the runs), so that a verb run through them would see no workflow run.
    let fakes = empty();
    let commit: Commit = COMMIT.parse().expect("commit");
    fakes.runs.on_main.borrow_mut().push(commit.clone());
    green(&fakes, &commit);
    let locate = verb(&["locate", "--repo", REPO, "--commit", COMMIT]);
    assert_eq!(
        run(locate, || Lent(fakes.ports())),
        Ok(vec![
            format!(
                "{COMMIT} is on main; ci.yml run 1000, privileged.yml run 1001 and artifacts.yml run 1002 succeeded"
            ),
            "run-id=1002".to_owned(),
        ])
    );
}

#[test]
fn errors_read_as_one_message() {
    // Catches: a context lost from an error: the operation, the port or the lines.
    let io = Error::io("read x")(std::io::Error::other("boom"));
    assert_eq!(
        io,
        Error::Io {
            what: "read x".to_owned(),
            why: "boom".to_owned(),
        }
    );
    assert_eq!(io.to_string(), "read x: boom");
    assert_eq!(
        Error::from(PortError::new("git rev-list", "bad")).to_string(),
        "git rev-list: bad"
    );
    assert_eq!(Error::Failed("no".to_owned()).to_string(), "no");
    assert_eq!(
        Error::Refused(vec!["a".to_owned(), "b".to_owned()]).to_string(),
        "a\nb"
    );
}

#[test]
fn a_run_from_a_pull_request_or_another_sha_never_counts() {
    // Catches: a pull-request run, a run on another branch or a run for another commit counted
    // as the push run on main, which would release a commit nobody tested.
    let c = nth_commit(7);
    let other = nth_commit(8);
    let mut runs = vec![run_of(
        1,
        "pull_request",
        "feature",
        c.as_str(),
        "completed",
        Some("success"),
    )];
    assert_eq!(state(&runs, &c), RunState::Missing);
    runs.push(run_of(
        2,
        "push",
        "feature",
        c.as_str(),
        "completed",
        Some("success"),
    ));
    assert_eq!(state(&runs, &c), RunState::Missing);
    runs.push(run_of(
        3,
        "push",
        "main",
        other.as_str(),
        "completed",
        Some("success"),
    ));
    assert_eq!(state(&runs, &c), RunState::Missing);
    runs.push(run_of(
        4,
        "push",
        "main",
        c.as_str(),
        "completed",
        Some("success"),
    ));
    assert_eq!(state(&runs, &c), RunState::Succeeded(4));
    assert_eq!(state(&[], &c), RunState::Missing);
}

#[test]
fn state_tells_each_way_a_run_can_stand() {
    // Catches: a run that is not green counted as green, a red one as pending, or a cancelled
    // one as a failure.
    let c = nth_commit(7);
    for (status, end, want) in [
        ("completed", Some("success"), RunState::Succeeded(5)),
        ("completed", Some("failure"), RunState::Failed(5)),
        ("completed", Some("timed_out"), RunState::Failed(5)),
        ("completed", Some("startup_failure"), RunState::Failed(5)),
        (
            "completed",
            Some("cancelled"),
            RunState::Other(5, "cancelled".to_owned()),
        ),
        (
            "completed",
            Some("skipped"),
            RunState::Other(5, "skipped".to_owned()),
        ),
        ("completed", None, RunState::Other(5, "none".to_owned())),
        ("in_progress", None, RunState::Pending(5)),
        ("queued", None, RunState::Pending(5)),
    ] {
        let runs = [run_of(5, "push", "main", c.as_str(), status, end)];
        assert_eq!(state(&runs, &c), want, "{status} {end:?}");
    }
}

#[test]
fn the_latest_run_decides_whatever_the_order_of_the_list() {
    // Catches: a re-run that went green not lifting an earlier failure, or an old green run
    // hiding a later failure.
    let c = nth_commit(7);
    let failed = |id| run_of(id, "push", "main", c.as_str(), "completed", Some("failure"));
    let passed = |id| run_of(id, "push", "main", c.as_str(), "completed", Some("success"));
    assert_eq!(state(&[failed(5), passed(9)], &c), RunState::Succeeded(9));
    assert_eq!(state(&[passed(9), failed(5)], &c), RunState::Succeeded(9));
    assert_eq!(state(&[failed(9), passed(5)], &c), RunState::Failed(9));
}

#[test]
fn three_plans_for_one_sha_publish_once() {
    // Catches: a re-run or a repeated trigger for one SHA creating a second release, a second
    // draft or a second publish.
    let mut h = FakeHistory::default();
    let root = h.commit(
        None,
        &[("VERSION", Some("0.1\n")), ("src/lib.rs", Some("one"))],
    );
    let c = h.commit(Some(&root), &[("src/lib.rs", Some("two"))]);
    let ver = version::version(&h, &c, "VERSION").expect("version");
    let mut fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.verifier = FakeVerifier::good(&c);
    let dir = assets("release-three-plans", &ver);

    let mut released = 0;
    for _ in 0..3 {
        let plan = verb(&["plan", "--repo", REPO, "--commit", c.as_str()]);
        let lines = run_with(plan, &fakes.ports()).expect("plan");
        if lines.contains(&"release=true".to_owned()) {
            released += 1;
            run_with(publish_verb(&dir, &ver, &c), &fakes.ports()).expect("publish");
        }
    }

    let tag = ver.tag();
    assert_eq!(released, 1);
    let all = fakes.store.all();
    assert_eq!(all.len(), 1);
    assert!(!all[0].draft);
    assert_eq!(all[0].tag, tag);
    assert_eq!(all[0].commit, c.to_string());
    assert_eq!(
        writes_with(&fakes.store, "create "),
        vec![format!("create {tag}")]
    );
    assert_eq!(
        writes_with(&fakes.store, "publish "),
        vec![format!("publish {tag}")]
    );
}

#[test]
fn a_plan_from_a_tip_whose_ci_is_pending_publishes_the_done_commits_oldest_first() {
    // Catches: the plan step of release.yml, as every trigger runs it, planning only the tip
    // whose ci run has not completed, so the earlier commits whose runs are all done are never
    // released; or those published newest first, under the number of the tip, or with the
    // assets of another commit.
    let mut h = FakeHistory::default();
    let root = h.commit(
        None,
        &[("VERSION", Some("0.1\n")), ("src/lib.rs", Some("0"))],
    );
    let r = h.commit(Some(&root), &[("src/lib.rs", Some("1"))]);
    let a = h.commit(Some(&r), &[("src/lib.rs", Some("2"))]);
    let b = h.commit(Some(&a), &[("src/lib.rs", Some("3"))]);
    let tip = h.commit(Some(&b), &[("src/lib.rs", Some("4"))]);
    let fakes = Fakes::over(h);
    fakes.store.insert(Release {
        tag: "v0.1.2".to_owned(),
        commit: r.to_string(),
        draft: false,
        assets: Vec::new(),
    });
    green(&fakes, &a);
    green(&fakes, &b);
    fakes.runs.push("privileged.yml", &tip, "success");
    fakes.runs.push("artifacts.yml", &tip, "success");
    fakes.runs.add("ci.yml", &tip, "push", "main", None);

    let plan = verb(&[
        "plan",
        "--repo",
        REPO,
        "--commit",
        tip.as_str(),
        "--version-file",
        "VERSION",
        "--freeze",
        "",
    ]);
    let lines = run_with(plan, &fakes.ports()).expect("plan");
    let matrix = lines
        .iter()
        .find_map(|l| l.strip_prefix("matrix="))
        .expect("a matrix line");
    let matrix: serde_json::Value = serde_json::from_str(matrix).expect("matrix JSON");
    let entries = matrix["include"].as_array().expect("an include array");
    let tags: Vec<&str> = entries
        .iter()
        .map(|e| e["tag"].as_str().expect("a tag"))
        .collect();
    assert_eq!(tags, vec!["v0.1.3", "v0.1.4"]);

    for (i, entry) in entries.iter().enumerate() {
        let ver: Version = entry["version"]
            .as_str()
            .expect("a version")
            .parse()
            .expect("version");
        let commit: Commit = entry["commit"]
            .as_str()
            .expect("a commit")
            .parse()
            .expect("commit");
        let dir = assets(&format!("release-scheduled-{i}"), &ver);
        let verifier = FakeVerifier::good(&commit);
        let ports = Ports {
            git: &fakes.git,
            runs: &fakes.runs,
            store: &fakes.store,
            verifier: &verifier,
            run_version: &fakes.run_version,
            pkgbuild: &fakes.pkgbuild,
            advisories: &fakes.advisories,
            download: &fakes.download,
            registry: &fakes.registry,
            wait: &fakes.wait,
        };
        run_with(publish_verb(&dir, &ver, &commit), &ports).expect("publish");
        assert_eq!(fakes.store.all()[i + 1].commit, commit.to_string());
    }

    assert_eq!(
        writes_with(&fakes.store, "create "),
        vec!["create v0.1.3".to_owned(), "create v0.1.4".to_owned()]
    );
    assert_eq!(
        writes_with(&fakes.store, "publish "),
        vec!["publish v0.1.3".to_owned(), "publish v0.1.4".to_owned()]
    );
    assert!(fakes.store.all().iter().all(|r| !r.draft));
}

/// An [`Exec`] that starts nothing: every program fails to start, with its name in the error.
#[derive(Clone)]
struct StartsNothing;

impl Exec for StartsNothing {
    fn spawn(&self, program: &str, _args: &[String]) -> std::io::Result<ports::Ran> {
        Err(std::io::Error::other(format!(
            "{program} is not started in a test"
        )))
    }
}

#[test]
fn the_wired_adapters_route_each_port_to_its_program() {
    // Catches: a port of the real adapters wired to the wrong adapter (the runs, the store or
    // the verifier answered by git, the history by gh, `--version` run through gh, or a
    // download not run through curl).
    let wired = Wired::new(StartsNothing);
    let ports = wired.ports();
    let c = nth_commit(7);
    let repo: Repo = REPO.parse().expect("repo");
    let why = |program: &str| format!("{program} is not started in a test");
    assert_eq!(
        ports.git.is_shallow(),
        Err(PortError::new(
            "git rev-parse --is-shallow-repository",
            why("git")
        ))
    );
    assert_eq!(
        ports.runs.on_main(&repo, &c),
        Err(PortError::new(
            format!("gh api repos/{REPO}/compare/main...{c}"),
            why("gh")
        ))
    );
    assert_eq!(
        ports.store.publish(&repo, "v0.1.2"),
        Err(PortError::new(
            format!("gh release edit v0.1.2 --repo {REPO} --draft=false"),
            why("gh")
        ))
    );
    let signer = contract::Signer::hostkeeper();
    let verified = ports.verifier.verify(&signer, Path::new("a.deb"), None, &c);
    assert_eq!(verified.map_err(|e| e.why), Err(why("gh")));
    assert_eq!(
        ports.run_version.version_of(Path::new("bin/x")),
        Err(PortError::new("bin/x --version", why("bin/x")))
    );
    let url = "https://example.com/v1/a.zip";
    assert_eq!(
        ports.download.download(url, Path::new("d/a.zip")),
        Err(PortError::new(format!("curl {url}"), why("curl")))
    );
    let digest = format!("sha256:{}", "ab".repeat(32));
    assert_eq!(
        ports.registry.served("r", &digest),
        Err(PortError::new(
            format!("skopeo inspect --raw docker://r@{digest}"),
            why("skopeo")
        ))
    );
}

#[test]
fn the_verbs_that_built_archives_indexes_and_the_release_channel_are_not_taken() {
    // Catches: a verb of the archive package, the release indexes or the release channel still
    // parsed, so that a workflow step calling it would build or stage what no host reads.
    for verb in [
        "package",
        "index",
        "channel-stage",
        "channel-sync",
        "channel-host",
    ] {
        let refused = parse(&[verb]).err().map(|e| e.kind());
        assert_eq!(
            refused,
            Some(clap::error::ErrorKind::InvalidSubcommand),
            "{verb}"
        );
    }
}
