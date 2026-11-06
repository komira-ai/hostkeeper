use super::*;
use crate::release::contract::Commit;
use crate::release::fakes::{FakeAdvisories, FakeExec, FakeRuns};
use crate::release::ports::Run;

/// cargo-deny 0.20.2 on a crate that locks smallvec 0.6.13 and time 0.1.43: two advisories and
/// the summary, as it wrote them to stderr (each advisory's date and description shortened).
const TWO: &str = include_str!("fixtures/cargo-deny-0.20.2-two-advisories.stderr");
/// The same on a lock file whose cfg-if 1.0.2 is yanked.
const YANKED_RUN: &str = include_str!("fixtures/cargo-deny-0.20.2-yanked.stderr");
/// The same on a lock file with nothing to report.
const CLEAN: &str = include_str!("fixtures/cargo-deny-0.20.2-clean.stderr");
/// The same with a `deny.toml` whose `[advisories]` has an unknown key.
const BAD_CONFIG: &str = include_str!("fixtures/cargo-deny-0.20.2-bad-config.stderr");

/// The body [`TWO`] renders to, byte for byte.
const TWO_BODY: &str = "\
`cargo deny check advisories` reports 2 findings against the dependencies in `Cargo.lock` on `main`.

| Advisory | Kind | Crate | Version | Title |
|---|---|---|---|---|
| [RUSTSEC-2020-0071](https://rustsec.org/advisories/RUSTSEC-2020-0071) | vulnerability | time | 0.1.43 | Potential segfault in the time crate |
| [RUSTSEC-2021-0003](https://rustsec.org/advisories/RUSTSEC-2021-0003) | vulnerability | smallvec | 0.6.13 | Buffer overflow in SmallVec::insert_many |

The advisories workflow (`.github/workflows/advisories.yml`) rewrites this issue daily and on every push to `main`, and closes it when none remain. Advisories never block a pull request.
";

fn said(success: bool, stderr: &str) -> Said {
    Said {
        success,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

/// What cargo-deny prints and how it exits when it reports findings: exit 1.
fn found(stderr: &str) -> Said {
    said(false, stderr)
}

fn repo() -> Repo {
    "komira-ai/hostkeeper".parse().expect("a repository")
}

fn finding(id: &str, krate: &str, version: &str, kind: &str, title: &str) -> Finding {
    Finding {
        id: id.to_owned(),
        krate: krate.to_owned(),
        version: version.to_owned(),
        kind: kind.to_owned(),
        title: title.to_owned(),
    }
}

fn two_findings() -> Vec<Finding> {
    vec![
        finding(
            "RUSTSEC-2020-0071",
            "time",
            "0.1.43",
            "vulnerability",
            "Potential segfault in the time crate",
        ),
        finding(
            "RUSTSEC-2021-0003",
            "smallvec",
            "0.6.13",
            "vulnerability",
            "Buffer overflow in SmallVec::insert_many",
        ),
    ]
}

fn issue(number: u64, title: &str, body: &str) -> Issue {
    Issue {
        number,
        title: title.to_owned(),
        body: body.to_owned(),
    }
}

/// Runs the verb with cargo-deny printing `stderr` (exit 1 unless it is [`CLEAN`]) against
/// `github`.
fn run(stderr: &str, github: &FakeRuns) -> Result<Vec<String>, Error> {
    let check = FakeAdvisories {
        said: said(stderr == CLEAN, stderr),
    };
    sync(&check, github, &repo())
}

fn writes(github: &FakeRuns) -> Vec<String> {
    github.writes.borrow().clone()
}

#[test]
fn two_advisories_parse_to_sorted_findings_and_render_the_expected_body() {
    // Catches: a verb that does not read cargo-deny's JSON records, takes the crate's version
    // from anywhere but the lock file's graph node, leaves the findings in cargo-deny's order,
    // or renders a body other than the table of id, kind, crate, version and title.
    let parsed = parse(&found(TWO)).expect("two advisories");
    assert_eq!(parsed, two_findings());
    assert_eq!(render(&parsed), TWO_BODY);
    assert_eq!(TITLE, "Security advisories in hostkeeper's dependencies");
}

#[test]
fn the_same_findings_in_another_order_render_a_byte_identical_body() {
    // Catches: a body that follows the order cargo-deny printed in, or lists a finding twice
    // when a crate is reached twice, so that the issue is rewritten when nothing changed.
    let mut lines: Vec<&str> = TWO.lines().collect();
    lines.reverse();
    let reversed = lines.join("\n");
    let doubled = format!("{TWO}\n{TWO}");
    for stderr in [reversed.as_str(), doubled.as_str()] {
        let parsed = parse(&found(stderr)).expect("findings");
        assert_eq!(parsed, two_findings());
        assert_eq!(render(&parsed), TWO_BODY);
    }
    // `render` sorts whatever order it is handed.
    let mut backwards = two_findings();
    backwards.reverse();
    backwards.push(backwards[0].clone());
    assert_eq!(render(&backwards), TWO_BODY);
}

#[test]
fn a_yanked_crate_is_a_finding_without_an_advisory_or_a_link() {
    // Catches: a yanked crate (deny.toml says `yanked = "deny"`) dropped because it has no
    // advisory object, so taking the check out of the gate would lose it.
    let parsed = parse(&found(YANKED_RUN)).expect("one yanked crate");
    let yanked = finding(
        YANKED,
        "cfg-if",
        "1.0.2",
        "yanked",
        "detected yanked crate (try `cargo update -p cfg-if`)",
    );
    assert_eq!(parsed, vec![yanked]);
    assert!(
        render(&parsed).contains(
            "| yanked | yanked | cfg-if | 1.0.2 | detected yanked crate (try `cargo update -p cfg-if`) |\n"
        ),
        "{}",
        render(&parsed)
    );
    assert!(render(&parsed).starts_with("`cargo deny check advisories` reports 1 finding against"));
}

#[test]
fn a_clean_run_has_no_findings() {
    // Catches: a finished run with nothing to report read as a failure, so the issue would
    // never be closed.
    assert_eq!(parse(&said(true, CLEAN)), Ok(vec![]));
    // Blank lines and a diagnostic below `error` that is no advisory are passed over.
    let warned = format!(
        "\n{}\n  \n{CLEAN}",
        r#"{"type":"diagnostic","fields":{"code":"advisory-not-detected","severity":"warning","message":"m"}}"#
    );
    assert_eq!(parse(&said(true, &warned)), Ok(vec![]));
}

#[test]
fn a_run_that_did_not_finish_or_failed_is_a_tool_failure() {
    // Catches: a failed check read as "no advisories", which would close the issue on a run
    // nobody understood: a usage error (not JSON), a deny.toml that does not parse, a database
    // that could not be fetched, a run cut off before its summary, and a failed exit with no
    // finding.
    assert_eq!(
        parse(&said(
            false,
            "error: invalid value 'advisoriez' for '[WHICH]...'\n"
        )),
        Err(Error::Output {
            line: 1,
            why: "not a JSON record (expected value at line 1 column 1): error: invalid value \
                  'advisoriez' for '[WHICH]...'"
                .to_owned()
        })
    );
    assert_eq!(
        parse(&said(false, BAD_CONFIG)).map_err(|e| e.to_string()),
        Err(
            "cargo deny output, line 1: an error that is no advisory (unexpected-keys): found 1 \
             unexpected keys, expected: [\"version\", \"db-path\", \"db-urls\", \"vulnerability\", \
             \"notice\", \"unmaintained\", \"unsound\", \"yanked\", \"ignore\", \
             \"severity-threshold\", \"git-fetch-with-cli\", \"disable-yank-checking\", \
             \"maximum-db-staleness\", \"unused-ignored-advisory\"]"
                .to_owned()
        )
    );
    let fetch = r#"{"type":"log","fields":{"level":"ERROR","message":"failed to fetch advisory database"}}"#;
    assert_eq!(
        parse(&said(false, &format!("{TWO}{fetch}\n"))),
        Err(Error::Tool("failed to fetch advisory database".to_owned()))
    );
    let warn_log = r#"{"type":"log","fields":{"level":"WARN","message":"stale database"}}"#;
    assert_eq!(
        parse(&said(true, &format!("{warn_log}\n{CLEAN}"))),
        Ok(vec![])
    );
    let first_advisory = TWO.lines().next().expect("a line");
    assert_eq!(
        parse(&found(first_advisory)),
        Err(Error::Unfinished),
        "a run cut off after one advisory"
    );
    assert_eq!(parse(&found("")), Err(Error::Unfinished));
    assert_eq!(parse(&said(false, CLEAN)), Err(Error::FailedWithoutFinding));
    assert_eq!(
        Error::FailedWithoutFinding.to_string(),
        "cargo deny exited non-zero and reported no advisory"
    );
    assert_eq!(
        Error::Unfinished.to_string(),
        "cargo deny printed no summary: the check did not finish"
    );
    assert_eq!(
        Error::Tool("x".to_owned()).to_string(),
        "cargo deny failed: x"
    );
    // A run that exits 0 with a finding (an advisory kind set to `warn`) still reports it.
    assert_eq!(parse(&said(true, TWO)), Ok(two_findings()));
}

#[test]
fn a_record_this_verb_does_not_read_is_refused_by_line() {
    // Catches: a record of another shape read as no finding: no `type`, a diagnostic with no
    // code, an advisory with no id or title, an advisory or yanked crate with no crate or
    // version in its graph; and a JSON value that is not an object.
    let advisory = |fields: &str| format!(r#"{{"type":"diagnostic","fields":{fields}}}"#);
    let graph = r#""graphs":[{"Krate":{"name":"time","version":"0.1.43"}}]"#;
    let cases: [(String, &str); 8] = [
        (r#"{"fields":{}}"#.to_owned(), "no `type`"),
        (r#"[1, 2]"#.to_owned(), "no `type`"),
        (advisory(r#"{"severity":"error"}"#), "no string `code`"),
        (
            advisory(&format!(
                r#"{{"code":"vulnerability",{graph},"advisory":{{"title":"t"}}}}"#
            )),
            "no string `id`",
        ),
        (
            advisory(&format!(
                r#"{{"code":"vulnerability",{graph},"advisory":{{"id":"RUSTSEC-2020-0071"}}}}"#
            )),
            "no string `title`",
        ),
        (
            advisory(
                r#"{"code":"vulnerability","advisory":{"id":"RUSTSEC-2020-0071","title":"t"}}"#,
            ),
            "vulnerability: graph crate: no string `name`",
        ),
        (
            advisory(
                r#"{"code":"unsound","graphs":[{"Krate":{"name":"time"}}],"advisory":{"id":"RUSTSEC-2020-0071","title":"t"}}"#,
            ),
            "unsound: graph crate: no string `version`",
        ),
        (
            advisory(&format!(r#"{{"code":"yanked",{graph}}}"#)),
            "no string `message`",
        ),
    ];
    for (record, why) in cases {
        assert_eq!(
            parse(&found(&format!("{CLEAN}{record}\n"))),
            Err(Error::Output {
                line: 2,
                why: why.to_owned()
            }),
            "{record}"
        );
    }
    // A null `advisory` is no advisory: a diagnostic of severity `error` is then a failure.
    assert_eq!(
        parse(&found(&advisory(
            r#"{"code":"vulnerability","severity":"error","message":"m","advisory":null}"#
        ))),
        Err(Error::Output {
            line: 1,
            why: "an error that is no advisory (vulnerability): m".to_owned()
        })
    );
}

#[test]
fn every_kind_of_advisory_is_a_finding() {
    // Catches: only vulnerabilities kept, so an unmaintained or unsound crate, or a notice,
    // never reaches the issue.
    let record = |code: &str, id: &str| {
        format!(
            r#"{{"type":"diagnostic","fields":{{"code":"{code}","severity":"warning","graphs":[{{"Krate":{{"name":"c","version":"1.0.0"}}}}],"advisory":{{"id":"{id}","title":"T {code}"}}}}}}"#
        )
    };
    let stderr = [
        record("unmaintained", "RUSTSEC-2024-0001"),
        record("unsound", "RUSTSEC-2024-0002"),
        record("notice", "RUSTSEC-2024-0003"),
        CLEAN.to_owned(),
    ]
    .join("\n");
    let parsed = parse(&said(true, &stderr)).expect("findings");
    let kinds: Vec<&str> = parsed.iter().map(|f| f.kind.as_str()).collect();
    assert_eq!(kinds, ["unmaintained", "unsound", "notice"]);
}

#[test]
fn a_cell_stays_one_line_and_only_a_rustsec_id_is_linked() {
    // Catches: a title with a `|` or a newline breaking the table, and an id that is not a
    // RustSec id written into a link.
    let odd = finding("GHSA-x|y", "c", "1.0.0", "vulnerability", "a | b\n  c\td");
    let body = render(&[odd]);
    assert!(
        body.contains("| GHSA-x\\|y | vulnerability | c | 1.0.0 | a \\| b c d |\n"),
        "{body}"
    );
    for (id, linked) in [
        ("RUSTSEC-2020-0071", true),
        ("RUSTSEC-2020-071", false),
        ("RUSTSEC-20200-0071", false),
        ("RUSTSEC-2020_0071", false),
        ("RUSTSEC-2020-007a", false),
        ("RUSTSEC-202a-0071", false),
        ("rustsec-2020-0071", false),
        ("RUSTSEC-2020-0071)", false),
        ("yanked", false),
    ] {
        assert_eq!(is_rustsec(id), linked, "{id}");
    }
}

#[test]
fn decide_closes_updates_or_creates_by_findings_and_the_open_issue() {
    // Catches: an empty set that leaves the issue open, findings that open a second issue
    // beside the open one, findings that open none, and an equal body written again.
    let some = two_findings();
    let open = issue(4, TITLE, "old");
    let current = issue(4, TITLE, TWO_BODY);
    assert_eq!(decide(&[], &render(&[]), Some(&open)), Action::Close(4));
    assert_eq!(decide(&[], &render(&[]), None), Action::Nothing);
    assert_eq!(decide(&some, TWO_BODY, Some(&open)), Action::Update(4));
    assert_eq!(decide(&some, TWO_BODY, None), Action::Create);
    assert_eq!(
        decide(&some, TWO_BODY, Some(&current)),
        Action::Unchanged(4)
    );
}

#[test]
fn the_issue_is_opened_then_kept_rewritten_and_closed_and_never_opened_twice() {
    // Catches: a run that opens a second issue while one is open, rewrites a body that did not
    // change, leaves a stale body, closes without a comment, or touches another issue.
    let github = FakeRuns::default();
    github
        .open_issue(&repo(), "Releases are held", "unrelated")
        .expect("another issue");

    assert_eq!(
        run(TWO, &github),
        Ok(vec!["2 found: opened issue #2".to_owned()])
    );
    assert_eq!(
        run(TWO, &github),
        Ok(vec!["2 found: issue #2 is up to date".to_owned()])
    );
    assert_eq!(
        run(YANKED_RUN, &github),
        Ok(vec!["1 found: updated issue #2".to_owned()])
    );
    assert_eq!(
        github.issues()[1].body,
        render(&parse(&found(YANKED_RUN)).expect("one"))
    );
    assert_eq!(
        run(CLEAN, &github),
        Ok(vec!["no advisories: closed issue #2".to_owned()])
    );
    assert_eq!(
        run(CLEAN, &github),
        Ok(vec!["no advisories, and no open issue".to_owned()])
    );

    assert_eq!(
        writes(&github),
        [
            "open #1".to_owned(),
            "open #2".to_owned(),
            "edit #2".to_owned(),
            format!("close #2: {CLOSE_COMMENT}"),
        ]
    );
    assert_eq!(
        github.issues(),
        vec![issue(1, "Releases are held", "unrelated")]
    );
}

#[test]
fn with_two_open_issues_of_the_title_the_lowest_number_is_the_one() {
    // Catches: a second issue opened, or the newer of two duplicates (opened by hand) kept
    // while the older goes stale.
    let github = FakeRuns::default();
    for body in ["first", "second"] {
        github.open_issue(&repo(), TITLE, body).expect("issue");
    }
    github.writes.borrow_mut().clear();
    assert_eq!(
        run(TWO, &github),
        Ok(vec!["2 found: updated issue #1".to_owned()])
    );
    assert_eq!(
        run(CLEAN, &github),
        Ok(vec!["no advisories: closed issue #1".to_owned()])
    );
    assert_eq!(
        writes(&github),
        ["edit #1".to_owned(), format!("close #1: {CLOSE_COMMENT}")]
    );
}

#[test]
fn a_tool_failure_writes_nothing() {
    // Catches: an issue closed or rewritten on a run that failed.
    let github = FakeRuns::default();
    github.open_issue(&repo(), TITLE, TWO_BODY).expect("issue");
    github.writes.borrow_mut().clear();
    assert!(run(BAD_CONFIG, &github).is_err());
    assert!(run("", &github).is_err());
    assert_eq!(writes(&github), Vec::<String>::new());
    assert_eq!(github.issues(), vec![issue(1, TITLE, TWO_BODY)]);
}

/// A GitHub whose every call fails, and a record of the calls asked of it.
#[derive(Default)]
struct Down {
    calls: std::cell::RefCell<Vec<&'static str>>,
}

impl Down {
    fn fail(&self, what: &'static str) -> PortError {
        self.calls.borrow_mut().push(what);
        PortError::new(format!("gh {what}"), "HTTP 502")
    }
}

impl GitHubRuns for Down {
    fn runs(&self, _: &Repo, _: &str, _: &Commit) -> Result<Vec<Run>, PortError> {
        Err(self.fail("runs"))
    }
    fn on_main(&self, _: &Repo, _: &Commit) -> Result<bool, PortError> {
        Err(self.fail("on_main"))
    }
    fn open_issues(&self, _: &Repo) -> Result<Vec<Issue>, PortError> {
        Err(self.fail("open_issues"))
    }
    fn open_issue(&self, _: &Repo, _: &str, _: &str) -> Result<u64, PortError> {
        Err(self.fail("open_issue"))
    }
    fn edit_issue(&self, _: &Repo, _: u64, _: &str) -> Result<(), PortError> {
        Err(self.fail("edit_issue"))
    }
    fn close_issue(&self, _: &Repo, _: u64, _: &str) -> Result<(), PortError> {
        Err(self.fail("close_issue"))
    }
}

/// A GitHub that lists `open` and whose every write fails.
struct ReadOnly {
    open: Vec<Issue>,
    down: Down,
}

impl GitHubRuns for ReadOnly {
    fn runs(&self, repo: &Repo, w: &str, c: &Commit) -> Result<Vec<Run>, PortError> {
        self.down.runs(repo, w, c)
    }
    fn on_main(&self, repo: &Repo, c: &Commit) -> Result<bool, PortError> {
        self.down.on_main(repo, c)
    }
    fn open_issues(&self, _: &Repo) -> Result<Vec<Issue>, PortError> {
        Ok(self.open.clone())
    }
    fn open_issue(&self, repo: &Repo, t: &str, b: &str) -> Result<u64, PortError> {
        self.down.open_issue(repo, t, b)
    }
    fn edit_issue(&self, repo: &Repo, n: u64, b: &str) -> Result<(), PortError> {
        self.down.edit_issue(repo, n, b)
    }
    fn close_issue(&self, repo: &Repo, n: u64, c: &str) -> Result<(), PortError> {
        self.down.close_issue(repo, n, c)
    }
}

#[test]
fn a_github_call_that_fails_fails_the_verb_with_its_reason() {
    // Catches: a failed list read as "no open issue" (which would open a second one), and a
    // failed open, rewrite or close reported as done.
    let check = |stderr: &str| FakeAdvisories {
        said: said(stderr == CLEAN, stderr),
    };
    let down = Down::default();
    assert_eq!(
        sync(&check(TWO), &down, &repo()).map_err(|e| e.to_string()),
        Err("gh open_issues: HTTP 502".to_owned())
    );
    assert_eq!(*down.calls.borrow(), ["open_issues"]);
    for (stderr, open, call) in [
        (TWO, vec![], "open_issue"),
        (TWO, vec![issue(3, TITLE, "old")], "edit_issue"),
        (CLEAN, vec![issue(3, TITLE, "old")], "close_issue"),
    ] {
        let github = ReadOnly {
            open,
            down: Down::default(),
        };
        assert_eq!(
            sync(&check(stderr), &github, &repo()),
            Err(Error::Port(PortError::new(
                format!("gh {call}"),
                "HTTP 502"
            )))
        );
        assert_eq!(*github.down.calls.borrow(), [call]);
    }
    // The fakes' other ports fail too, and are never asked by this verb.
    let commit = crate::release::fakes::nth_commit(1);
    assert!(down.runs(&repo(), "ci.yml", &commit).is_err());
    assert!(down.on_main(&repo(), &commit).is_err());
    let read_only = ReadOnly {
        open: vec![],
        down: Down::default(),
    };
    assert!(read_only.runs(&repo(), "ci.yml", &commit).is_err());
    assert!(read_only.on_main(&repo(), &commit).is_err());
}

#[test]
fn deny_cli_runs_cargo_deny_check_advisories_in_json_and_returns_a_failed_exit() {
    // Catches: `--format json` after `check` (cargo-deny refuses it there), the check of
    // anything but the advisories, a non-zero exit (every finding) turned into an error, and a
    // cargo that cannot start passed over.
    let line = [
        "cargo",
        "deny",
        "--log-level",
        "warn",
        "--format",
        "json",
        "--manifest-path",
        "./Cargo.toml",
        "--all-features",
        "check",
        "advisories",
    ];
    let deny = DenyCli::new(FakeExec::default().on(&line, false, "", TWO));
    assert_eq!(deny.check_advisories(), Ok(found(TWO)));
    let calls: Vec<Vec<String>> = deny.exec.calls.borrow().clone();
    assert_eq!(
        calls,
        vec![line.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()]
    );

    let missing = DenyCli::new(FakeExec::default());
    assert_eq!(
        missing.check_advisories(),
        Err(PortError::new(
            line.join(" "),
            format!("not scripted: {}", line.join(" "))
        ))
    );
}
