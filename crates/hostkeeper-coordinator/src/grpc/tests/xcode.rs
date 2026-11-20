//! `Xcode` on the operator API through `crate::grpc::route`: `List` and `Remove` over gRPC answer
//! what their handlers answer, a refusal is the status of its value, and a caller that is no
//! operator is refused before the store is read. No socket.

use std::fs;
use std::io::Write as _;
use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use tonic::Code;

use super::operator::World;
use super::support::{decode, raw, updater};
use super::targets::grpc;
use crate::http::{Peer, Surface};
use crate::testutil::{OPERATOR, Rig, ci_token};
use crate::xcode_feed::{XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, XcodeStore};

const LIST: &str = "/hostkeeper.operator.v1.Xcode/List";
const REMOVE: &str = "/hostkeeper.operator.v1.Xcode/Remove";

/// The reason of the removals of the tests.
const WHY: &str = "superseded";

/// A world whose coordinator keeps an Xcode store of at most 1 MiB archives in a directory of its
/// own when `with_store`, and the store.
fn world(name: &str, with_store: bool) -> (World, Option<Arc<XcodeStore>>) {
    let rig = Rig::new(name);
    let coordinator = Rig::start(&rig.dir, &rig.clock);
    if !with_store {
        return (World::over(rig, coordinator), None);
    }
    let dir = rig.dir.path().join("xcode");
    fs::create_dir(&dir).expect("the store's directory");
    let store = Arc::new(XcodeStore::open(&dir, 1 << 20, std::time::UNIX_EPOCH).expect("a store"));
    let coordinator = coordinator.with_xcode_store(Arc::clone(&store));
    (World::over(rig, coordinator), Some(store))
}

/// Stores `bytes` in `store` as the archive of `version` and `build`.
fn stored(store: &XcodeStore, version: &str, build: &str, bytes: &[u8]) -> Archive {
    let mut digest = Digesting::default();
    digest.feed(bytes);
    let sha256 = digest.finish();
    let (path, mut file) = store.upload_file(&sha256).expect("an upload file");
    file.write_all(bytes).expect("written");
    drop(file);
    let archive = Archive {
        sha256,
        id: XcodeId {
            version: XcodeVersion::parse(version).expect("a version number"),
            build: build.to_owned(),
        },
        size: u64::try_from(bytes.len()).expect("a size"),
        by: "uploader".to_owned(),
        at_unix: 1_800_000_000,
    };
    let _changing = store.changing();
    store.commit(&path, &archive).expect("committed");
    archive
}

/// How many entries the audit ring holds.
fn audited(world: &World) -> usize {
    world.coordinator.ops().state().audit.len()
}

fn remove_of(sha256: &str, reason: &str) -> proto::RemoveXcodeRequest {
    proto::RemoveXcodeRequest {
        sha256: sha256.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Proves: over gRPC, `List` with the operator token answers the store's archives newest release
/// first, `configured` true, a `keep` of 1 and the store's size limit; and a coordinator with no
/// store answers `configured` false and no archive.
///
/// Catches: an adapter that answers another handler's message, or one that refuses a coordinator
/// with no store.
#[tokio::test]
async fn list_over_grpc_answers_the_archives_newest_first_or_that_there_is_no_store() {
    let (world, store) = world("grpc-xcode-list", true);
    let store = store.expect("a store");
    let older = stored(&store, "27.1", "27A10", b"older");
    let newer = stored(&store, "27.2", "27B20", b"newer");
    let (code, message, answer) =
        grpc(&world, LIST, Some(OPERATOR), proto::ListXcodeRequest {}).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let listed: proto::ListXcodeResponse = decode(&answer.expect("an answer"));
    assert!(listed.configured);
    assert_eq!(
        listed
            .archives
            .iter()
            .map(|archive| (archive.sha256.as_str(), archive.version.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (newer.sha256.as_str(), "27.2"),
            (older.sha256.as_str(), "27.1")
        ]
    );
    assert_eq!((listed.keep, listed.max_bytes), (1, 1 << 20));

    let (bare, none) = world_without_store();
    assert!(none.is_none());
    let (code, message, answer) =
        grpc(&bare, LIST, Some(OPERATOR), proto::ListXcodeRequest {}).await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let listed: proto::ListXcodeResponse = decode(&answer.expect("an answer"));
    assert_eq!(
        listed,
        proto::ListXcodeResponse {
            configured: false,
            archives: vec![],
            waiting: None,
            keep: 0,
            max_bytes: 0,
        }
    );
}

/// A world with no store.
fn world_without_store() -> (World, Option<Arc<XcodeStore>>) {
    world("grpc-xcode-bare", false)
}

/// Proves: over gRPC, `Remove` with the operator token deletes the archive's files, writes one
/// `xcode_remove` entry and answers it, and removing it again is `NOT_FOUND` `unknown_archive`
/// with no second entry.
///
/// Catches: an adapter that drops the request's reason, answers no entry, or removes twice.
#[tokio::test]
async fn remove_over_grpc_deletes_the_archive_and_audits_it_once() {
    let (world, store) = world("grpc-xcode-remove", true);
    let store = store.expect("a store");
    let archive = stored(&store, "27.1", "27A10", b"the archive");
    let before = audited(&world);
    let (code, message, answer) = grpc(
        &world,
        REMOVE,
        Some(OPERATOR),
        remove_of(&archive.sha256, WHY),
    )
    .await;
    assert_eq!((code, message.as_str()), (Code::Ok, ""));
    let removed: proto::RemoveXcodeResponse = decode(&answer.expect("an answer"));
    let entry = removed.entry.expect("the entry");
    assert_eq!(
        (entry.action.as_str(), entry.reason.as_deref()),
        ("xcode_remove", Some(WHY))
    );
    assert_eq!(entry.by_name.as_deref(), Some("operator"));
    assert_eq!(audited(&world), before + 1);
    assert!(store.archives().expect("listed").is_empty());
    assert!(!store.archive_path(&archive.sha256).exists());

    let (code, message, _) = grpc(
        &world,
        REMOVE,
        Some(OPERATOR),
        remove_of(&archive.sha256, WHY),
    )
    .await;
    assert_eq!(
        (code, message.as_str()),
        (Code::NotFound, "unknown_archive")
    );
    assert_eq!(audited(&world), before + 1);
}

/// Proves: over gRPC, a sha256 the store does not hold is `NOT_FOUND` `unknown_archive`, one that
/// is not 64 lowercase hex characters and an empty reason are `INVALID_ARGUMENT`
/// `invalid_request`, and a coordinator with no store is `FAILED_PRECONDITION` `no_store`; none
/// removes a file or writes an entry.
///
/// Catches: a refusal told with another code or token than its proto comment names, or a refused
/// call that is audited.
#[tokio::test]
async fn remove_refusals_over_grpc_are_the_codes_of_their_values_and_change_nothing() {
    let (world, store) = world("grpc-xcode-refusals", true);
    let store = store.expect("a store");
    let archive = stored(&store, "27.1", "27A10", b"the archive");
    let before = audited(&world);
    for (request, code, token) in [
        (
            remove_of(&"ab".repeat(32), WHY),
            Code::NotFound,
            "unknown_archive",
        ),
        (
            remove_of("../etc", WHY),
            Code::InvalidArgument,
            "invalid_request",
        ),
        (
            remove_of(&archive.sha256, ""),
            Code::InvalidArgument,
            "invalid_request",
        ),
    ] {
        let (got, message, answer) = grpc(&world, REMOVE, Some(OPERATOR), request).await;
        assert_eq!((got, message.as_str()), (code, token));
        assert!(answer.is_none());
    }
    assert_eq!(audited(&world), before);
    assert!(store.archive_path(&archive.sha256).exists());

    let (bare, _) = world_without_store();
    let (code, message, _) = grpc(
        &bare,
        REMOVE,
        Some(OPERATOR),
        remove_of(&archive.sha256, WHY),
    )
    .await;
    assert_eq!(
        (code, message.as_str()),
        (Code::FailedPrecondition, "no_store")
    );
}

/// Proves: both methods refuse a CI token as `PERMISSION_DENIED` `forbidden` and a call with no
/// token as `UNAUTHENTICATED` `unauthorized`, and the archive stays and no entry is written. Each
/// CI call is the first of its coordinator, as a CI token is taken once.
///
/// Catches: a method of the store that CI's token or no token reaches.
#[tokio::test]
async fn list_and_remove_over_grpc_refuse_ci_and_no_token() {
    let ci = ci_token();
    for (method, name) in [(LIST, "List"), (REMOVE, "Remove")] {
        for bearer in [Some(ci.as_str()), None] {
            let (world, store) = world("grpc-xcode-callers", true);
            let store = store.expect("a store");
            let archive = stored(&store, "27.1", "27A10", b"the archive");
            let before = audited(&world);
            let expected = match bearer {
                Some(_) => (Code::PermissionDenied, "forbidden"),
                None => (Code::Unauthenticated, "unauthorized"),
            };
            let (code, message, _) = if method == LIST {
                grpc(&world, LIST, bearer, proto::ListXcodeRequest {}).await
            } else {
                grpc(&world, REMOVE, bearer, remove_of(&archive.sha256, WHY)).await
            };
            assert_eq!((code, message.as_str()), expected, "{name}");
            assert_eq!(audited(&world), before);
            assert!(store.archive_path(&archive.sha256).exists());
        }
    }
}

/// Proves: with a store, a path under `/v1/xcode/` is routed by the surface it arrives on: on the
/// heartbeat surface it is the store's read (a host in no group is 403 `forbidden`, a method other
/// than `GET` 405 `method_not_allowed`), on the private surface it is the upload (a `GET` is 405,
/// a `PUT` with no token 401 `unauthorized`); and without a store the path is a 404 on the
/// heartbeat surface.
///
/// Catches: a router that sends a read of the store to the upload or the upload to the read, and
/// one that serves the paths with no store behind them.
#[tokio::test]
async fn the_store_paths_follow_the_surface_and_need_a_store() {
    let (world, store) = world("grpc-xcode-surfaces", true);
    assert!(store.is_some());
    let path = format!("/v1/xcode/{}", "ab".repeat(32));
    let ask = |method: &str| {
        Request::builder()
            .method(method)
            .uri(path.as_str())
            .body(Full::new(Bytes::new()))
            .expect("a request")
    };
    let host = updater("m1", "mac");
    let nobody: Peer = None;
    let cases = [
        (
            Surface::Heartbeat,
            "GET",
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            Surface::Heartbeat,
            "PUT",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
        (
            Surface::Control,
            "GET",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
        (
            Surface::Control,
            "PUT",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
    ];
    for (surface, method, status, code) in cases {
        let who = if surface == Surface::Heartbeat {
            &host
        } else {
            &nobody
        };
        let (got, _, body) = raw(&world.coordinator, surface, who, ask(method)).await;
        assert_eq!(got, status, "{surface:?} {method}");
        assert_eq!(
            String::from_utf8(body.to_vec()).expect("text"),
            format!(r#"{{"error":"{code}"}}"#),
            "{surface:?} {method}"
        );
    }

    let (bare, _) = world_without_store();
    let (got, _, _) = raw(&bare.coordinator, Surface::Heartbeat, &host, ask("GET")).await;
    assert_eq!(got, StatusCode::NOT_FOUND);
}
