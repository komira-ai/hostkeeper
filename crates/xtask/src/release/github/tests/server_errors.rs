//! A read that GitHub answers with a server error is asked again after a wait, up to
//! `READ_ATTEMPTS` attempts in all; any other failure of a read, and every failure of a write,
//! is the answer at once.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

use super::*;
use crate::release::ports::Ran;

/// An `Exec` that answers its calls, whatever they are, with its answers in turn (an unscripted
/// call cannot start), and records each call's command line.
#[derive(Default)]
struct InTurn {
    answers: RefCell<VecDeque<Ran>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl InTurn {
    /// The answers `(success, stdout, stderr)`, in the order the calls get them.
    fn of(answers: &[(bool, &str, &str)]) -> InTurn {
        let answers = answers
            .iter()
            .map(|(success, stdout, stderr)| Ran {
                success: *success,
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            })
            .collect();
        InTurn {
            answers: RefCell::new(answers),
            calls: RefCell::default(),
        }
    }
}

impl Exec for InTurn {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        let mut line = vec![program.to_owned()];
        line.extend(args.iter().cloned());
        self.calls.borrow_mut().push(line);
        self.answers
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| std::io::Error::other("not scripted"))
    }
}

thread_local! {
    static WAITED: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
}

/// The wait of these tests: records how long it was asked to wait.
fn record(d: Duration) {
    WAITED.with(|w| w.borrow_mut().push(d));
}

/// The waits recorded on this thread so far, which are then forgotten.
fn waited() -> Vec<Duration> {
    WAITED.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

/// A `gh` over the answers `answers` whose waits are recorded.
fn in_turn(answers: &[(bool, &str, &str)]) -> Gh<InTurn> {
    waited();
    Gh {
        exec: InTurn::of(answers),
        wait: Sleep(record),
    }
}

fn secs(all: &[u64]) -> Vec<Duration> {
    all.iter().copied().map(Duration::from_secs).collect()
}

const TIMED_OUT: &str = "gh: We couldn't respond to your request in time. Sorry about that. \
                         Please try resubmitting your request and contact us if the problem \
                         persists. (HTTP 504)\n";

fn releases_path() -> String {
    format!("repos/{REPO_NAME}/releases?per_page=100")
}

fn one_release() -> String {
    json!([{
        "tag_name": "v0.1.446",
        "target_commitish": commit().to_string(),
        "draft": true,
        "assets": [{ "name": "SHA256SUMS", "digest": format!("sha256:{}", digest()) }]
    }])
    .to_string()
}

fn the_draft() -> Vec<Release> {
    vec![Release {
        tag: "v0.1.446".to_owned(),
        commit: commit().to_string(),
        draft: true,
        assets: vec![StoredAsset {
            name: "SHA256SUMS".to_owned(),
            sha256: Some(digest()),
        }],
    }]
}

#[test]
fn a_release_list_that_times_out_once_is_asked_again_after_ten_seconds_and_read() {
    // Catches: the read-back of a fully uploaded draft failing `release publish` on one gateway
    // timeout of GitHub's, leaving the release a draft; or the second attempt asking for
    // something else than the first.
    let api = in_turn(&[(false, "", TIMED_OUT), (true, &one_release(), "")]);
    assert_eq!(api.releases(&repo()), Ok(the_draft()));
    let line = words(&["gh", "api", &releases_path()]);
    assert_eq!(*api.exec.calls.borrow(), vec![line.clone(), line]);
    assert_eq!(waited(), secs(&[10]));
}

#[test]
fn each_server_error_is_asked_again_and_every_read_asks_again() {
    // Catches: one of the four server errors (500, 502, 503, 504) read as final, or a read
    // (the runs, the jobs and issues of every page, the comparison with main) that does not
    // ask again.
    for code in ["500", "502", "503", "504"] {
        let stderr = format!("gh: Server Error (HTTP {code})\n");
        let api = in_turn(&[(false, "", &stderr), (true, &one_release(), "")]);
        assert_eq!(api.releases(&repo()), Ok(the_draft()), "HTTP {code}");
        assert_eq!(api.exec.calls.borrow().len(), 2, "HTTP {code}");
        assert_eq!(waited(), secs(&[10]), "HTTP {code}");
    }
    let api = in_turn(&[
        (false, "", TIMED_OUT),
        (true, r#"{"workflow_runs":[]}"#, ""),
    ]);
    assert_eq!(api.runs(&repo(), "ci.yml", &commit()), Ok(Vec::new()));
    let api = in_turn(&[(false, "", TIMED_OUT), (true, r#"[{"jobs":[]}]"#, "")]);
    assert_eq!(api.jobs(&repo(), 42), Ok(Vec::new()));
    let api = in_turn(&[(false, "", TIMED_OUT), (true, "[[]]", "")]);
    assert_eq!(api.open_issues(&repo()), Ok(Vec::new()));
    let api = in_turn(&[(false, "", TIMED_OUT), (true, r#"{"status":"behind"}"#, "")]);
    assert_eq!(api.on_main(&repo(), &commit()), Ok(true));
    assert_eq!(api.exec.calls.borrow().len(), 2);
}

#[test]
fn a_read_that_meets_server_errors_in_every_attempt_fails_with_the_last_after_three_waits() {
    // Catches: a read asked again for ever while GitHub is down, a fifth attempt, waits other
    // than 10, 30 and 60 seconds, or the error of an earlier attempt (or none) reported.
    let api = in_turn(&[
        (false, "", "gh: Bad Gateway (HTTP 502)\n"),
        (false, "", "gh: Service Unavailable (HTTP 503)\n"),
        (false, "", TIMED_OUT),
        (false, "", "gh: Server Error (HTTP 500)\n"),
        (true, &one_release(), ""),
    ]);
    assert_eq!(
        api.releases(&repo()),
        Err(failure(
            &format!("gh api {}", releases_path()),
            "gh: Server Error (HTTP 500) (the last of 4 attempts)"
        ))
    );
    assert_eq!(api.exec.calls.borrow().len(), READ_ATTEMPTS);
    assert_eq!(waited(), secs(&[10, 30, 60]));
}

#[test]
fn a_read_is_asked_once_for_any_other_failure() {
    // Catches: a refusal that asking again cannot change (not found, forbidden, bad
    // credentials, a 5xx code that is not a server error of GitHub's) asked again, a server
    // error named in the middle of the text read as GitHub's answer, a reply that is not JSON
    // asked again, or a workflow that is not on main no longer read as no runs.
    for stderr in [
        "gh: Not Found (HTTP 404)\n",
        "gh: Forbidden (HTTP 403)\n",
        "gh: Bad credentials (HTTP 401)\n",
        "gh: HTTP Version Not Supported (HTTP 505)\n",
        "gh: the proxy said (HTTP 504) and closed\n",
        "HTTP 504\n",
    ] {
        let api = in_turn(&[(false, "", stderr), (true, &one_release(), "")]);
        assert_eq!(
            api.releases(&repo()),
            Err(failure(
                &format!("gh api {}", releases_path()),
                stderr.trim()
            ))
        );
        assert_eq!(api.exec.calls.borrow().len(), 1, "{stderr}");
        assert_eq!(waited(), Vec::<Duration>::new(), "{stderr}");
    }
    let api = in_turn(&[(true, "<html>", ""), (true, &one_release(), "")]);
    assert!(api.releases(&repo()).is_err());
    assert_eq!(api.exec.calls.borrow().len(), 1);
    let api = in_turn(&[(false, "", "gh: Not Found (HTTP 404)\n")]);
    assert_eq!(
        api.runs(&repo(), "privileged.yml", &commit()),
        Ok(Vec::new())
    );
    assert_eq!(api.exec.calls.borrow().len(), 1);
    assert_eq!(waited(), Vec::<Duration>::new());
}

/// A write of the `gh` it is given: whether it failed.
type Write = fn(&Gh<InTurn>) -> bool;

#[test]
fn a_write_that_times_out_is_asked_once() {
    // Catches: a write asked again after a gateway timeout, which may have done it already: a
    // second draft or issue, a second upload of an asset, a second comment.
    let writes: [(&str, Write); 6] = [
        ("create_draft", |api| {
            api.create_draft(&repo(), "v0.1.446", &commit(), "notes")
                .is_err()
        }),
        ("upload", |api| {
            api.upload(&repo(), "v0.1.446", Path::new("/x/out/SHA256SUMS"))
                .is_err()
        }),
        ("publish", |api| api.publish(&repo(), "v0.1.446").is_err()),
        ("open_issue", |api| {
            api.open_issue(&repo(), "t", "b").is_err()
        }),
        ("edit_issue", |api| api.edit_issue(&repo(), 7, "b").is_err()),
        ("close_issue", |api| {
            api.close_issue(&repo(), 7, "c").is_err()
        }),
    ];
    for (name, write) in writes {
        let api = in_turn(&[(false, "", TIMED_OUT), (true, "{\"number\":1}", "")]);
        assert!(write(&api), "{name}");
        assert_eq!(api.exec.calls.borrow().len(), 1, "{name}");
        assert_eq!(waited(), Vec::<Duration>::new(), "{name}");
    }
}
