//! The loop's one outbound call.

use std::collections::BTreeSet;
use std::path::Path;

use hostkeeper_link::Feature;

use crate::{Rig, recorded, reply, update};

/// The names a source of this crate would need to reach the outside without a port: the
/// standard library's network and process modules, their types and the DNS lookup, and the wall
/// clock. Matched as whole identifiers, so a grouped import or an alias cannot hide them.
const OUTSIDE: [&str; 13] = [
    "net",
    "process",
    "ToSocketAddrs",
    "to_socket_addrs",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "UnixStream",
    "UnixListener",
    "UnixDatagram",
    "Command",
    "SystemTime",
    "UNIX_EPOCH",
];

/// The one use of a process module the crate makes: the effective uid, for the owner check of
/// a step-record directory. It starts and signals nothing.
const OWNER_CHECK: &str = "rustix::process::geteuid";

/// The one source that reaches the network, the transports of the links, and the names of
/// [`OUTSIDE`] it uses: a TCP connection to the coordinator's address, and nothing else.
const TRANSPORT: (&str, [&str; 4]) = (
    "dial.rs",
    ["net", "TcpStream", "ToSocketAddrs", "to_socket_addrs"],
);

/// The names of [`OUTSIDE`] the code of `source` uses, in order of first use: every identifier
/// outside `//` comments, as a whole word, but for [`OWNER_CHECK`].
fn outside_names(source: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in source.lines() {
        let code = line
            .split("//")
            .next()
            .unwrap_or_default()
            .replace(OWNER_CHECK, "");
        for word in code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if OUTSIDE.contains(&word) && !found.iter().any(|seen| seen == word) {
                found.push(word.to_owned());
            }
        }
    }
    found
}

/// Catches a check that a grouped import, an alias or a method call slips past, and one that
/// refuses a source using none of the names (a comment about a network, a type named `Network`).
#[test]
fn the_outside_check_refuses_each_way_in_and_passes_a_clean_source() {
    let cases: [(&str, &[&str]); 8] = [
        ("let me = rustix::process::geteuid();", &[]),
        (
            "use rustix::process;\nlet _ = process::getpid();",
            &["process"],
        ),
        (
            "use std::{net::ToSocketAddrs};\nfn f(a: &str) { let _ = a.to_socket_addrs(); }",
            &["net", "ToSocketAddrs", "to_socket_addrs"],
        ),
        (
            "use std::net as n;\nfn f() -> n::Shutdown { todo!() }",
            &["net"],
        ),
        (
            "use std::process::Command as Run;\nfn f() { Run::new(\"x\"); }",
            &["process", "Command"],
        ),
        ("let t = std::time::SystemTime::now();", &["SystemTime"]),
        (
            "let s = std::os::unix::net::UnixStream::connect(p);",
            &["net", "UnixStream"],
        ),
        (
            "/// A network link of a process.\npub trait Link { fn heartbeat(&self) -> Network; }",
            &[],
        ),
    ];
    for (source, names) in cases {
        assert_eq!(outside_names(source), names, "{source}");
    }
}

/// Catches a second way out of the loop: a source of the crate that opens a socket, starts a
/// process or reads the wall clock, but for the link's transport, which opens a TCP connection
/// and nothing else; or a dependency that brings a network client (the link's TLS is rustls,
/// over that transport). A full round (an order, its verdict, a halt generation, a deferral)
/// calls the link once per heartbeat and nothing else.
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
        let name = path
            .file_name()
            .expect("a file name")
            .to_string_lossy()
            .into_owned();
        let allowed: &[&str] = if name == TRANSPORT.0 {
            &TRANSPORT.1
        } else {
            &[]
        };
        assert_eq!(outside_names(&text), allowed, "{}", path.display());
        sources.insert(name);
    }
    let expected = [
        "beat.rs",
        "deferral.rs",
        "dial.rs",
        "grpc.rs",
        "heartbeater.rs",
        "host_facts.rs",
        "lib.rs",
        "mtls.rs",
        "names.rs",
        "negotiated.rs",
        "order.rs",
        "pki.rs",
        "ports.rs",
        "proto_map.rs",
        "proto_map_facts_tests.rs",
        "proto_map_tests.rs",
        "repositories.rs",
        "testing.rs",
        "wire_map.rs",
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
            "hostkeeper-wire",
            "hyper-util",
            "p256",
            "prost",
            "rustix",
            "rustls",
            "serde",
            "serde_json",
            "sha2",
            "thiserror",
            "tokio",
            "tokio-rustls",
            "tonic",
            "tower",
            "x509-cert",
        ])
    );
}
