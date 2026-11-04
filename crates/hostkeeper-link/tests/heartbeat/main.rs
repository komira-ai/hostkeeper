//! The heartbeat loop, over the fakes of `hostkeeper_link::testing`: no network, no wall clock,
//! no thread but the one test of the thread dispatcher. Each module is one subject of the loop.

mod certs;
mod deferral;
mod facts;
mod features;
mod halts;
mod orders;
mod ports;
mod resend;
mod size;
mod types;

use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_link::testing::World;
use hostkeeper_link::{
    Feature, Heartbeater, Order, OrderId, OrderRef, PackageDigest, RecordedService, Reply, Target,
    Verb,
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

/// A service whose record says it runs `at`, with no update running and no verdict kept.
pub fn recorded(name: &str, at: &str) -> RecordedService {
    RecordedService {
        name: service(name),
        version: version(at),
        updating: None,
        last_result: None,
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

    /// Installs `services`: their records, and their step-record directories.
    pub fn install(&self, services: Vec<RecordedService>) {
        for recorded in &services {
            std::fs::create_dir_all(self.service_dir(recorded.name.as_str()))
                .expect("a step-record directory");
        }
        self.world.records.set(services);
    }

    /// A loop over the rig, for a daemon of version 0.9.0.
    pub fn heartbeater(&self) -> Heartbeater {
        self.world.heartbeater(self.state(), version("0.9.0"))
    }
}
