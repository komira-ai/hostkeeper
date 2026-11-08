//! `hostkeeper.release.v1` (docs/design/packages.md sections 3.1 and 4.2): the messages of a
//! release, new since the committed baseline (`proto/compat/baseline.binpb`). Nothing of them
//! shipped before, so this is their first shape and rule B8 binds it from here on: every message
//! and every field number, type and label is pinned as the design gives it. Each message reads
//! back as written, an absent optional stays absent, a manifest's params encode in one order
//! whatever order they were set in, and a reader built from the baseline, the release before,
//! keeps what it does not know and writes it back unchanged.

use std::collections::BTreeMap;

use prost::Message as _;
use prost_reflect::{Cardinality, DescriptorPool, DynamicMessage, Kind};

use hostkeeper_proto::hostkeeper::release::v1 as release;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// Every message of the file, each field as `name number type label`, in number order, as
/// packages.md sections 3.1 and 4.2 give them. `label` is `repeated`, `optional` (proto3
/// `optional`), `map` or `-`.
const SHAPE: &[(&str, &[&str])] = &[
    ("TargetList", &["entries 1 Target repeated"]),
    (
        "Target",
        &[
            "installer 1 string -",
            "locations 2 Location repeated",
            "digest 3 string -",
            "bytes 4 uint64 -",
            "attestation 5 Blob -",
            "manifest 6 Manifest -",
        ],
    ),
    (
        "Blob",
        &[
            "digest 1 string -",
            "bytes 2 uint64 -",
            "locations 3 Location repeated",
        ],
    ),
    ("Location", &["store 1 string -", "locator 2 string -"]),
    (
        "Manifest",
        &[
            "package 1 string optional",
            "hook 2 Hook -",
            "run 3 string repeated",
            "health_url 4 string optional",
            "place 5 Place repeated",
            "params 6 Param map",
            "min_hostkeeper 7 string -",
            "containers 8 Container repeated",
            "installer_bounds 9 InstallerBounds -",
        ],
    ),
    (
        "Hook",
        &[
            "command 1 string repeated",
            "boot 2 string optional",
            "timeout_secs 3 StepBounds -",
            "verify_deadline_secs 4 uint64 -",
            "promotion_ceiling_secs 5 uint64 optional",
            "cleanup_retries 6 uint32 optional",
        ],
    ),
    (
        "StepBounds",
        &[
            "prepare 1 uint64 -",
            "cutover 2 uint64 -",
            "verify 3 uint64 -",
            "rollback 4 uint64 -",
            "cleanup 5 uint64 -",
            "uninstall 6 uint64 optional",
        ],
    ),
    (
        "InstallerBounds",
        &[
            "stage_secs 1 uint64 -",
            "activate_secs 2 uint64 -",
            "remove_secs 3 uint64 -",
        ],
    ),
    (
        "Place",
        &["kind 1 string -", "source 2 string -", "path 3 string -"],
    ),
    (
        "Param",
        &[
            "text 1 string -",
            "integer 2 int64 -",
            "float 3 double -",
            "flag 4 bool -",
        ],
    ),
    (
        "Container",
        &[
            "unit 1 string -",
            "args 2 string repeated",
            "ports 3 Port repeated",
            "volumes 4 Volume repeated",
        ],
    ),
    (
        "Port",
        &[
            "host 1 uint32 -",
            "container 2 uint32 -",
            "loopback_only 3 bool -",
        ],
    ),
    (
        "Volume",
        &[
            "host_path 1 string -",
            "container_path 2 string -",
            "read_only 3 bool -",
        ],
    ),
];

fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::String => "string".to_owned(),
        Kind::Uint64 => "uint64".to_owned(),
        Kind::Uint32 => "uint32".to_owned(),
        Kind::Int64 => "int64".to_owned(),
        Kind::Double => "double".to_owned(),
        Kind::Bool => "bool".to_owned(),
        Kind::Message(m) => m.name().to_owned(),
        other => format!("{other:?}"),
    }
}

/// Catches: a message missing, renamed or present in the release before, a field renumbered,
/// renamed, retyped or relabelled against the design, or a field added that the design does not
/// name.
#[test]
fn the_release_messages_are_new_and_have_exactly_the_designs_fields() {
    let older = baseline_pool();
    let current = current_pool();
    let file = current
        .get_file_by_name("hostkeeper/release/v1/release.proto")
        .expect("the file is compiled");
    assert_eq!(file.package_name(), "hostkeeper.release.v1");
    assert_eq!(file.services().count(), 0, "it declares no service");
    let declared: Vec<String> = file.messages().map(|m| m.name().to_owned()).collect();
    let designed: Vec<&str> = SHAPE.iter().map(|(name, _)| *name).collect();
    assert_eq!(declared, designed);
    assert!(
        older
            .get_file_by_name("hostkeeper/release/v1/release.proto")
            .is_none(),
        "the file is new"
    );
    for (name, fields) in SHAPE {
        let full = format!("hostkeeper.release.v1.{name}");
        assert!(older.get_message_by_name(&full).is_none(), "{full} is new");
        let message = current.get_message_by_name(&full).expect("declared");
        let mut got: Vec<(u32, String)> = message
            .fields()
            .map(|f| {
                let label = if f.is_map() {
                    "map"
                } else if f.cardinality() == Cardinality::Repeated {
                    "repeated"
                } else if f.field_descriptor_proto().proto3_optional() {
                    "optional"
                } else {
                    "-"
                };
                let kind = match (f.is_map(), f.kind()) {
                    (true, Kind::Message(entry)) => {
                        kind_name(&entry.map_entry_value_field().kind())
                    }
                    (_, kind) => kind_name(&kind),
                };
                (
                    f.number(),
                    format!("{} {} {kind} {label}", f.name(), f.number()),
                )
            })
            .collect();
        got.sort();
        let got: Vec<String> = got.into_iter().map(|(_, text)| text).collect();
        assert_eq!(got, *fields, "{full}");
    }
    let params = current
        .get_message_by_name("hostkeeper.release.v1.Manifest")
        .and_then(|m| m.get_field_by_name("params"))
        .expect("params");
    let Kind::Message(entry) = params.kind() else {
        panic!("a map is a message of entries");
    };
    assert_eq!(kind_name(&entry.map_entry_key_field().kind()), "string");
    let param = current
        .get_message_by_name("hostkeeper.release.v1.Param")
        .expect("Param");
    let oneof: Vec<String> = param.oneofs().map(|o| o.name().to_owned()).collect();
    assert_eq!(oneof, ["value"], "a param's four fields are one oneof");
    assert_eq!(param.oneofs().next().expect("value").fields().count(), 4);
}

/// A manifest with every field set.
fn manifest() -> release::Manifest {
    release::Manifest {
        package: Some("tierstore".to_owned()),
        hook: Some(release::Hook {
            command: vec!["bin/tierstore".to_owned(), "update-hook".to_owned()],
            boot: Some("restore".to_owned()),
            timeout_secs: Some(release::StepBounds {
                prepare: 1,
                cutover: 300,
                verify: 1000,
                rollback: 600,
                cleanup: 1300,
                uninstall: Some(300),
            }),
            verify_deadline_secs: 1600,
            promotion_ceiling_secs: Some(3600),
            cleanup_retries: Some(3),
        }),
        run: vec!["tierstore@9301.service".to_owned()],
        health_url: Some("http://127.0.0.1:{port}/healthz".to_owned()),
        place: vec![release::Place {
            kind: "file".to_owned(),
            source: "files/tierstore.conf".to_owned(),
            path: "/etc/tierstore/tierstore.conf".to_owned(),
        }],
        params: BTreeMap::from([
            (
                "a".to_owned(),
                param(release::param::Value::Text("x".to_owned())),
            ),
            ("b".to_owned(), param(release::param::Value::Integer(-7))),
            ("c".to_owned(), param(release::param::Value::Float(0.25))),
            ("d".to_owned(), param(release::param::Value::Flag(true))),
            ("e".to_owned(), release::Param { value: None }),
        ]),
        min_hostkeeper: "0.4.0".to_owned(),
        containers: vec![release::Container {
            unit: "tierstore-api.service".to_owned(),
            args: vec!["--port".to_owned(), "8080".to_owned()],
            ports: vec![release::Port {
                host: 8080,
                container: 80,
                loopback_only: true,
            }],
            volumes: vec![release::Volume {
                host_path: "/var/lib/tierstore/data".to_owned(),
                container_path: "/data".to_owned(),
                read_only: true,
            }],
        }],
        installer_bounds: Some(release::InstallerBounds {
            stage_secs: 60,
            activate_secs: 120,
            remove_secs: 60,
        }),
    }
}

fn param(value: release::param::Value) -> release::Param {
    release::Param { value: Some(value) }
}

fn location(store: &str, locator: &str) -> release::Location {
    release::Location {
        store: store.to_owned(),
        locator: locator.to_owned(),
    }
}

/// An entry with every field set.
fn target() -> release::Target {
    release::Target {
        installer: "deb".to_owned(),
        locations: vec![
            location("lan", "releases/tierstore/1.6.0/tierstore.deb"),
            location(
                "github",
                "example-org/tierstore/releases/download/v1.6.0/tierstore.deb",
            ),
        ],
        digest: "ab".repeat(32),
        bytes: 48_812_032,
        attestation: Some(release::Blob {
            digest: "cd".repeat(32),
            bytes: 7311,
            locations: vec![location(
                "github",
                "example-org/tierstore/releases/download/v1.6.0/tierstore.deb.sigstore.json",
            )],
        }),
        manifest: Some(manifest()),
    }
}

/// The full name of a message of the package. Built, not written out: a name such as the
/// package's and `Target` reads to the hygiene lint as a host.
fn full(name: &str) -> String {
    format!("hostkeeper.release.v1.{name}")
}

/// One message of each type, every field set, by full name.
fn release_messages() -> Vec<(String, Vec<u8>)> {
    let entry = target();
    let manifest = manifest();
    let hook = manifest.hook.clone().expect("a hook");
    let container = manifest.containers[0].clone();
    vec![
        (
            full("TargetList"),
            round_trip(&release::TargetList {
                entries: vec![
                    entry.clone(),
                    release::Target {
                        installer: "tarball".to_owned(),
                        ..entry.clone()
                    },
                ],
            }),
        ),
        (full("Target"), round_trip(&entry)),
        (
            full("Blob"),
            round_trip(&entry.attestation.clone().expect("a bundle")),
        ),
        (full("Location"), round_trip(&entry.locations[0])),
        (full("Manifest"), round_trip(&manifest)),
        (full("Hook"), round_trip(&hook)),
        (
            full("StepBounds"),
            round_trip(&hook.timeout_secs.expect("bounds")),
        ),
        (
            full("InstallerBounds"),
            round_trip(&manifest.installer_bounds.expect("bounds")),
        ),
        (full("Place"), round_trip(&manifest.place[0])),
        (
            full("Param"),
            round_trip(&param(release::param::Value::Float(-1.5))),
        ),
        (full("Container"), round_trip(&container)),
        (full("Port"), round_trip(&container.ports[0])),
        (full("Volume"), round_trip(&container.volumes[0])),
    ]
}

/// Catches: a release message that does not read back as written, or that a reader of the
/// release before drops: it knows none of them, and a message it reads with no field keeps every
/// field of the new ones unknown and writes them back unchanged.
#[test]
fn the_release_messages_round_trip_and_an_older_reader_keeps_unknown_fields() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    let messages = release_messages();
    assert_eq!(messages.len(), SHAPE.len(), "one of each message");
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(&name).is_none(), "{name} is new");
        assert!(!bytes.is_empty(), "{name} sets its fields");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
}

/// Catches: an optional of a manifest that reads as empty or zero when it was not given, so
/// that "no package", "no health URL", "no ceiling" and "no uninstall step" could not be told
/// from an empty or zero one; or a param whose oneof is unset read as a value.
#[test]
fn an_absent_optional_of_a_manifest_stays_absent_and_a_zero_is_written() {
    let bare = release::Manifest {
        min_hostkeeper: "0.4.0".to_owned(),
        hook: Some(release::Hook {
            timeout_secs: Some(release::StepBounds::default()),
            ..release::Hook::default()
        }),
        ..release::Manifest::default()
    };
    let read = release::Manifest::decode(round_trip(&bare).as_slice()).expect("decodes");
    let hook = read.hook.clone().expect("a hook");
    assert_eq!(
        (
            read.package.clone(),
            read.health_url.clone(),
            hook.boot.clone(),
            hook.promotion_ceiling_secs,
            hook.cleanup_retries,
            hook.timeout_secs.expect("bounds").uninstall,
            read.installer_bounds,
        ),
        (None, None, None, None, None, None, None)
    );
    let zero = release::Manifest {
        package: Some(String::new()),
        health_url: Some(String::new()),
        hook: Some(release::Hook {
            boot: Some(String::new()),
            promotion_ceiling_secs: Some(0),
            cleanup_retries: Some(0),
            timeout_secs: Some(release::StepBounds {
                uninstall: Some(0),
                ..release::StepBounds::default()
            }),
            ..release::Hook::default()
        }),
        ..bare.clone()
    };
    let bytes = round_trip(&zero);
    assert!(bytes.len() > round_trip(&bare).len());
    assert_eq!(
        release::Manifest::decode(bytes.as_slice()).expect("decodes"),
        zero
    );

    let unset = release::Param { value: None };
    assert!(round_trip(&unset).is_empty());
    for value in [
        release::param::Value::Text(String::new()),
        release::param::Value::Integer(0),
        release::param::Value::Float(0.0),
        release::param::Value::Flag(false),
    ] {
        let set = param(value);
        assert!(!round_trip(&set).is_empty(), "{set:?} is written");
    }
}

/// Catches: a manifest whose encoding depends on the order its params were set in, so that one
/// manifest would have two digests: the audit entry of a set records the SHA-256 of the
/// encoding.
#[test]
fn a_manifests_params_encode_in_key_order_whatever_order_they_were_set_in() {
    let keys = ["zeta", "alpha", "mid", "beta", "omega", "gamma", "delta"];
    let mut forward = manifest();
    forward.params.clear();
    let mut backward = forward.clone();
    for key in keys {
        forward.params.insert(
            key.to_owned(),
            param(release::param::Value::Text(key.to_owned())),
        );
    }
    for key in keys.iter().rev() {
        backward.params.insert(
            (*key).to_owned(),
            param(release::param::Value::Text((*key).to_owned())),
        );
    }
    assert_eq!(forward.encode_to_vec(), backward.encode_to_vec());
    let read = release::Manifest::decode(forward.encode_to_vec().as_slice()).expect("decodes");
    let order: Vec<&str> = read.params.keys().map(String::as_str).collect();
    assert_eq!(
        order,
        ["alpha", "beta", "delta", "gamma", "mid", "omega", "zeta"]
    );
}

/// Catches: the field numbers of an entry and a manifest drifting from the design, read off the
/// wire rather than the descriptor: each field alone encodes to its tag.
#[test]
fn the_tags_of_an_entry_and_a_manifest_are_pinned_on_the_wire() {
    let installer = release::Target {
        installer: "deb".to_owned(),
        ..release::Target::default()
    };
    assert_eq!(installer.encode_to_vec(), [0x0a, 3, b'd', b'e', b'b']);
    let bytes = release::Target {
        bytes: 1,
        ..release::Target::default()
    };
    assert_eq!(bytes.encode_to_vec(), [0x20, 1]);
    let manifest = release::Target {
        manifest: Some(release::Manifest::default()),
        ..release::Target::default()
    };
    assert_eq!(manifest.encode_to_vec(), [0x32, 0]);
    let bounds = release::Manifest {
        installer_bounds: Some(release::InstallerBounds::default()),
        ..release::Manifest::default()
    };
    assert_eq!(bounds.encode_to_vec(), [0x4a, 0]);
    let floor = release::Manifest {
        min_hostkeeper: "1".to_owned(),
        ..release::Manifest::default()
    };
    assert_eq!(floor.encode_to_vec(), [0x3a, 1, b'1']);
}
