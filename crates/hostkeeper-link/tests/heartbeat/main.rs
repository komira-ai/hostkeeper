//! The heartbeat loop, over the fakes of `hostkeeper_link::testing`, and its links: no wall
//! clock, and no network but the loopback sockets of `dial` and of the one test of `TcpDial` in
//! `grpc`.
//! Each module is one subject of the loop or of its links. The JSON link's tests run real TLS over
//! in-process pipes, with a server thread on the other end (`tls`); `coordinator` puts the real
//! coordinator's handler behind it; `grpc` runs the gRPC link against the coordinator's listener
//! code over in-memory streams; `dial` is the test of the blocking transport's socket, on the
//! loopback address with a port the system chose; `waits` plays the loop's waits from a script.

mod certs;
mod clear_record;
mod coordinator;
mod deferral;
mod dial;
mod facts;
mod features;
mod grpc;
mod grpc_reply;
mod halts;
mod host_facts;
mod kbf;
mod link;
mod orders;
mod pki;
mod ports;
mod repositories;
mod resend;
mod size;
mod tls;
mod typed_orders;
mod types;
mod unlisted;
mod waits;
mod wire;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_link::testing::World;
use hostkeeper_link::{
    Feature, Heartbeater, Order, OrderId, OrderRef, OrderResult, Outcome, PackageDigest,
    RecordedService, Reply, Target, Verb,
};
use hostkeeper_testkit::Scratch;

pub fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

pub fn version(text: &str) -> Version {
    Version::new(text).expect("a version")
}

pub fn digest() -> PackageDigest {
    PackageDigest::new(&"0a".repeat(32)).expect("a digest")
}

pub fn order_id(id: &str) -> OrderId {
    OrderId::new(id).expect("an order id")
}

/// The verdict of `order`, which names a version, with `outcome`, naming that version.
pub fn verdict(order: &Order, outcome: Outcome) -> OrderResult {
    let target = order.target().expect("an order that names a version");
    hostkeeper_link::testing::verdict(order, &target.version, outcome)
}

pub fn reference(id: &str, order_try: u64) -> OrderRef {
    OrderRef {
        id: order_id(id),
        order_try,
    }
}

fn target(name: &str, to: &str) -> Target {
    Target {
        service: service(name),
        version: version(to),
        digest: digest(),
        index_digest: None,
        entry: None,
    }
}

/// `update <name> <to>`, order `id`, try `order_try`.
pub fn update(id: &str, order_try: u64, name: &str, to: &str) -> Order {
    Order {
        id: order_id(id),
        order_try,
        verb: Verb::Update(target(name, to)),
    }
}

/// The forced update of `name` to `to`.
pub fn forced(id: &str, order_try: u64, name: &str, to: &str) -> Order {
    Order {
        id: order_id(id),
        order_try,
        verb: Verb::Force(target(name, to)),
    }
}

/// The install of `name` at `to`.
pub fn install(id: &str, order_try: u64, name: &str, to: &str) -> Order {
    Order {
        id: order_id(id),
        order_try,
        verb: Verb::Install(target(name, to)),
    }
}

/// The uninstall of `name`.
pub fn uninstall(id: &str, order_try: u64, name: &str) -> Order {
    Order {
        id: order_id(id),
        order_try,
        verb: Verb::Uninstall(service(name)),
    }
}

/// The abort of what runs for `name`.
pub fn abort(id: &str, name: &str) -> Order {
    Order {
        id: order_id(id),
        order_try: 1,
        verb: Verb::Abort(service(name)),
    }
}

/// The `clear_record` of `name`'s step record, order `id`.
pub fn clear_record(id: &str, name: &str) -> Order {
    Order {
        id: order_id(id),
        order_try: 1,
        verb: Verb::ClearRecord(service(name)),
    }
}

/// A service whose record says it runs `at`, with no update running and no verdict kept.
pub fn recorded(name: &str, at: &str) -> RecordedService {
    RecordedService {
        name: service(name),
        version: version(at),
        updating: None,
        last_result: None,
        record_try: None,
        installer: None,
    }
}

/// A reply with `order` and `features`, and nothing else.
pub fn reply(order: Option<Order>, features: &[Feature]) -> Reply {
    Reply {
        order,
        features: features.to_vec(),
        ..Reply::default()
    }
}

/// A world of fakes and the state directory of its daemon, with one step-record directory per
/// installed service.
pub struct Rig {
    pub world: World,
    scratch: Scratch,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        Rig::with_world(name, World::default())
    }

    pub fn with_world(name: &str, world: World) -> Rig {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = Scratch::under(base, name).expect("a scratch directory");
        Rig { world, scratch }
    }

    /// The state directory.
    pub fn state(&self) -> &Path {
        self.scratch.path()
    }

    /// The step-record directory of `name`.
    pub fn service_dir(&self, name: &str) -> PathBuf {
        self.state().join(name)
    }

    /// The step-record directory of `name`, as the daemon makes it: owner only.
    pub fn make_service_dir(&self, name: &str) {
        let dir = self.service_dir(name);
        std::fs::create_dir_all(&dir).expect("a step-record directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    /// Installs `services`: their records, and their step-record directories.
    pub fn install(&self, services: Vec<RecordedService>) {
        for recorded in &services {
            self.make_service_dir(recorded.name.as_str());
        }
        self.world.records.set(services);
    }

    /// A loop over the rig, for a daemon of version 0.9.0.
    pub fn heartbeater(&self) -> Heartbeater {
        self.world.heartbeater(self.state(), version("0.9.0"))
    }
}
