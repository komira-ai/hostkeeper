//! The move off archives (docs/design/packages-native.md section 10.6) as a daemon of the
//! release v0.1.221 sees it: that daemon runs `hostkeeper` from `hostkeeper-archive` and lists
//! `hostkeeper-archive` and, on a Linux host with a repository entry, `apt`. Its heartbeat is
//! encoded here, and the answer decoded, with the `.proto` files of that release
//! (`tests/fixtures/v0.1.221/proto`, copied unchanged from the tag), not with this crate's: the
//! order it is sent is one its own messages read whole.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::{Location, Target};
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, MessageDescriptor, ReflectMessage as _, Value,
};
use serde_json::json;
use support::OPERATOR;
use support::beats::{grpc_beat_bytes, reason};
use support::rig::{Rig, Spec};
use support::targets::{SET_DESIRED, digest, entry, grpc, list, put_komira, repository_entry};

/// The features a daemon of v0.1.221 announces, in its order (`hostkeeper-link`'s `ANNOUNCED`
/// at that tag).
const FEATURES: [&str; 11] = [
    "deferred-v1",
    "halt-v1",
    "heartbeat-v2",
    "forced-v1",
    "order-v1",
    "verdict-acks-v1",
    "grpc-v1",
    "install-v1",
    "abort-v1",
    "clear-record-v1",
    "target-v1",
];

/// The release the farm runs, and the one it moves to.
const RUNS: &str = "0.1.221";
const NEXT: &str = "0.1.242";

/// A time past the coordinator's quiet start.
const LATER: u64 = 2_000;

/// The messages of v0.1.221's `hostkeeper.agent.v1` and `hostkeeper.release.v1`.
fn old_pool() -> DescriptorPool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0.1.221/proto");
    let mut compiler = protox::Compiler::new([root]).expect("the include root");
    compiler.include_imports(true);
    compiler
        .open_file("hostkeeper/agent/v1/agent.proto")
        .expect("v0.1.221's agent.proto compiles");
    DescriptorPool::decode(compiler.encode_file_descriptor_set().as_slice()).expect("a pool")
}

fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name).expect("the message")
}

/// The heartbeat a daemon of v0.1.221 on a host of `platform` sends over gRPC, listing
/// `installers`, while it runs `hostkeeper` at [`RUNS`] from archives (no installer named):
/// encoded by v0.1.221's `HeartbeatRequest`.
fn old_heartbeat(pool: &DescriptorPool, platform: &str, installers: &[&str]) -> Vec<u8> {
    let text = json!({
        "platform": platform,
        "agent": {"name": "hostkeeper", "version": RUNS},
        "features": FEATURES,
        "installers": installers,
        "services": [{"name": "hostkeeper", "version": RUNS, "health": "HEALTH_HEALTHY"}],
    });
    let descriptor = message(pool, "hostkeeper.agent.v1.HeartbeatRequest");
    DynamicMessage::deserialize(descriptor, text)
        .expect("v0.1.221's heartbeat")
        .encode_to_vec()
}

/// The fields of `found`, at any depth, that v0.1.221's messages do not know, by the message
/// that holds each and its number.
fn unknown(found: &DynamicMessage) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = found
        .unknown_fields()
        .map(|field| (found.descriptor().full_name().to_owned(), field.number()))
        .collect();
    for (_, value) in found.fields() {
        let nested: Vec<&DynamicMessage> = match value {
            Value::Message(one) => vec![one],
            Value::List(items) => items.iter().filter_map(Value::as_message).collect(),
            Value::Map(entries) => entries.values().filter_map(Value::as_message).collect(),
            _ => Vec::new(),
        };
        out.extend(nested.into_iter().flat_map(unknown));
    }
    out
}

/// hostkeeper's entry of [`NEXT`] on Linux as `cargo xtask release targets` writes it: its
/// `.deb` in the repository entry `komira-testing`, its unit, no hook command (its daemon runs
/// built-in steps) and the floor of the oldest daemon that reads the release.
fn own_deb() -> Target {
    let mut own = repository_entry("hostkeeper", &format!("{NEXT}-1"), 'a');
    let manifest = own.manifest.as_mut().expect("a manifest");
    manifest.run = vec!["hostkeeper.service".to_owned()];
    manifest.min_hostkeeper = "0.1.194".to_owned();
    manifest.hook.as_mut().expect("a hook").command.clear();
    own
}

/// hostkeeper's entry of [`NEXT`] on macOS: its signed `.pkg`.
fn own_pkg() -> Target {
    let mut pkg = entry("hostkeeper", "macos-pkg", 'd', 52_000_000);
    pkg.locations = vec![Location {
        store: "komira-testing".to_owned(),
        locator: format!("hostkeeper/{NEXT}/hostkeeper.pkg"),
    }];
    let manifest = pkg.manifest.as_mut().expect("a manifest");
    manifest.package = Some("dev.hostkeeper.hostkeeper".to_owned());
    manifest.run = Vec::new();
    pkg
}

/// A coordinator of `hostkeeper`, set at [`NEXT`] with its native entries alone, past its quiet
/// start.
async fn rig(name: &str) -> Rig {
    let services = "[[service]]\nname = \"hostkeeper\"\nroles = [\"daemon\"]\nstages = [100]\n";
    let rig = Rig::new(
        name,
        Spec::recorded().with_services(services).without_legacy(),
    )
    .routed();
    put_komira(&rig).await;
    let mut request = support::targets::set(
        "hostkeeper",
        NEXT,
        BTreeMap::from([
            ("linux-x86_64".to_owned(), list(vec![own_deb()])),
            ("macos-arm64".to_owned(), list(vec![own_pkg()])),
        ]),
    );
    request
        .desired
        .as_mut()
        .expect("a desired state")
        .max_updating = [("daemon".to_owned(), 16)].into_iter().collect();
    let answered = grpc(&rig, SET_DESIRED, Some(OPERATOR), &request).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(LATER);
    rig
}

/// Proves: a Linux daemon of v0.1.221 that runs `hostkeeper` from archives and lists
/// `[hostkeeper-archive, apt]`, its heartbeat encoded by that release's messages, is answered an
/// `update` of `hostkeeper` to the next version that v0.1.221's `HeartbeatResponse` reads with
/// no field it does not know, at any depth: the order's id and try, the package digest, and the
/// `apt` entry field for field as it was set (its repository, package version and manifest), whose
/// digest is the package digest and whose installer is one the daemon listed, which are what
/// that daemon's receipt check of an entry asks of the coordinator.
/// Catches: the farm's hosts held `installer_dropped` instead of moved, or an order whose shape a
/// daemon of v0.1.221 cannot read whole or refuses on receipt (`order_invalid` for a digest that
/// is not the entry's, `installer_not_listed`, `coordinator_too_old` for an entry with no
/// repository).
#[tokio::test]
async fn a_v0_1_221_linux_daemon_running_hostkeeper_from_archives_is_ordered_its_deb() {
    let rig = rig("v0-1-221-linux").await;
    let pool = old_pool();
    let request = old_heartbeat(
        &pool,
        "PLATFORM_LINUX_X86_64",
        &["hostkeeper-archive", "apt"],
    );
    let bytes = grpc_beat_bytes(&rig, "node-1", "daemon", &request).await;
    let response = DynamicMessage::decode(
        message(&pool, "hostkeeper.agent.v1.HeartbeatResponse"),
        &bytes[..],
    )
    .expect("v0.1.221 reads the answer");
    assert_eq!(unknown(&response), Vec::<(String, u32)>::new());

    let order = response.get_field_by_name("order").expect("an order");
    let order = order.as_message().expect("a message");
    assert!(
        !order
            .get_field_by_name("order_id")
            .expect("an id")
            .as_str()
            .expect("text")
            .is_empty()
    );
    assert!(order.get_field_by_name("try").expect("a try").as_u64() >= Some(1));
    assert!(order.has_field_by_name("update"), "{order:?}");
    let update = order.get_field_by_name("update").expect("an update");
    let update = update.as_message().expect("a message");
    let text = |name: &str| {
        update
            .get_field_by_name(name)
            .expect("the field")
            .as_str()
            .expect("text")
            .to_owned()
    };
    assert_eq!(
        (text("service"), text("version"), text("package_digest")),
        ("hostkeeper".to_owned(), NEXT.to_owned(), digest('a'))
    );
    let target = update.get_field_by_name("target").expect("a target");
    let target = target.as_message().expect("a message");
    let read: Target = target.transcode_to().expect("the entry");
    assert_eq!(read, own_deb(), "the entry as it was set, field for field");
    assert_eq!(read.digest, text("package_digest"));
    assert_eq!(read.installer, "apt");
    assert_eq!(read.repository.as_deref(), Some("komira-testing"));
    assert_eq!(
        reason(&rig, "hostkeeper", "node-1").await,
        proto::GateReason::None,
        "nothing holds the host"
    );
}

/// Proves: a Mac daemon of v0.1.221, which announces `hostkeeper-archive` alone (that release
/// announces no `macos-pkg`), running `hostkeeper` from archives, is answered no order and held
/// `installer_dropped`; a Linux one that lists no `apt` (no repository entry on the host) is held
/// the same way.
/// Catches: a host sent an entry of an installer its daemon does not list, which it would defer
/// `installer_not_listed` at every heartbeat.
#[tokio::test]
async fn a_v0_1_221_daemon_that_lists_no_native_installer_is_ordered_nothing() {
    let rig = rig("v0-1-221-none").await;
    let pool = old_pool();
    for (host, platform) in [
        ("mac-1", "PLATFORM_DARWIN_ARM64"),
        ("node-2", "PLATFORM_LINUX_X86_64"),
    ] {
        let request = old_heartbeat(&pool, platform, &["hostkeeper-archive"]);
        let bytes = grpc_beat_bytes(&rig, host, "daemon", &request).await;
        let response = DynamicMessage::decode(
            message(&pool, "hostkeeper.agent.v1.HeartbeatResponse"),
            &bytes[..],
        )
        .expect("v0.1.221 reads the answer");
        assert!(!response.has_field_by_name("order"), "{host}: {response:?}");
        assert_eq!(
            reason(&rig, "hostkeeper", host).await,
            proto::GateReason::InstallerDropped,
            "{host}"
        );
    }
}
