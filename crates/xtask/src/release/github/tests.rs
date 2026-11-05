use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use serde_json::{Value, json};

use super::*;
use crate::release::fakes::{FakeExec, nth_commit};

const REPO_NAME: &str = "komira-ai/hostkeeper";
const SIGNER_URI: &str =
    "https://github.com/komira-ai/hostkeeper/.github/workflows/artifacts.yml@refs/heads/main";

fn repo() -> Repo {
    REPO_NAME.parse().expect("a repository")
}

fn commit() -> Commit {
    nth_commit(1)
}

fn digest() -> String {
    "ab12cd34".repeat(8)
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn failure(what: &str, why: &str) -> PortError {
    PortError::new(what, why)
}

/// A `gh` that answers the command line `line` with `stdout`.
fn gh(line: &[&str], stdout: &str) -> Gh<FakeExec> {
    Gh::new(FakeExec::default().on(line, true, stdout, ""))
}

/// A `gh` whose command line `line` ends unsuccessfully, printing `stderr`.
fn failing(line: &[&str], stderr: &str) -> Gh<FakeExec> {
    Gh::new(FakeExec::default().on(line, false, "", stderr))
}

fn calls(gh: &Gh<FakeExec>) -> Vec<Vec<String>> {
    gh.exec.calls.borrow().clone()
}

fn runs_path(workflow: &str) -> String {
    format!(
        "repos/{REPO_NAME}/actions/workflows/{workflow}/runs?head_sha={}&per_page=100",
        commit()
    )
}

fn run_json() -> Value {
    json!({
        "id": 11,
        "event": "push",
        "head_branch": "main",
        "head_sha": commit().to_string(),
        "status": "completed",
        "conclusion": "success"
    })
}

/// `value` without the entry the JSON pointer `pointer` names.
fn without(mut value: Value, pointer: &str) -> Value {
    let (parent, key) = pointer.rsplit_once('/').expect("a pointer with a key");
    value
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("an object to remove from")
        .remove(key);
    value
}

#[test]
fn runs_lists_the_runs_of_a_workflow_for_a_commit() {
    // Catches: a wrong workflow or head filter in the request, a pull-request run's null branch
    // failing the parse, or a pending run's null conclusion read as a value.
    let path = runs_path("ci.yml");
    let pending = json!({
        "id": 12,
        "event": "pull_request",
        "head_branch": null,
        "head_sha": commit().to_string(),
        "status": "in_progress",
        "conclusion": null
    });
    let reply = json!({ "total_count": 2, "workflow_runs": [run_json(), pending] });
    let api = gh(&["gh", "api", &path], &reply.to_string());
    assert_eq!(
        api.runs(&repo(), "ci.yml", &commit()),
        Ok(vec![
            Run {
                id: 11,
                event: "push".to_owned(),
                branch: "main".to_owned(),
                head_sha: commit().to_string(),
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
            },
            Run {
                id: 12,
                event: "pull_request".to_owned(),
                branch: String::new(),
                head_sha: commit().to_string(),
                status: "in_progress".to_owned(),
                conclusion: None,
            },
        ])
    );
    assert_eq!(calls(&api), vec![words(&["gh", "api", &path])]);
}

#[test]
fn runs_refuses_a_run_that_lacks_a_field() {
    // Catches: a run without its id, event, head commit or status counted as a run of nothing.
    let path = runs_path("ci.yml");
    let what = format!("gh api {path}");
    for (key, wanted) in [
        ("id", "a number"),
        ("event", "a string"),
        ("head_sha", "a string"),
        ("status", "a string"),
    ] {
        let run = without(run_json(), &format!("/{key}"));
        let reply = json!({ "workflow_runs": [run] }).to_string();
        let api = gh(&["gh", "api", &path], &reply);
        assert_eq!(
            api.runs(&repo(), "ci.yml", &commit()),
            Err(failure(&what, &format!("{key} is missing or not {wanted}")))
        );
    }
}

#[test]
fn runs_refuses_a_reply_without_the_run_list() {
    // Catches: a reply of another shape read as no runs, which would wait forever or release.
    let path = runs_path("ci.yml");
    let api = gh(&["gh", "api", &path], "{}");
    assert_eq!(
        api.runs(&repo(), "ci.yml", &commit()),
        Err(failure(
            &format!("gh api {path}"),
            "workflow_runs is missing or not a list"
        ))
    );
}

#[test]
fn a_reply_that_is_not_json_is_an_error() {
    // Catches: unparsable output read as an empty answer.
    let path = runs_path("ci.yml");
    let api = gh(&["gh", "api", &path], "");
    assert_eq!(
        api.runs(&repo(), "ci.yml", &commit()),
        Err(failure(
            &format!("gh api {path}"),
            "not JSON: EOF while parsing a value at line 1 column 0"
        ))
    );
}

#[test]
fn a_failing_gh_reports_its_trimmed_stderr() {
    // Catches: a failed request whose stdout is parsed anyway, or whose reason is dropped.
    let path = runs_path("ci.yml");
    let api = failing(&["gh", "api", &path], "gh: HTTP 404\n");
    assert_eq!(
        api.runs(&repo(), "ci.yml", &commit()),
        Err(failure(&format!("gh api {path}"), "gh: HTTP 404"))
    );
}

#[test]
fn a_gh_that_cannot_start_names_the_command_line() {
    // Catches: a spawn failure reported without the command that was asked for.
    let api = Gh::new(FakeExec::default());
    let path = runs_path("privileged.yml");
    assert_eq!(
        api.runs(&repo(), "privileged.yml", &commit()),
        Err(failure(
            &format!("gh api {path}"),
            &format!("not scripted: gh api {path}")
        ))
    );
}

#[test]
fn on_main_is_true_for_a_commit_main_equals_or_is_ahead_of() {
    // Catches: a commit ahead of or diverged from main released, or one behind main refused.
    let path = format!("repos/{REPO_NAME}/compare/main...{}", commit());
    for (status, expected) in [
        ("identical", true),
        ("behind", true),
        ("ahead", false),
        ("diverged", false),
    ] {
        let reply = json!({ "status": status }).to_string();
        let api = gh(&["gh", "api", &path], &reply);
        assert_eq!(api.on_main(&repo(), &commit()), Ok(expected));
        assert_eq!(calls(&api), vec![words(&["gh", "api", &path])]);
    }
}

#[test]
fn on_main_refuses_a_reply_without_a_status() {
    // Catches: a reply of another shape (a missing commit) read as not on main without a reason.
    let path = format!("repos/{REPO_NAME}/compare/main...{}", commit());
    let api = gh(&["gh", "api", &path], r#"{"message":"Not Found"}"#);
    assert_eq!(
        api.on_main(&repo(), &commit()),
        Err(failure(
            &format!("gh api {path}"),
            "status is missing or not a string"
        ))
    );
}

#[test]
fn open_issues_skips_pull_requests_and_reads_a_null_body_as_empty() {
    // Catches: a pull request taken for an issue (GitHub lists both), or a null body failing.
    let path = format!("repos/{REPO_NAME}/issues?state=open&per_page=100");
    let reply = json!([
        { "number": 3, "title": "A pull request", "body": "x", "pull_request": {} },
        { "number": 4, "title": "No body", "body": null },
        { "number": 5, "title": "With body", "body": "text" }
    ]);
    let api = gh(&["gh", "api", &path], &reply.to_string());
    assert_eq!(
        api.open_issues(&repo()),
        Ok(vec![
            Issue {
                number: 4,
                title: "No body".to_owned(),
                body: String::new()
            },
            Issue {
                number: 5,
                title: "With body".to_owned(),
                body: "text".to_owned()
            },
        ])
    );
    assert_eq!(calls(&api), vec![words(&["gh", "api", &path])]);
}

#[test]
fn open_issues_refuses_an_issue_without_a_title_and_a_reply_that_is_no_list() {
    // Catches: an issue without its title matched against the tracking title as empty, and an
    // error object read as no open issues, which would open a second tracking issue.
    let path = format!("repos/{REPO_NAME}/issues?state=open&per_page=100");
    let what = format!("gh api {path}");
    let titleless = gh(&["gh", "api", &path], r#"[{"number":4}]"#);
    assert_eq!(
        titleless.open_issues(&repo()),
        Err(failure(&what, "title is missing or not a string"))
    );
    let numberless = gh(&["gh", "api", &path], r#"[{"title":"x"}]"#);
    assert_eq!(
        numberless.open_issues(&repo()),
        Err(failure(&what, "number is missing or not a number"))
    );
    let object = gh(&["gh", "api", &path], r#"{"message":"Bad"}"#);
    assert_eq!(
        object.open_issues(&repo()),
        Err(failure(&what, "the reply is missing or not a list"))
    );
}

#[test]
fn open_issue_posts_the_title_and_body_as_single_arguments() {
    // Catches: a title or body with spaces split into several arguments, or the wrong method.
    let path = format!("repos/{REPO_NAME}/issues");
    let line = [
        "gh",
        "api",
        "--method",
        "POST",
        path.as_str(),
        "-f",
        "title=Releases are held",
        "-f",
        "body=Two words\nand a line",
    ];
    let api = gh(&line, r#"{"number":7,"title":"Releases are held"}"#);
    assert_eq!(
        api.open_issue(&repo(), "Releases are held", "Two words\nand a line"),
        Ok(7)
    );
    assert_eq!(calls(&api), vec![words(&line)]);
}

#[test]
fn open_issue_refuses_a_reply_without_a_number() {
    // Catches: a created issue whose number was not read, so it could never be edited.
    let path = format!("repos/{REPO_NAME}/issues");
    let line = [
        "gh",
        "api",
        "--method",
        "POST",
        path.as_str(),
        "-f",
        "title=T",
        "-f",
        "body=B",
    ];
    let api = gh(&line, "{}");
    assert_eq!(
        api.open_issue(&repo(), "T", "B"),
        Err(failure(
            &format!("gh api --method POST {path} -f title=T -f body=B"),
            "number is missing or not a number"
        ))
    );
}

#[test]
fn edit_issue_patches_the_body_of_the_numbered_issue() {
    // Catches: the wrong issue number or method, or a reply read as required.
    let path = format!("repos/{REPO_NAME}/issues/7");
    let line = [
        "gh",
        "api",
        "--method",
        "PATCH",
        path.as_str(),
        "-f",
        "body=New text",
    ];
    let api = gh(&line, "");
    assert_eq!(api.edit_issue(&repo(), 7, "New text"), Ok(()));
    assert_eq!(calls(&api), vec![words(&line)]);
    let failed = failing(&line, "gh: HTTP 403\n");
    assert_eq!(
        failed.edit_issue(&repo(), 7, "New text"),
        Err(failure(
            &format!("gh api --method PATCH {path} -f body=New text"),
            "gh: HTTP 403"
        ))
    );
}

fn release_json() -> Value {
    json!({
        "tag_name": "v0.1.5",
        "target_commitish": commit().to_string(),
        "draft": true,
        "assets": [
            { "name": "SHA256SUMS", "digest": format!("sha256:{}", digest()) },
            { "name": "no-digest", "digest": null },
            { "name": "other-digest", "digest": "md5:abc" }
        ]
    })
}

#[test]
fn releases_reads_tags_commits_drafts_and_asset_digests() {
    // Catches: a digest kept with its algorithm prefix, a missing digest read as a value, or a
    // draft read as published.
    let path = format!("repos/{REPO_NAME}/releases?per_page=100");
    let published = json!({
        "tag_name": "v0.1.4",
        "target_commitish": "main",
        "draft": false,
        "assets": []
    });
    let reply = json!([release_json(), published]);
    let api = gh(&["gh", "api", &path], &reply.to_string());
    assert_eq!(
        api.releases(&repo()),
        Ok(vec![
            Release {
                tag: "v0.1.5".to_owned(),
                commit: commit().to_string(),
                draft: true,
                assets: vec![
                    StoredAsset {
                        name: "SHA256SUMS".to_owned(),
                        sha256: Some(digest())
                    },
                    StoredAsset {
                        name: "no-digest".to_owned(),
                        sha256: None
                    },
                    StoredAsset {
                        name: "other-digest".to_owned(),
                        sha256: None
                    },
                ],
            },
            Release {
                tag: "v0.1.4".to_owned(),
                commit: "main".to_owned(),
                draft: false,
                assets: Vec::new(),
            },
        ])
    );
    assert_eq!(calls(&api), vec![words(&["gh", "api", &path])]);
}

#[test]
fn releases_refuses_an_entry_that_lacks_a_field() {
    // Catches: a release without its tag, commit, draft flag or assets read as an empty one.
    let path = format!("repos/{REPO_NAME}/releases?per_page=100");
    let what = format!("gh api {path}");
    for (pointer, why) in [
        ("/0/tag_name", "tag_name is missing or not a string"),
        (
            "/0/target_commitish",
            "target_commitish is missing or not a string",
        ),
        ("/0/draft", "draft is missing or not true or false"),
        ("/0/assets", "assets is missing or not a list"),
        ("/0/assets/0/name", "name is missing or not a string"),
    ] {
        let reply = without(json!([release_json()]), pointer);
        let api = gh(&["gh", "api", &path], &reply.to_string());
        assert_eq!(api.releases(&repo()), Err(failure(&what, why)));
    }
    let object = gh(&["gh", "api", &path], "{}");
    assert_eq!(
        object.releases(&repo()),
        Err(failure(&what, "the reply is missing or not a list"))
    );
}

#[test]
fn create_draft_names_the_tag_the_commit_and_the_notes() {
    // Catches: a release created at the default branch tip, published at once, or with the
    // notes split into several arguments.
    let sha = commit().to_string();
    let line = [
        "gh",
        "release",
        "create",
        "v0.1.5",
        "--repo",
        REPO_NAME,
        "--target",
        sha.as_str(),
        "--title",
        "v0.1.5",
        "--notes",
        "Built from a commit.\nSecond line.",
        "--draft",
    ];
    let api = gh(
        &line,
        "https://github.com/komira-ai/hostkeeper/releases/tag/v0.1.5\n",
    );
    assert_eq!(
        api.create_draft(
            &repo(),
            "v0.1.5",
            &commit(),
            "Built from a commit.\nSecond line."
        ),
        Ok(())
    );
    assert_eq!(calls(&api), vec![words(&line)]);
    let failed = failing(&line, "HTTP 422\n");
    assert_eq!(
        failed.create_draft(
            &repo(),
            "v0.1.5",
            &commit(),
            "Built from a commit.\nSecond line."
        ),
        Err(failure(&line.join(" "), "HTTP 422"))
    );
}

#[test]
fn upload_names_the_tag_and_the_file() {
    // Catches: an upload to the wrong release, or one that replaces an asset (no clobber flag).
    let line = [
        "gh",
        "release",
        "upload",
        "v0.1.5",
        "/x/out/SHA256SUMS",
        "--repo",
        REPO_NAME,
    ];
    let api = gh(&line, "");
    assert_eq!(
        api.upload(&repo(), "v0.1.5", Path::new("/x/out/SHA256SUMS")),
        Ok(())
    );
    assert_eq!(calls(&api), vec![words(&line)]);
    let failed = failing(&line, "HTTP 500\n");
    assert_eq!(
        failed.upload(&repo(), "v0.1.5", Path::new("/x/out/SHA256SUMS")),
        Err(failure(&line.join(" "), "HTTP 500"))
    );
}

#[test]
fn upload_refuses_a_path_that_is_not_utf8() {
    // Catches: a path converted lossily, so another file than the checked one is uploaded.
    let api = Gh::new(FakeExec::default());
    let path = Path::new(OsStr::from_bytes(b"/x/a\xffb"));
    assert_eq!(
        api.upload(&repo(), "v0.1.5", path),
        Err(failure(
            "gh release upload",
            "the path /x/a\u{fffd}b is not UTF-8"
        ))
    );
    assert_eq!(calls(&api), Vec::<Vec<String>>::new());
}

#[test]
fn publish_turns_the_draft_into_a_published_release() {
    // Catches: a draft left unpublished, or the wrong release edited.
    let line = [
        "gh",
        "release",
        "edit",
        "v0.1.5",
        "--repo",
        REPO_NAME,
        "--draft=false",
    ];
    let api = gh(&line, "");
    assert_eq!(api.publish(&repo(), "v0.1.5"), Ok(()));
    assert_eq!(calls(&api), vec![words(&line)]);
    let failed = failing(&line, "HTTP 404\n");
    assert_eq!(
        failed.publish(&repo(), "v0.1.5"),
        Err(failure(&line.join(" "), "HTTP 404"))
    );
}

fn verify_line(file: &str, bundle: Option<&str>) -> Vec<String> {
    let mut line = words(&[
        "gh",
        "attestation",
        "verify",
        file,
        "--repo",
        REPO,
        "--signer-workflow",
        SIGNER_WORKFLOW,
        "--source-ref",
        SIGNER_REF,
        "--deny-self-hosted-runners",
        "--format",
        "json",
    ]);
    if let Some(bundle) = bundle {
        line.push("--bundle".to_owned());
        line.push(bundle.to_owned());
    }
    line
}

/// A `gh` that answers the command line `line` with `stdout`.
fn scripted(line: &[String], stdout: &str) -> Gh<FakeExec> {
    let refs: Vec<&str> = line.iter().map(String::as_str).collect();
    gh(&refs, stdout)
}

fn element(uri: &str) -> Value {
    json!({
        "verificationResult": {
            "signature": {
                "certificate": {
                    "buildSignerURI": uri,
                    "sourceRepositoryRef": SIGNER_REF,
                    "sourceRepositoryDigest": commit().to_string()
                }
            },
            "statement": { "subject": [{ "digest": { "sha256": digest() } }] }
        }
    })
}

fn attestation() -> Attestation {
    Attestation {
        signer_workflow: SIGNER_WORKFLOW.to_owned(),
        source_ref: SIGNER_REF.to_owned(),
        source_commit: commit().to_string(),
        subjects: vec![digest()],
    }
}

#[test]
fn verify_online_pins_the_repository_signer_and_ref() {
    // Catches: a verification that accepts any signer, any ref, or a self-hosted runner.
    let line = verify_line("/x/asset", None);
    let reply = json!([element(SIGNER_URI)]).to_string();
    let api = scripted(&line, &reply);
    assert_eq!(
        api.verify(Path::new("/x/asset"), None),
        Ok(vec![attestation()])
    );
    assert_eq!(calls(&api), vec![line]);
}

#[test]
fn verify_offline_adds_the_bundle() {
    // Catches: an offline check that ignores its bundle and fetches the attestations instead.
    let line = verify_line("/x/asset", Some("/x/asset.sigstore.json"));
    let reply = json!([element(SIGNER_URI), element(SIGNER_URI)]).to_string();
    let api = scripted(&line, &reply);
    assert_eq!(
        api.verify(
            Path::new("/x/asset"),
            Some(Path::new("/x/asset.sigstore.json"))
        ),
        Ok(vec![attestation(), attestation()])
    );
    assert_eq!(calls(&api), vec![line]);
}

#[test]
fn verify_with_no_attestation_in_the_reply_is_an_empty_list() {
    // Catches: an empty reply turned into an error here, hiding the caller's own wording.
    let line = verify_line("/x/asset", None);
    let api = scripted(&line, "[]");
    assert_eq!(api.verify(Path::new("/x/asset"), None), Ok(Vec::new()));
}

#[test]
fn verify_reports_a_failed_verification_with_gh_stderr() {
    // Catches: a file whose attestation did not verify read as attested.
    let line = verify_line("/x/asset", None);
    let refs: Vec<&str> = line.iter().map(String::as_str).collect();
    let api = failing(&refs, "Error: verification failed\n");
    assert_eq!(
        api.verify(Path::new("/x/asset"), None),
        Err(failure(&line.join(" "), "Error: verification failed"))
    );
}

#[test]
fn verify_refuses_a_malformed_signer_uri() {
    // Catches: a signer URI of another shape read as a signer, or its workflow cut wrongly.
    let line = verify_line("/x/asset", None);
    for uri in [
        "https://example.com/owner/repo/wf@refs/heads/main",
        "https://github.com/owner/repo/wf",
        "",
    ] {
        let reply = json!([element(uri)]).to_string();
        let api = scripted(&line, &reply);
        assert_eq!(
            api.verify(Path::new("/x/asset"), None),
            Err(failure(
                &line.join(" "),
                &format!("buildSignerURI {uri:?} is not https://github.com/<workflow>@<ref>")
            ))
        );
    }
}

#[test]
fn verify_refuses_an_element_that_lacks_a_field() {
    // Catches: a missing signer, ref, commit or subject read as an empty value that then could
    // not be told from a mismatch.
    let line = verify_line("/x/asset", None);
    let certificate = "/0/verificationResult/signature/certificate";
    let statement = "/0/verificationResult/statement";
    for (pointer, why) in [
        (
            format!("{certificate}/buildSignerURI"),
            "buildSignerURI is missing or not a string",
        ),
        (
            format!("{certificate}/sourceRepositoryRef"),
            "sourceRepositoryRef is missing or not a string",
        ),
        (
            format!("{certificate}/sourceRepositoryDigest"),
            "sourceRepositoryDigest is missing or not a string",
        ),
        (
            format!("{statement}/subject"),
            "subject is missing or not a list",
        ),
        (
            format!("{statement}/subject/0/digest"),
            "subject sha256 is missing or not a string",
        ),
    ] {
        let reply = without(json!([element(SIGNER_URI)]), &pointer).to_string();
        let api = scripted(&line, &reply);
        assert_eq!(
            api.verify(Path::new("/x/asset"), None),
            Err(failure(&line.join(" "), why))
        );
    }
    let object = scripted(&line, "{}");
    assert_eq!(
        object.verify(Path::new("/x/asset"), None),
        Err(failure(
            &line.join(" "),
            "the reply is missing or not a list"
        ))
    );
}

#[test]
fn verify_refuses_a_file_or_bundle_path_that_is_not_utf8() {
    // Catches: a path converted lossily, so another file than the named one is verified.
    let api = Gh::new(FakeExec::default());
    let bad = Path::new(OsStr::from_bytes(b"/x/a\xffb"));
    let expected = failure(
        "gh attestation verify",
        "the path /x/a\u{fffd}b is not UTF-8",
    );
    assert_eq!(api.verify(bad, None), Err(expected.clone()));
    assert_eq!(api.verify(Path::new("/x/asset"), Some(bad)), Err(expected));
    assert_eq!(calls(&api), Vec::<Vec<String>>::new());
}
