//! What a filesystem's fullness adds to the wire (docs/design/host-health-alerts.md sections 3.1
//! and 5.1): `HostHealth.filesystems` (7), of the new message `FilesystemUse`. A reader of the
//! release before, which knows `HostHealth` fields 1 to 6, keeps field 7 as one it does not know
//! and writes it back byte for byte; this build reads a health that release's daemon wrote, with
//! no field 7, as one with no filesystems. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::health::{HealthError, MAX_FILESYSTEMS, MAX_MOUNT, check_health};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

#[path = "support/host_health.rs"]
mod host_health;
use host_health::incident;

/// What a reader of the release before knows: this build's descriptors less `HostHealth` field 7
/// and the message `FilesystemUse`.
fn release_before() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes");
    for file in &mut set.file {
        if file.package() != "hostkeeper.agent.v1" {
            continue;
        }
        file.message_type.retain(|m| m.name() != "FilesystemUse");
        for message in &mut file.message_type {
            if message.name() == "HostHealth" {
                message.field.retain(|field| field.number() != 7);
            }
        }
    }
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set")
}

/// The incident's health with two filesystems, one of them 97% used.
fn with_filesystems() -> agent::HostHealth {
    agent::HostHealth {
        filesystems: vec![
            agent::FilesystemUse {
                mount: "/".to_owned(),
                size_bytes: 100 << 30,
                available_bytes: 3 << 30,
            },
            agent::FilesystemUse {
                mount: "/var/lib/js-agent".to_owned(),
                size_bytes: 1 << 40,
                available_bytes: 1 << 39,
            },
        ],
        ..incident()
    }
}

/// Catches: a health with filesystems that a coordinator of the release before refuses, drops,
/// reads its other fields of differently, or writes back changed. Proves the tag on the wire:
/// field 7 of `HostHealth` (`0x3a`), and each field of `FilesystemUse` (`0x0a`, `0x10`, `0x18`).
#[test]
fn an_older_coordinator_keeps_the_filesystems() {
    let older = release_before();
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.FilesystemUse")
            .is_none()
    );
    let health_d = older
        .get_message_by_name("hostkeeper.agent.v1.HostHealth")
        .expect("HostHealth");
    assert!(health_d.get_field_by_name("filesystems").is_none());
    assert!(health_d.get_field_by_name("sources").is_some());

    let health = with_filesystems();
    let bytes = health.encode_to_vec();
    let alone = agent::HostHealth {
        filesystems: vec![agent::FilesystemUse {
            mount: "/".to_owned(),
            size_bytes: 1,
            available_bytes: 2,
        }],
        ..agent::HostHealth::default()
    };
    assert_eq!(
        alone.encode_to_vec(),
        [0x3a, 7, 0x0a, 1, b'/', 0x10, 1, 0x18, 2]
    );

    let read = DynamicMessage::decode(health_d, bytes.as_slice()).expect("decodes");
    let kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    assert_eq!(kept, [7, 7], "both entries kept as unknown");
    assert_eq!(
        read.get_field_by_name("boot_id").as_deref(),
        Some(&Value::String("b7c1".to_owned()))
    );
    let Some(Value::List(sources)) = read.get_field_by_name("sources").as_deref().cloned() else {
        panic!("the sources");
    };
    assert_eq!(sources.len(), 2);
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Catches: a health from a daemon of the release before, which sends no field 7, refused or read
/// as one with filesystems: this build reads it with none and every other field as written.
#[test]
fn this_build_reads_a_health_with_no_filesystems() {
    let older = release_before();
    let health_d = older
        .get_message_by_name("hostkeeper.agent.v1.HostHealth")
        .expect("HostHealth");
    let mut old = DynamicMessage::new(health_d);
    old.set_field_by_name("boot_id", Value::String("b7c1".to_owned()));
    old.set_field_by_name("boot_unix", Value::U64(1_795_662_060));
    let bytes = old.encode_to_vec();

    let read = agent::HostHealth::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(
        read,
        agent::HostHealth {
            boot_id: "b7c1".to_owned(),
            boot_unix: 1_795_662_060,
            ..agent::HostHealth::default()
        }
    );
    assert!(read.filesystems.is_empty());
    assert_eq!(check_health(&read), Ok(()));
}

/// Proves the bounds of the field: 8 filesystems and a 32-byte mount are taken, a ninth entry and
/// a 33rd byte refused by name; and that 8 filesystems at their bound with the incident's health
/// are well within the 4096 bytes.
#[test]
fn the_filesystems_keep_their_bounds() {
    let mut health = incident();
    health.filesystems = (0..MAX_FILESYSTEMS)
        .map(|_| agent::FilesystemUse {
            mount: "m".repeat(MAX_MOUNT),
            size_bytes: u64::MAX,
            available_bytes: u64::MAX,
        })
        .collect();
    assert_eq!(check_health(&health), Ok(()));
    assert!(health.encoded_len() < 1024, "{}", health.encoded_len());
    health.filesystems[0].mount.push('m');
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooLong {
            field: "filesystems.mount",
            len: MAX_MOUNT + 1,
            max: MAX_MOUNT
        })
    );
    health.filesystems.push(agent::FilesystemUse::default());
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooMany {
            field: "filesystems",
            count: MAX_FILESYSTEMS + 1,
            max: MAX_FILESYSTEMS
        })
    );
}
