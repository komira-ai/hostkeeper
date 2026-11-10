//! What a set with targets keeps: a repeat, `release_exists`, the audit, `GetRelease`, a restart,
//! and the previous coordinator on the new state.
use std::collections::BTreeMap;
use std::path::Path;

use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_oidc::testkit::WORKFLOW;
use hostkeeper_proto::hostkeeper::release::v1::TargetList;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Version};
use prost::Message;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::readers::Readers;
use support::rig::PATIENCE;
use support::targets::{Answered, entry, framed, list, repository_entry};

use super::*;

const AUDIT_LIST: &str = "/hostkeeper.operator.v1.Audit/List";

/// A status code of gRPC.
const NOT_FOUND: i32 = 5;
const INVALID_ARGUMENT: i32 = 3;
const FAILED_PRECONDITION: i32 = 9;
const UNAUTHENTICATED: i32 = 16;

/// Calls `path` over gRPC with `message`, reading the status from the trailers or, for a refusal
/// the router answers with no body, from the headers.
async fn call<M: Message>(rig: &Rig, path: &str, bearer: Option<&str>, message: &M) -> Answered {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(framed(&message.encode_to_vec())))
        .expect("a request");
    let response = route(&rig.coordinator, Surface::Control, &None, request, PATIENCE).await;
    let headers = response.headers().clone();
    let collected = response.into_body().collect().await.expect("a body");
    let trailers = collected.trailers().cloned().unwrap_or_default();
    let data = collected.to_bytes();
    let field = |name: &str| {
        trailers
            .get(name)
            .or_else(|| headers.get(name))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let code = field("grpc-status")
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let message = field("grpc-message").unwrap_or_default();
    let body = (code == 0 && data.len() >= 5).then(|| data[5..].to_vec());
    Answered {
        code,
        message,
        body,
    }
}

type Targets = BTreeMap<String, TargetList>;

/// CI's set.
async fn ci_set(rig: &Rig, request: &proto::SetDesiredRequest) -> Answered {
    call(rig, SET_DESIRED, Some(&ci_token()), request).await
}

/// The operator's set.
async fn operator_set(rig: &Rig, request: &proto::SetDesiredRequest) -> Answered {
    call(rig, SET_DESIRED, Some(OPERATOR), request).await
}

/// `GetRelease` of `service` at `version` with `bearer`.
async fn get(rig: &Rig, bearer: Option<&str>, service: &str, version: &str) -> Answered {
    let request = proto::GetReleaseRequest {
        service: service.to_owned(),
        version: version.to_owned(),
    };
    call(rig, GET_RELEASE, bearer, &request).await
}

/// What `GetRelease` answers for `service` at `version`, read with the operator token.
async fn release(rig: &Rig, service: &str, version: &str) -> proto::GetReleaseResponse {
    get(rig, Some(OPERATOR), service, version).await.decoded()
}

/// The `set_desired` entries of the audit log, oldest first.
async fn audited(rig: &Rig) -> Vec<proto::AuditEntry> {
    let request = proto::ListAuditRequest {
        action: Some("set_desired".to_owned()),
        ..proto::ListAuditRequest::default()
    };
    let answered = call(rig, AUDIT_LIST, Some(OPERATOR), &request).await;
    answered.decoded::<proto::ListAuditResponse>().entries
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `bytes`, in lower-case hex.
fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The text of the file `name` in the rig's directory, if there is one.
fn file(rig: &Rig, name: &str) -> Option<String> {
    std::fs::read_to_string(rig.dir.path().join(name)).ok()
}

/// Everything a set could change: the files, the audit and the release read back.
#[derive(Debug, PartialEq)]
struct Kept {
    desired: Option<String>,
    releases: Option<String>,
    ops: Option<String>,
    audit: Vec<proto::AuditEntry>,
    answer: Option<proto::GetReleaseResponse>,
}

async fn kept(rig: &Rig, service: &str, version: &str) -> Kept {
    let answered = get(rig, Some(OPERATOR), service, version).await;
    Kept {
        desired: file(rig, "desired.json"),
        releases: file(rig, "desired.json.releases"),
        ops: file(rig, "desired.json.ops"),
        audit: audited(rig).await,
        answer: answered.body.is_some().then(|| answered.decoded()),
    }
}

/// Asserts that a call was refused as `release_exists` (`FAILED_PRECONDITION`).
fn assert_release_exists(answered: &Answered) {
    assert_eq!(
        answered.refusal(),
        (FAILED_PRECONDITION, "release_exists"),
        "{answered:?}"
    );
}

/// The targets of `hostkeeper`: its native entry (`apt` from a repository) on each of two Linux
/// platforms, the packages' digests `c` and the next letter.
fn own_targets(c: char) -> Targets {
    let next = char::from(u8::try_from(c).expect("ascii") + 1);
    BTreeMap::from([
        (
            "linux-x86_64".to_owned(),
            list(vec![repository_entry("hostkeeper", "2.0-1", c)]),
        ),
        (
            "linux-arm64".to_owned(),
            list(vec![repository_entry("hostkeeper", "2.0-1", next)]),
        ),
    ])
}

/// A release index of `hostkeeper` at `version` as an earlier CI wrote it, listing the digest of
/// each platform's first entry, in the canonical form, or pretty-printed. The coordinator keeps
/// its digest and never reads its text.
fn index_of(version: &str, targets: &Targets, pretty: bool) -> String {
    let packages: Vec<Value> = targets
        .iter()
        .map(|(platform, list)| {
            json!({
                "file": format!("hostkeeper-{version}-{platform}.tar.zst"),
                "platform": platform,
                "sha256": list.entries[0].digest,
            })
        })
        .collect();
    let index = json!({
        "packages": packages,
        "schema": 1,
        "service": "hostkeeper",
        "version": version,
    });
    if pretty {
        serde_json::to_string_pretty(&index).expect("JSON")
    } else {
        index.to_string()
    }
}

/// A set of `hostkeeper` at `version` with `targets` and the release index `text`, if any.
fn set_own(version: &str, targets: Targets, text: Option<&str>) -> proto::SetDesiredRequest {
    let mut request = set("hostkeeper", version, targets);
    request.release_index = text.map(str::to_owned);
    request.release_index_digest = text.map(|t| sha(t.as_bytes()));
    request
}

/// The reason of the audit entry of a set, as the design words it: `version with targets`, the
/// workflow when CI set it, the release index digest when there is one, then each platform's
/// entries as installer, digest and the SHA-256 of the manifest's encoding; a repository entry
/// names its repository, its package and its package version before its pin.
fn reason(version: &str, workflow: Option<&str>, index: Option<&str>, targets: &Targets) -> String {
    let mut text = format!("{version} with targets");
    if let Some(workflow) = workflow {
        text.push_str(&format!(" by workflow {workflow}"));
    }
    if let Some(index) = index {
        text.push_str(&format!(" and release index {index}"));
    }
    let platforms: Vec<String> = targets
        .iter()
        .map(|(platform, list)| {
            let entries: Vec<String> = list
                .entries
                .iter()
                .map(|e| {
                    let manifest = e.manifest.as_ref().expect("a manifest").encode_to_vec();
                    let package = |m: &hostkeeper_proto::hostkeeper::release::v1::Manifest| {
                        m.package.clone().expect("a package")
                    };
                    let from = match (&e.repository, &e.package_version) {
                        (Some(repository), Some(version)) => format!(
                            " repository {repository} package {} package_version {version}",
                            package(e.manifest.as_ref().expect("a manifest"))
                        ),
                        _ => String::new(),
                    };
                    format!(
                        "{}{from} {} manifest {}",
                        e.installer,
                        e.digest,
                        sha(&manifest)
                    )
                })
                .collect();
            format!("{platform} {}", entries.join(", "))
        })
        .collect();
    format!("{text}: {}", platforms.join("; "))
}

/// Proves: the same targets set again under one version are accepted, answer as the first set
/// did, leave the release as it was, and are audited again; by the gateway as by gRPC.
/// Catches: a repeat of a release refused as `release_exists` (CI's retry of a failed run would
/// then never finish) or one that rewrites the release.
#[tokio::test]
async fn an_identical_repeat_is_accepted_and_audited_again() {
    let rig = rig("kept-repeat");
    let request = set("web", "2.0", targets("web"));
    let first = ci_set(&rig, &request).await;
    assert_eq!(first.code, 0, "{first:?}");
    let before = kept(&rig, "web", "2.0").await;
    let again = ci_set(&rig, &request).await;
    assert_eq!(
        again.decoded::<proto::SetDesiredResponse>(),
        first.decoded::<proto::SetDesiredResponse>()
    );
    let (code, answer) = gateway(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set_json(&request).to_string(),
    )
    .await;
    assert_eq!(code, 200, "{answer}");
    let after = kept(&rig, "web", "2.0").await;
    assert_eq!(after.releases, before.releases);
    assert_eq!(after.answer, before.answer);
    assert_eq!(after.answer.expect("a release").targets, targets("web"));
    let reasons: Vec<_> = after.audit.iter().map(|e| e.reason.clone()).collect();
    let want = Some(reason("2.0", Some(WORKFLOW), None, &targets("web")));
    assert_eq!(reasons, vec![want.clone(), want.clone(), want]);
}

/// Proves: a version set with targets is set again with other targets only as `release_exists`
/// (`FAILED_PRECONDITION`), and then nothing changes: not the files, the audit or the release
/// read back. The other targets are the same entries in another order, another artifact digest,
/// another size, another manifest, an entry more, a platform less, and a platform more.
/// Catches: an order of preference compared as a set, a manifest left out of the comparison, a
/// refused set that still writes.
#[tokio::test]
async fn other_targets_under_one_version_are_release_exists_and_change_nothing() {
    let rig = rig("kept-other-targets");
    let first = ci_set(&rig, &set("web", "2.0", targets("web"))).await;
    assert_eq!(first.code, 0, "{first:?}");
    let before = kept(&rig, "web", "2.0").await;

    let mut reordered = targets("web");
    reordered
        .get_mut("linux-x86_64")
        .expect("a list")
        .entries
        .swap(0, 1);
    let mut digest = targets("web");
    digest.get_mut("macos-arm64").expect("a list").entries[0].digest =
        support::targets::digest('9');
    let mut size = targets("web");
    size.get_mut("macos-arm64").expect("a list").entries[0].bytes += 1;
    let mut manifest = targets("web");
    manifest.get_mut("macos-arm64").expect("a list").entries[0]
        .manifest
        .as_mut()
        .expect("a manifest")
        .min_hostkeeper = "0.5.0".to_owned();
    let mut more = targets("web");
    more.get_mut("macos-arm64")
        .expect("a list")
        .entries
        .push(entry("web", "deb", 'f', 1_000));
    let mut fewer = targets("web");
    fewer.remove("macos-arm64");
    let mut platform = targets("web");
    platform.insert(
        "linux-arm64".to_owned(),
        list(vec![entry("web", "tarball", 'f', 1_000)]),
    );

    for (what, other) in [
        ("entries in another order", reordered),
        ("another artifact digest", digest),
        ("another size", size),
        ("another manifest", manifest),
        ("an entry more", more),
        ("a platform less", fewer),
        ("a platform more", platform),
    ] {
        let answered = ci_set(&rig, &set("web", "2.0", other.clone())).await;
        assert_eq!(
            answered.refusal(),
            (FAILED_PRECONDITION, "release_exists"),
            "{what}"
        );
        let answered = operator_set(&rig, &set("web", "2.0", other)).await;
        assert_eq!(
            answered.refusal(),
            (FAILED_PRECONDITION, "release_exists"),
            "{what}, by an operator"
        );
        assert_eq!(kept(&rig, "web", "2.0").await, before, "{what}");
    }
}

/// Proves: the release index digest is part of the release: the same targets set again with
/// another index (the same packages in another text), with none, or the same targets first set
/// with none and then with one, are `release_exists` and change nothing; the same targets and
/// the same index are accepted.
/// Catches: a release compared by its targets alone.
#[tokio::test]
async fn another_release_index_digest_under_one_version_is_release_exists() {
    let rig = rig("kept-other-index");
    let targets = own_targets('a');
    let canonical = index_of("3.0", &targets, false);
    let pretty = index_of("3.0", &targets, true);
    assert_ne!(sha(canonical.as_bytes()), sha(pretty.as_bytes()));

    let first = ci_set(&rig, &set_own("3.0", targets.clone(), Some(&canonical))).await;
    assert_eq!(first.code, 0, "{first:?}");
    let before = kept(&rig, "hostkeeper", "3.0").await;
    assert_eq!(
        before
            .answer
            .as_ref()
            .and_then(|a| a.release_index_digest.clone()),
        Some(sha(canonical.as_bytes()))
    );
    for (what, other) in [
        (
            "another index text",
            set_own("3.0", targets.clone(), Some(&pretty)),
        ),
        ("no index", set_own("3.0", targets.clone(), None)),
    ] {
        let answered = ci_set(&rig, &other).await;
        assert_eq!(
            answered.refusal(),
            (FAILED_PRECONDITION, "release_exists"),
            "{what}"
        );
        assert_eq!(kept(&rig, "hostkeeper", "3.0").await, before, "{what}");
    }
    let same = ci_set(&rig, &set_own("3.0", targets.clone(), Some(&canonical))).await;
    assert_eq!(
        same.decoded::<proto::SetDesiredResponse>(),
        first.decoded::<proto::SetDesiredResponse>()
    );

    let bare = set_own("3.1", targets.clone(), None);
    assert_eq!(ci_set(&rig, &bare).await.code, 0);
    let indexed = set_own(
        "3.1",
        targets,
        Some(&index_of("3.1", &own_targets('a'), false)),
    );
    assert_release_exists(&ci_set(&rig, &indexed).await);
    assert_eq!(
        release(&rig, "hostkeeper", "3.1")
            .await
            .release_index_digest,
        None
    );
}

/// Proves: a set with targets of a version the registry already names (first set with a package
/// digest, or with a release index and no targets) is `release_exists` and changes nothing; a
/// version first set with plain limits is not a version of the registry, and is accepted.
/// Catches: a release recorded beside a package digest the older coordinator would then order.
#[tokio::test]
async fn targets_for_a_version_set_before_with_a_package_digest_or_index_are_release_exists() {
    let rig = rig("kept-registry");
    let mut digest = set("web", "2.0", BTreeMap::new());
    digest.package_digest = Some(support::targets::digest('1'));
    assert_eq!(ci_set(&rig, &digest).await.code, 0);
    let text = index_of("2.0", &own_targets('a'), false);
    let mut indexed = set("hostkeeper", "2.0", BTreeMap::new());
    indexed.release_index = Some(text.clone());
    indexed.release_index_digest = Some(sha(text.as_bytes()));
    assert_eq!(ci_set(&rig, &indexed).await.code, 0);
    let plain = set("web", "1.0", BTreeMap::new());
    assert_eq!(ci_set(&rig, &plain).await.code, 0);

    let before_web = kept(&rig, "web", "2.0").await;
    assert_release_exists(&ci_set(&rig, &set("web", "2.0", targets("web"))).await);
    assert_eq!(kept(&rig, "web", "2.0").await, before_web);
    assert_eq!(before_web.releases, None);

    let before_host = kept(&rig, "hostkeeper", "2.0").await;
    let with_index = set_own("2.0", own_targets('a'), Some(&text));
    assert_release_exists(&ci_set(&rig, &with_index).await);
    assert_release_exists(&operator_set(&rig, &with_index).await);
    assert_eq!(kept(&rig, "hostkeeper", "2.0").await, before_host);

    assert_eq!(
        ci_set(&rig, &set("web", "1.0", targets("web"))).await.code,
        0
    );
    assert_eq!(release(&rig, "web", "1.0").await.targets, targets("web"));
}

/// Proves: a set without targets that names a package digest or a release index of a version
/// that has a release is `release_exists` (gRPC `FAILED_PRECONDITION`, the frozen JSON route
/// 409 `{"error":"release_exists"}`), changes nothing, and the release is still read back.
/// Catches: a package digest recorded for a version that has targets, which the older
/// coordinator would order the one package.
#[tokio::test]
async fn a_package_digest_or_index_for_a_version_with_a_release_is_release_exists() {
    let rig = rig("kept-no-targets");
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", targets("web"))).await.code,
        0
    );
    let host_targets = own_targets('a');
    let text = index_of("2.0", &host_targets, false);
    let first = ci_set(&rig, &set_own("2.0", host_targets.clone(), Some(&text))).await;
    assert_eq!(first.code, 0, "{first:?}");
    let before_web = kept(&rig, "web", "2.0").await;
    let before_host = kept(&rig, "hostkeeper", "2.0").await;

    let mut digest = set("web", "2.0", BTreeMap::new());
    digest.package_digest = Some(support::targets::digest('1'));
    assert_release_exists(&ci_set(&rig, &digest).await);
    assert_release_exists(&operator_set(&rig, &digest).await);
    let mut index = set("hostkeeper", "2.0", BTreeMap::new());
    index.release_index = Some(text.clone());
    index.release_index_digest = Some(sha(text.as_bytes()));
    assert_release_exists(&ci_set(&rig, &index).await);
    assert_release_exists(&operator_set(&rig, &index).await);

    let by_ci = |body: Value| {
        let rig = &rig;
        async move { rig.desired(&body.to_string()).await }
    };
    let (code, body) = by_ci(json!({
        "service": "web", "version": "2.0", "release_digest": support::targets::digest('1'),
    }))
    .await;
    assert_eq!(
        (code, body.as_str()),
        (409, r#"{"error":"release_exists"}"#)
    );
    let (code, body) = by_ci(json!({
        "service": "hostkeeper", "version": "2.0",
        "release_index": text, "release_index_digest": sha(text.as_bytes()),
    }))
    .await;
    assert_eq!(
        (code, body.as_str()),
        (409, r#"{"error":"release_exists"}"#)
    );

    assert_eq!(kept(&rig, "web", "2.0").await, before_web);
    assert_eq!(kept(&rig, "hostkeeper", "2.0").await, before_host);
}

/// Proves: a plain set (no package digest, no index, no targets) of a version that has a release
/// is accepted and changes the limits, over gRPC and the frozen JSON route; the release stays,
/// the releases file is not rewritten, and the set is not audited (it is CI's, without targets).
/// Catches: a set of new limits for a released version refused, or one that drops the release.
#[tokio::test]
async fn a_plain_set_of_a_released_version_changes_the_limits_and_keeps_the_release() {
    let rig = rig("kept-plain");
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", targets("web"))).await.code,
        0
    );
    let before = kept(&rig, "web", "2.0").await;

    let mut plain = set("web", "2.0", BTreeMap::new());
    plain.desired = Some(proto::DesiredState {
        version: "2.0".to_owned(),
        max_updating: [("daemon".to_owned(), 1)].into(),
        ..proto::DesiredState::default()
    });
    let answered = ci_set(&rig, &plain).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let response: proto::SetDesiredResponse = answered.decoded();
    assert_eq!(response.desired.expect("a state").max_updating["daemon"], 1);
    let state: Value =
        serde_json::from_str(&file(&rig, "desired.json").expect("state")).expect("JSON");
    assert_eq!(state["services"]["web"]["max_updating"]["daemon"], 1);
    let after = kept(&rig, "web", "2.0").await;
    assert_eq!(after.releases, before.releases);
    assert_eq!(after.answer, before.answer);
    assert_eq!(after.audit, before.audit);

    let (code, body) = rig
        .desired(r#"{"service":"web","version":"2.0","max_updating":{"daemon":2}}"#)
        .await;
    assert_eq!(code, 200, "{body}");
    let state: Value =
        serde_json::from_str(&file(&rig, "desired.json").expect("state")).expect("JSON");
    assert_eq!(state["services"]["web"]["max_updating"]["daemon"], 2);
    assert_eq!(release(&rig, "web", "2.0").await.targets, targets("web"));
    assert_eq!(file(&rig, "desired.json.releases"), before.releases);
}

/// Proves: a set with targets by CI writes exactly one `set_desired` entry, made by `ci` under
/// the name `ci` for the service, whose reason is the version, the workflow, and each platform's
/// entries in order with the artifact's digest and the SHA-256 of the manifest's encoding; and a
/// CI set without targets writes none.
/// Catches: a release set by CI that leaves no trace, an entry that names no workflow, or one
/// that lists the entries in another order than the list.
#[tokio::test]
async fn a_set_with_targets_by_ci_is_audited_once_with_the_release_in_the_reason() {
    let rig = rig("kept-audit-ci");
    let answered = ci_set(&rig, &set("web", "2.0", targets("web"))).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let entries = audited(&rig).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.by, proto::Actor::Ci as i32);
    assert_eq!(entry.by_name.as_deref(), Some("ci"));
    assert_eq!(entry.service.as_deref(), Some("web"));
    assert_eq!(entry.node_id, None);
    assert_eq!(
        entry.reason.as_deref(),
        Some(reason("2.0", Some(WORKFLOW), None, &targets("web")).as_str())
    );

    let plain = ci_set(&rig, &set("web", "2.1", BTreeMap::new())).await;
    assert_eq!(plain.code, 0, "{plain:?}");
    assert_eq!(audited(&rig).await, entries);
}

/// Proves: a set with targets by an operator writes exactly one `set_desired` entry under the
/// operator token's name, and its reason has no workflow; with a release index it names the
/// index's digest, after the version and before the entries.
/// Catches: an operator's release that is not audited, or an entry that names CI's workflow
/// for a set CI did not make.
#[tokio::test]
async fn a_set_with_targets_by_an_operator_is_audited_once_under_the_token_name() {
    let rig = rig("kept-audit-operator");
    let answered = operator_set(&rig, &set("web", "2.0", targets("web"))).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let entries = audited(&rig).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].by, proto::Actor::Operator as i32);
    assert_eq!(entries[0].by_name.as_deref(), Some("operator"));
    assert_eq!(entries[0].service.as_deref(), Some("web"));
    assert_eq!(
        entries[0].reason.as_deref(),
        Some(reason("2.0", None, None, &targets("web")).as_str())
    );

    let own = own_targets('a');
    let text = index_of("3.0", &own, false);
    let digest = sha(text.as_bytes());
    let answered = operator_set(&rig, &set_own("3.0", own.clone(), Some(&text))).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let entries = audited(&rig).await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(entries[1].service.as_deref(), Some("hostkeeper"));
    let want = reason("3.0", None, Some(&digest), &own);
    assert_eq!(entries[1].reason.as_deref(), Some(want.as_str()));
    assert!(want.starts_with(&format!(
        "3.0 with targets and release index {digest}: linux-arm64 "
    )));

    let by_ci = ci_set(&rig, &set_own("3.0", own.clone(), Some(&text))).await;
    assert_eq!(by_ci.code, 0);
    let entries = audited(&rig).await;
    assert_eq!(entries.len(), 3);
    let want = reason("3.0", Some(WORKFLOW), Some(&digest), &own);
    assert_eq!(entries[2].reason.as_deref(), Some(want.as_str()));
}

/// Proves: `GetRelease` refuses what it cannot answer with a token: a service the coordinator
/// does not manage is `NOT_FOUND` `unknown_service`; a version never set with targets (unknown,
/// or set without) is `NOT_FOUND` `release_unknown`; an empty service with no legacy service is
/// `INVALID_ARGUMENT` `invalid_request`; no token, and a token that is none, are
/// `UNAUTHENTICATED` `unauthorized`. It changes nothing.
/// Catches: a read of a missing release answered with an empty release.
#[tokio::test]
async fn get_release_refuses_the_unknown_and_the_unauthorized() {
    let rig = rig("kept-get-refused");
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", targets("web"))).await.code,
        0
    );
    assert_eq!(
        ci_set(&rig, &set("web", "2.1", BTreeMap::new())).await.code,
        0
    );
    let before = kept(&rig, "web", "2.0").await;
    for (service, version, want) in [
        ("nope", "2.0", (NOT_FOUND, "unknown_service")),
        ("web", "3.0", (NOT_FOUND, "release_unknown")),
        ("web", "2.1", (NOT_FOUND, "release_unknown")),
        ("hostkeeper", "2.0", (NOT_FOUND, "release_unknown")),
        ("", "2.0", (INVALID_ARGUMENT, "invalid_request")),
    ] {
        for token in [Some(OPERATOR.to_owned()), Some(ci_token())] {
            let answered = get(&rig, token.as_deref(), service, version).await;
            assert_eq!(answered.refusal(), want, "{service} {version}");
        }
    }
    for token in [None, Some("not-a-token"), Some(&OPERATOR[1..])] {
        let answered = get(&rig, token, "web", "2.0").await;
        assert_eq!(
            answered.refusal(),
            (UNAUTHENTICATED, "unauthorized"),
            "{token:?}"
        );
    }
    assert_eq!(kept(&rig, "web", "2.0").await, before);
}

/// Proves: `GetRelease` answers a release to CI's token and to the operator's alike, with each
/// platform's entries in the order they were set (an order that is not the sorted one) and
/// without a release index digest when none came; with an index it answers the index's digest.
/// An empty service names the legacy service.
/// Catches: entries sorted on the way back, a digest invented for a set without an index.
#[tokio::test]
async fn get_release_answers_the_lists_in_the_order_set_and_the_index_digest() {
    let rig = rig("kept-get-answers");
    let mut in_order = targets("web");
    in_order
        .get_mut("linux-x86_64")
        .expect("a list")
        .entries
        .reverse();
    let installers: Vec<_> = in_order["linux-x86_64"]
        .entries
        .iter()
        .map(|e| e.installer.as_str())
        .collect();
    assert_eq!(installers, ["tarball", "rpm", "deb"]);
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", in_order.clone()))
            .await
            .code,
        0
    );
    for token in [ci_token(), OPERATOR.to_owned()] {
        let answered = get(&rig, Some(&token), "web", "2.0").await;
        let read: proto::GetReleaseResponse = answered.decoded();
        assert_eq!(read.targets, in_order);
        assert_eq!(read.release_index_digest, None);
    }

    let own = own_targets('a');
    let text = index_of("3.0", &own, false);
    assert_eq!(
        ci_set(&rig, &set_own("3.0", own.clone(), Some(&text)))
            .await
            .code,
        0
    );
    let read = release(&rig, "hostkeeper", "3.0").await;
    assert_eq!(read.targets, own);
    assert_eq!(read.release_index_digest, Some(sha(text.as_bytes())));

    let legacy = Rig::new("kept-get-legacy", Spec::recorded().with_legacy("kbf")).routed();
    let mut request = set("kbf", "2.0", targets("kbf"));
    request.desired.as_mut().expect("a state").min_up =
        [("storage".to_owned(), 0), ("frontend".to_owned(), 0)].into();
    let answered = ci_set(&legacy, &request).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read: proto::GetReleaseResponse =
        get(&legacy, Some(&ci_token()), "", "2.0").await.decoded();
    assert_eq!(read.targets, targets("kbf"));
}

/// Proves: a restart of the coordinator over the same files keeps the release: `GetRelease`
/// answers what it answered, with and without an index digest, `desired.json.releases` holds each
/// platform's entries in the order set, the files are the same, and the set again of other
/// targets is still `release_exists` while the same targets are still accepted.
/// Catches: a release held in memory only, one read back in another order, or a repeat after a
/// restart that is refused.
#[tokio::test]
async fn a_restart_keeps_the_release_and_the_releases_file_holds_it() {
    let mut rig = rig("kept-restart");
    let mut in_order = targets("web");
    in_order
        .get_mut("linux-x86_64")
        .expect("a list")
        .entries
        .reverse();
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", in_order.clone()))
            .await
            .code,
        0
    );
    let own = own_targets('a');
    let text = index_of("3.0", &own, false);
    let digest = sha(text.as_bytes());
    assert_eq!(
        ci_set(&rig, &set_own("3.0", own.clone(), Some(&text)))
            .await
            .code,
        0
    );
    let before = (
        kept(&rig, "web", "2.0").await,
        kept(&rig, "hostkeeper", "3.0").await,
    );

    let text_of_file = file(&rig, "desired.json.releases").expect("the releases file");
    let book: Value = serde_json::from_str(&text_of_file).expect("JSON");
    let installers: Vec<&str> =
        book["services"]["web"]["2.0"]["targets"]["linux-x86_64"]["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e["installer"].as_str().expect("an installer"))
            .collect();
    assert_eq!(installers, ["tarball", "rpm", "deb"]);
    assert_eq!(
        book["services"]["web"]["2.0"]["targets"]["macos-arm64"]["entries"][0]["digest"],
        support::targets::digest('d')
    );
    assert_eq!(
        book["services"]["web"]["2.0"].get("release_index_digest"),
        None
    );
    assert_eq!(
        book["services"]["hostkeeper"]["3.0"]["release_index_digest"],
        digest
    );

    rig.restart();
    let after = (
        kept(&rig, "web", "2.0").await,
        kept(&rig, "hostkeeper", "3.0").await,
    );
    assert_eq!(after, before);
    assert_eq!(after.0.answer.expect("a release").targets, in_order);
    assert_eq!(
        after.1.answer.expect("a release").release_index_digest,
        Some(digest)
    );
    assert_eq!(file(&rig, "desired.json.releases"), Some(text_of_file));
    assert_eq!(ci_set(&rig, &set("web", "2.0", in_order)).await.code, 0);
    assert_release_exists(&ci_set(&rig, &set("web", "2.0", targets("web"))).await);
}

/// The reader of the desired-state file as the coordinator before targets had it: the map form,
/// every struct refusing a key it does not know, each state its version, its limits, the digest
/// and the index record of its release, and the registry `digests`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenFile {
    services: BTreeMap<String, FrozenEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenEntry {
    version: String,
    #[serde(default)]
    max_updating: BTreeMap<String, u32>,
    min_up: BTreeMap<String, u32>,
    #[serde(default)]
    release_digest: Option<String>,
    #[serde(default)]
    release_index: Option<FrozenIndex>,
    #[serde(default)]
    digests: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    dead_code,
    reason = "the reader names the keys it takes; no set with targets writes the record"
)]
struct FrozenIndex {
    digest: String,
    packages: BTreeMap<String, String>,
}

/// Proves: the `desired.json` written for a set with targets (with and without a release index)
/// is read by the previous coordinator's strict reader and by kbf's mirror of it: it holds no
/// `release_digest`, `release_index` or `digests` key for the version, no key the reader
/// refuses, and the limits and version that were set; the targets are in the releases file
/// beside it and nowhere in it.
/// Catches: a release written into the state file, which an older coordinator refuses to start
/// on (it reads that file strictly).
#[tokio::test]
async fn the_state_file_of_a_set_with_targets_is_read_by_the_previous_coordinators_reader() {
    let rig = rig("kept-frozen");
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", targets("web"))).await.code,
        0
    );
    let own = own_targets('a');
    let text = index_of("3.0", &own, false);
    assert_eq!(
        ci_set(&rig, &set_own("3.0", own, Some(&text))).await.code,
        0
    );
    let state = file(&rig, "desired.json").expect("the state file");
    let frozen: FrozenFile = serde_json::from_str(&state).expect("the previous reader takes it");
    let web = &frozen.services["web"];
    assert_eq!(web.version, "2.0");
    assert_eq!(web.max_updating["daemon"], 2);
    assert!(web.min_up.contains_key("storage"));
    assert_eq!(
        (
            &web.release_digest,
            web.release_index.is_none(),
            &web.digests
        ),
        (&None, true, &None)
    );
    let host = &frozen.services["hostkeeper"];
    assert_eq!(host.version, "3.0");
    assert_eq!(
        (
            &host.release_digest,
            host.release_index.is_none(),
            &host.digests
        ),
        (&None, true, &None)
    );
    for key in ["release_digest", "release_index", "digests", "targets"] {
        assert!(!state.contains(key), "{key} in {state}");
    }
    assert!(state.ends_with("}\n"));
    let stored = file(&rig, "desired.json.releases").expect("the releases file");
    assert!(stored.contains("komira-testing"), "{stored}");

    Readers::recorded().check_desired_in(rig.dir.path(), "desired-map");
}

/// Copies the file `name` from `from` to `to`.
fn copy(from: &Path, to: &Path, name: &str) {
    std::fs::copy(from.join(name), to.join(name)).expect("copied");
}

/// The hold the status gives the host `d1` for `web`, after `d1` reports `web` staged and never
/// run, on a daemon that can install.
async fn hold_of_d1(rig: &Rig) -> Value {
    rig.at(2_000);
    let (code, body) = rig
        .heartbeat(
            "d1",
            "daemon",
            r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy",
                "agent_features":["install-v1"],
                "services":[{"name":"web","version":"1.0","health":"absent"}]}"#,
        )
        .await;
    assert_eq!(code, 200, "{body}");
    let status = rig.status().await;
    let hosts = status["per_service"]["web"]["hosts"]
        .as_array()
        .expect("hosts");
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    hosts[0]["hold"].clone()
}

/// Proves: a coordinator that reads only `desired.json` (the previous one, which has no releases
/// file: here the same state file with `desired.json.releases` left behind) sees a version set
/// with targets as a version with neither package digest nor index, which has no release kept:
/// a host that does not run the service is held `no_build_for_host` and told nothing. The
/// coordinator with the releases picks the host from them (section 3.3): its daemon, before
/// `target-v1`, lists nothing a pick can name, so it is held `no_build_for_host` and told nothing
/// either. Only the coordinator that reads the releases file answers `GetRelease`. A version
/// set with a package digest on the previous coordinator's state orders the same host nothing.
/// Catches: a version set with targets that a coordinator without its release would order some
/// package, or that the new one holds differently.
#[tokio::test]
async fn the_previous_coordinator_holds_a_version_set_with_targets_as_no_digest() {
    let rig = rig("kept-previous-new");
    assert_eq!(
        ci_set(&rig, &set("web", "2.0", targets("web"))).await.code,
        0
    );

    let mut old = Rig::new("kept-previous-old", rig.spec.clone()).routed();
    copy(rig.dir.path(), old.dir.path(), "desired.json");
    assert!(!old.dir.path().join("desired.json.releases").exists());
    old.restart();

    assert_eq!(hold_of_d1(&rig).await, json!("no_build_for_host"));
    assert_eq!(hold_of_d1(&old).await, json!("no_build_for_host"));
    assert_eq!(release(&rig, "web", "2.0").await.targets, targets("web"));
    assert_eq!(
        get(&old, Some(OPERATOR), "web", "2.0").await.refusal(),
        (NOT_FOUND, "release_unknown")
    );

    let (code, body) = old
        .desired(
            &json!({"service": "web", "version": "2.1",
            "release_digest": support::targets::digest('1')})
            .to_string(),
        )
        .await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(hold_of_d1(&old).await, json!("no_build_for_host"));
}
