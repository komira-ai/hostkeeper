//! `Releases.SetDesired` against a release channel (`--release-channel`, `crate::channel`): a
//! version the channel does not hold, or holds another artifact for, is refused with the
//! existing refusal shape, `FAILED_PRECONDITION` and the token as the whole message, for CI's
//! token and an operator's alike, and nothing changes. The channel is a fake fetcher.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use tonic::Code;

use super::operator::{World, as_ci, as_operator, state};
use crate::channel::ReleaseChannel;
use crate::channel_tests::{D1, D2, FakeChannel, index};
use crate::grpc::operator::ReleasesService;
use crate::testutil::{LEGACY, Rig};

/// A world that checks every set against `fake`.
fn world(name: &str, fake: &Arc<FakeChannel>) -> World {
    let rig = Rig::new(name);
    let started =
        Rig::start(&rig.dir, &rig.clock).with_release_channel(ReleaseChannel::new(fake.clone()));
    World::over(rig, started)
}

/// The set of `version` of kbf with the package digest `digest`.
fn set_request(version: &str, digest: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: LEGACY.to_owned(),
        desired: Some(state(version, &[])),
        package_digest: Some(digest.to_owned()),
        ..Default::default()
    }
}

/// The files a set could change.
fn files(world: &World) -> [Option<Vec<u8>>; 3] {
    ["desired.json", "desired.json.ops", "desired.json.ops.audit"].map(|name| world.file(name))
}

/// Catches: a channel refusal reaching a gRPC caller as another code (`UNAVAILABLE` would read
/// as no answer to CI's client) or with more than its token, for either caller; a refused set
/// writing anything; and a set the channel holds refused.
#[tokio::test]
async fn set_desired_is_refused_with_the_channels_token_for_either_caller() {
    let fake = Arc::new(FakeChannel::default());
    let world = world("grpc-channel", &fake);
    let releases = ReleasesService::new(world.coordinator.clone());
    fake.hold(LEGACY, "3.0", &index(LEGACY, "3.0", &[D1]));
    let before = files(&world);
    for (version, digest, token) in [
        ("2.0", D1, "not_in_channel"),
        ("3.0", D2, "channel_digest_mismatch"),
    ] {
        let by_ci = releases
            .set_desired(as_ci(&world.coordinator, set_request(version, digest)).await)
            .await
            .expect_err("refused");
        assert_eq!(
            (by_ci.code(), by_ci.message()),
            (Code::FailedPrecondition, token),
            "{version}"
        );
        assert!(by_ci.details().is_empty());
        let by_operator = releases
            .set_desired(as_operator(&world.coordinator, set_request(version, digest)).await)
            .await
            .expect_err("refused");
        assert_eq!(
            (by_operator.code(), by_operator.message()),
            (Code::FailedPrecondition, token),
            "{version}"
        );
    }
    assert_eq!(files(&world), before);
    let set = releases
        .set_desired(as_ci(&world.coordinator, set_request("3.0", D1)).await)
        .await
        .expect("set")
        .into_inner();
    assert_eq!(
        set.desired.map(|desired| desired.version),
        Some("3.0".to_owned())
    );
}
