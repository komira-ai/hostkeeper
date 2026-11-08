//! The trust list: a service of another repository staged by its own release workflow, under a
//! name that is not its repository's; and each way a service or a signer is refused.

use super::*;

const JOBSCHEDULER: &str = "komira-ai/jobscheduler";
const RELEASE_YML: &str = ".github/workflows/release.yml";
const AGENT_SIGNER: &str = "komira-ai/jobscheduler/.github/workflows/release.yml";
const AGENT_V: &str = "0.1.176";
const MAIN: &str = "refs/heads/main";

/// The trust list of tierstore (its `artifacts.yml`) and js-agent (jobscheduler's `release.yml`
/// on `refs`).
fn tierstore_and_agent(refs: &[&str]) -> ChannelTrust {
    trust(&[
        entry(
            "tierstore",
            TIERSTORE,
            ".github/workflows/artifacts.yml",
            &[MAIN],
        ),
        entry("js-agent", JOBSCHEDULER, RELEASE_YML, refs),
    ])
}

/// The trust list of js-agent alone, from jobscheduler's `release.yml` on `refs`.
fn agent_only(refs: &[&str]) -> ChannelTrust {
    trust(&[entry("js-agent", JOBSCHEDULER, RELEASE_YML, refs)])
}

fn request_with(c: &Channel, trust: ChannelTrust) -> Request {
    Request {
        trust,
        channel: c.dir.clone(),
        since: None,
    }
}

/// A verifier that attests every file as signed by `workflow` on `source_ref`, at the commit it
/// is asked about, whoever it is asked for: what `gh` answers when it reads the bundle's
/// certificate.
fn signed_by(workflow: &'static str, source_ref: &'static str) -> FakeVerifier {
    FakeVerifier::with(Box::new(move |file, _| {
        Ok(vec![Attestation {
            signer_workflow: workflow.to_owned(),
            source_ref: source_ref.to_owned(),
            source_commit: nth_commit(2).to_string(),
            subjects: vec![file_sha256(file).expect("hash")],
        }])
    }))
}

#[test]
fn a_second_repositorys_service_signed_by_its_own_release_workflow_is_staged_under_its_name() {
    // Catches: a service staged only when its repository is hostkeeper or is named after it, or a
    // repository's packages verified against `artifacts.yml` or another repository's workflow
    // rather than the signer its entry names.
    let c = channel("sync-trust-agent");
    let source = FakeSource::default();
    let ts = release_of("tierstore", TS_V, "tierstore");
    let agent = release_of("js-agent", AGENT_V, "agent");
    publish(&source, TIERSTORE, "v0.3.53", 1, &ts);
    publish(&source, JOBSCHEDULER, "v0.1.176", 2, &agent);
    let verifier = FakeVerifier::attests_as_asked();

    let lines = sync(
        &source,
        &verifier,
        &request_with(&c, tierstore_and_agent(&[MAIN])),
    )
    .expect("synced");

    assert_eq!(
        lines,
        [
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
            format!("staged {JOBSCHEDULER} v0.1.176: js-agent {AGENT_V}"),
            "channel-sync: 2 versions staged, 0 already in the channel, 0 refused".to_owned(),
        ]
    );
    let mut want = staged("tierstore", TS_V, &ts, "index.json");
    want.extend(staged("js-agent", AGENT_V, &agent, "index.json"));
    assert_eq!(snapshot(&c.dir), want);
    // Each package of each version, online and from its bundle, by its own entry's signer.
    let signers = verifier.signers.borrow();
    assert_eq!(
        *signers,
        [
            vec![TIERSTORE_SIGNER.to_owned(); 6],
            vec![AGENT_SIGNER.to_owned(); 6]
        ]
        .concat()
    );
    let calls = verifier.calls.borrow();
    let agent_calls: Vec<&String> = calls[6..].iter().collect();
    let mut want_calls = Vec::new();
    for platform in Platform::ALL {
        let name = format!("js-agent-{AGENT_V}-{platform}.tar.zst");
        want_calls.push(format!("online {name}"));
        want_calls.push(format!("offline {name} {name}.sigstore.json"));
    }
    assert_eq!(agent_calls, want_calls.iter().collect::<Vec<_>>());
}

#[test]
fn the_same_package_signed_by_an_unlisted_workflow_is_refused_naming_the_service_and_signer() {
    // Catches: a package staged when its attestation is by another workflow of the right
    // repository (one the trust list does not name), or a refusal that does not say which
    // service and which signer it wanted.
    let c = channel("sync-trust-unlisted");
    let source = FakeSource::default();
    let agent = release_of("js-agent", AGENT_V, "agent");
    publish(&source, JOBSCHEDULER, "v0.1.176", 2, &agent);
    let ci = "komira-ai/jobscheduler/.github/workflows/ci.yml";

    let lines = refused_lines(sync(
        &source,
        &signed_by(ci, MAIN),
        &request_with(&c, agent_only(&[MAIN])),
    ));

    let mut problems = Vec::new();
    for platform in Platform::ALL {
        for form in ["online", "offline"] {
            problems.push(format!(
                "js-agent-{AGENT_V}-{platform}.tar.zst ({form}): signed by {ci}, not {AGENT_SIGNER}"
            ));
        }
    }
    assert_eq!(
        lines,
        [
            format!(
                "refused {JOBSCHEDULER} v0.1.176: js-agent {AGENT_V}: not signed as the trust \
                 list requires of js-agent, by {AGENT_SIGNER} on {MAIN}: {}",
                problems.join("; ")
            ),
            "channel-sync: 0 versions staged, 0 already in the channel, 1 refused".to_owned(),
        ]
    );
    assert!(snapshot(&c.dir).is_empty(), "{:?}", snapshot(&c.dir));
}

#[test]
fn a_package_signed_on_a_ref_no_glob_matches_is_refused_and_one_a_glob_matches_is_staged() {
    // Catches: refs compared as text rather than as the daemon's globs, so a tag-signed release
    // never stages, or a branch run passing for a tag run.
    for (source_ref, staged_it) in [("refs/tags/v0.1.176", true), ("refs/heads/dev", false)] {
        let c = channel("sync-trust-glob");
        let source = FakeSource::default();
        let agent = release_of("js-agent", AGENT_V, "agent");
        publish(&source, JOBSCHEDULER, "v0.1.176", 2, &agent);
        let trust = agent_only(&["refs/tags/v*"]);

        let result = sync(
            &source,
            &signed_by(AGENT_SIGNER, source_ref),
            &request_with(&c, trust),
        );

        if staged_it {
            let lines = result.expect("synced");
            assert_eq!(
                lines[0],
                format!("staged {JOBSCHEDULER} v0.1.176: js-agent {AGENT_V}")
            );
            assert_eq!(
                snapshot(&c.dir),
                staged("js-agent", AGENT_V, &agent, "index.json")
            );
        } else {
            let lines = refused_lines(result);
            let one = format!(
                "js-agent-{AGENT_V}-linux-arm64.tar.zst (online): attests the ref {source_ref}, \
                 not refs/tags/v*"
            );
            assert!(lines[0].contains(&one), "{lines:?}");
            assert!(snapshot(&c.dir).is_empty());
        }
    }
}

#[test]
fn a_service_the_list_trusts_from_another_repository_is_refused_naming_that_repository() {
    // Catches: a service listed for one repository staged from another's release, which could
    // then claim its versions in the channel first.
    let c = channel("sync-trust-other-repo");
    let source = FakeSource::default();
    let agent = release_of("js-agent", AGENT_V, "agent");
    let mut ts = release_of("tierstore", TS_V, "tierstore");
    ts.extend(
        agent
            .iter()
            .map(|(n, b)| (n.replace("index.json", "js-agent-index.json"), b.clone())),
    );
    resum(&mut ts);
    publish(&source, TIERSTORE, "v0.3.53", 1, &ts);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request_with(&c, tierstore_and_agent(&[MAIN])),
    ));

    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: js-agent {AGENT_V}: js-agent-index.json is the index of \
             js-agent, which the trust list trusts from {JOBSCHEDULER} only"
        )
    );
    assert_eq!(
        lines[1],
        format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}")
    );
    assert!(!c.dir.join("js-agent").exists());
}

#[test]
fn a_service_named_for_its_repository_but_not_listed_is_refused() {
    // Catches: the old rule (a repository ships the service named after it) still trusting a
    // service the list leaves out.
    let c = channel("sync-trust-repo-name");
    let source = FakeSource::default();
    let named = release_of("jobscheduler", AGENT_V, "named");
    publish(&source, JOBSCHEDULER, "v0.1.176", 2, &named);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request_with(&c, agent_only(&[MAIN])),
    ));

    assert_eq!(
        lines[0],
        format!(
            "refused {JOBSCHEDULER} v0.1.176: jobscheduler {AGENT_V}: index.json is the index of \
             jobscheduler, a service the trust list does not list"
        )
    );
    assert_eq!(source.downloads.borrow().len(), 1, "only the index");
    assert!(snapshot(&c.dir).is_empty());
}

#[test]
fn a_hostkeeper_release_is_checked_by_release_check_and_each_service_by_its_entrys_signer() {
    // Catches: a hostkeeper release staged on its contract check alone, so an entry naming
    // another signer for hello is never applied, or hostkeeper's own check skipped.
    let c = channel("sync-trust-hostkeeper");
    let source = FakeSource::default();
    let version: Version = "1.4.0".parse().expect("a version");
    let assets = hostkeeper_release(&version);
    publish(&source, REPO, "v1.4.0", 1, &assets);
    let hello_by = "komira-ai/hostkeeper/.github/workflows/hello.yml";
    let trust = trust(&[
        entry(
            "hostkeeper",
            REPO,
            ".github/workflows/artifacts.yml",
            &[MAIN],
        ),
        entry("hello", REPO, ".github/workflows/hello.yml", &[MAIN]),
        entry("mdm", REPO, ".github/workflows/artifacts.yml", &[MAIN]),
    ]);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::good(&nth_commit(1)),
        &request_with(&c, trust),
    ));

    // The indexes in the order of their asset names: hello's failing build, hello, hostkeeper,
    // mdm.
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(lines[2], format!("staged {REPO} v1.4.0: hostkeeper 1.4.0"));
    assert_eq!(lines[3], format!("staged {REPO} v1.4.0: mdm 1.4.0"));
    let hello_versions = ["1.4.0-fail-ready", "1.4.0"];
    for (line, v) in lines[..2].iter().zip(hello_versions) {
        let start = format!(
            "refused {REPO} v1.4.0: hello {v}: not signed as the trust list requires of hello, \
             by {hello_by} on {MAIN}: hello-{v}-linux-arm64.tar.zst (online): signed by \
             {SIGNER_WORKFLOW}, not {hello_by}; "
        );
        assert!(line.starts_with(&start), "{line}");
    }
    assert!(c.dir.join("hostkeeper/1.4.0/index.json").is_file());
    assert!(!c.dir.join("hello").exists());
}

#[test]
fn an_index_that_does_not_parse_is_refused_before_any_attestation() {
    // Catches: a version whose index is not the daemon's verified package by package from a list
    // read some other way.
    let c = channel("sync-trust-bad-index");
    let source = FakeSource::default();
    let mut agent = release_of("js-agent", AGENT_V, "agent");
    let index = String::from_utf8(agent["index.json"].clone()).expect("UTF-8");
    agent.insert(
        "index.json".to_owned(),
        index.replace("\"schema\":1", "\"schema\":2").into_bytes(),
    );
    resum(&mut agent);
    publish(&source, JOBSCHEDULER, "v0.1.176", 2, &agent);
    let verifier = FakeVerifier::attests_as_asked();

    let lines = refused_lines(sync(
        &source,
        &verifier,
        &request_with(&c, agent_only(&[MAIN])),
    ));

    assert!(
        lines[0].starts_with(&format!(
            "refused {JOBSCHEDULER} v0.1.176: js-agent {AGENT_V}: index.json: "
        )),
        "{lines:?}"
    );
    assert!(verifier.calls.borrow().is_empty());
    assert!(snapshot(&c.dir).is_empty());
}
