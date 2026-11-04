//! The loop's one outbound call.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_link::Feature;

use crate::{Rig, recorded, reply, update};

/// What a source of this crate would need to reach the outside without a port: a socket, a
/// process, the wall clock.
const OUTSIDE: [&str; 8] = [
    "std::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "UnixStream",
    "std::process",
    "Command::new",
    "SystemTime",
];

/// Catches a second way out of the loop: a source of the crate that opens a socket, starts a
/// process or reads the wall clock, or a dependency that brings a network client. A full round
/// (an order, its verdict, a halt generation, a deferral) calls the link once per heartbeat and
/// nothing else.
#[test]
fn the_loop_dials_only_the_coordinator_link_port() {
    let rig = Rig::new("ports-only-link");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 1, "x", "2.0")),
        &[Feature::DeferredV1, Feature::HaltV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert_eq!(rig.world.link.calls().len(), 2);
    assert_eq!(rig.world.orders.orders().len(), 1);

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("src")).expect("the sources") {
        let path = entry.expect("an entry").path();
        let text = std::fs::read_to_string(&path).expect("a source");
        for outside in OUTSIDE {
            assert!(!text.contains(outside), "{} uses {outside}", path.display());
        }
        sources.insert(
            path.file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let expected = [
        "beat.rs",
        "deferral.rs",
        "heartbeater.rs",
        "lib.rs",
        "names.rs",
        "order.rs",
        "ports.rs",
        "testing.rs",
    ];
    assert_eq!(
        sources,
        expected
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<String>>()
    );

    let text = std::fs::read_to_string(root.join("Cargo.toml")).expect("the manifest");
    let manifest: toml::Table = toml::from_str(&text).expect("TOML");
    let dependencies: BTreeSet<&str> = manifest["dependencies"]
        .as_table()
        .expect("a table")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        dependencies,
        BTreeSet::from([
            "clap",
            "hostkeeper-config",
            "hostkeeper-hook",
            "hostkeeper-proto",
            "hostkeeper-steps",
            "libc",
            "serde",
            "serde_json",
            "thiserror",
        ])
    );
}
