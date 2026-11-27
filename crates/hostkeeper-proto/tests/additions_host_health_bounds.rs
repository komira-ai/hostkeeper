//! The bounds of host health (`hostkeeper_proto::health`): [`check_health`] takes a `HostHealth`
//! with every list and text at its bound and refuses one with any of them one over, naming it; it
//! refuses a record with no `event_id`, and a message over 4096 bytes encoded whatever its fields
//! hold, taking one of exactly 4096. [`check_service`] does the same for a service's health
//! reason, body and restarts.

use prost::Message as _;

use hostkeeper_proto::health::{
    HealthError, MAX_BOOT_ID, MAX_CODE, MAX_DISK_NAME, MAX_DISKS, MAX_EVENT_ID, MAX_EVENTS,
    MAX_HEALTH_BODY, MAX_HEALTH_BYTES, MAX_HEALTH_REASON, MAX_LAST_RESULT, MAX_LOCATOR,
    MAX_MEMORY_DEVICES, MAX_MESSAGE, MAX_NIC_NAME, MAX_NICS, MAX_RAW, MAX_SMT, MAX_SOURCE,
    MAX_SOURCE_REASON, MAX_SOURCES, MAX_SUBJECT, check_health, check_service,
};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

#[path = "support/host_health.rs"]
mod host_health;
use host_health::{fullest, incident, record_at_bound};

/// A memory device whose locator is `len` bytes, of an empty slot.
fn device(len: usize) -> agent::MemoryDevice {
    agent::MemoryDevice {
        locator: "l".repeat(len),
        size_bytes: 0,
    }
}

/// A source state with every text at its bound.
fn source_at_bound() -> agent::SourceState {
    agent::SourceState {
        source: "s".repeat(MAX_SOURCE),
        status: agent::SourceStatus::Unreadable.into(),
        reason: "r".repeat(MAX_SOURCE_REASON),
    }
}

/// The baseline of `health`.
fn baseline(health: &mut agent::HostHealth) -> &mut agent::Baseline {
    health.baseline.get_or_insert_with(agent::Baseline::default)
}

/// Proves the incident's health and the fullest health the daemon sends keep every bound, and
/// that the fullest, 6 records with every text at its bound, is within 4096 bytes encoded.
#[test]
fn the_incident_and_the_fullest_health_keep_every_bound() {
    assert_eq!(check_health(&incident()), Ok(()));
    let full = fullest();
    assert_eq!(check_health(&full), Ok(()));
    assert!(
        full.encoded_len() <= MAX_HEALTH_BYTES,
        "{}",
        full.encoded_len()
    );
    assert_eq!(check_health(&agent::HostHealth::default()), Ok(()));
}

/// Proves each list at its bound is taken, and one entry over is refused naming the list, its
/// count and its bound. Lists are kept small enough, entry by entry, that the encoded length is
/// not what refuses them.
#[test]
fn each_list_one_over_its_bound_is_refused_by_name() {
    type Grow = fn(&mut agent::HostHealth, usize);
    let lists: [(&str, usize, Grow); 5] = [
        ("events", MAX_EVENTS, |h, n| {
            h.events = (0..n)
                .map(|i| agent::HardwareEvent {
                    event_id: format!("e{i}"),
                    ..agent::HardwareEvent::default()
                })
                .collect();
        }),
        ("baseline.memory_devices", MAX_MEMORY_DEVICES, |h, n| {
            baseline(h).memory_devices = (0..n).map(|_| device(1)).collect();
        }),
        ("baseline.disks", MAX_DISKS, |h, n| {
            baseline(h).disks = vec![agent::BaselineDisk::default(); n];
        }),
        ("baseline.nics", MAX_NICS, |h, n| {
            baseline(h).nics = vec![agent::BaselineNic::default(); n];
        }),
        ("sources", MAX_SOURCES, |h, n| {
            h.sources = vec![agent::SourceState::default(); n];
        }),
    ];
    for (field, max, grow) in lists {
        let mut health = agent::HostHealth::default();
        grow(&mut health, max);
        assert_eq!(check_health(&health), Ok(()), "{field} at its bound");
        grow(&mut health, max + 1);
        assert_eq!(
            check_health(&health),
            Err(HealthError::TooMany {
                field,
                count: max + 1,
                max
            })
        );
    }
}

/// Proves each text one byte over its bound is refused naming the text, its length and its
/// bound, from a health that keeps every other bound; and that a text at its bound is taken (the
/// fullest health holds the record texts, the disk and interface names, `smt` and `boot_id` at
/// theirs; a memory device's locator and a source state's texts are taken here).
#[test]
fn each_text_one_byte_over_its_bound_is_refused_by_name() {
    type Stretch = fn(&mut agent::HostHealth, usize);
    let texts: [(&str, usize, Stretch); 13] = [
        ("events.event_id", MAX_EVENT_ID, |h, n| {
            h.events[0].event_id = "i".repeat(n);
        }),
        ("events.subject", MAX_SUBJECT, |h, n| {
            h.events[0].subject = "s".repeat(n);
        }),
        ("events.source", MAX_SOURCE, |h, n| {
            h.events[0].source = "e".repeat(n);
        }),
        ("events.code", MAX_CODE, |h, n| {
            h.events[0].code = "c".repeat(n)
        }),
        ("events.message", MAX_MESSAGE, |h, n| {
            h.events[0].message = "m".repeat(n);
        }),
        ("events.raw", MAX_RAW, |h, n| {
            h.events[0].raw = "r".repeat(n)
        }),
        ("events.about", MAX_EVENT_ID, |h, n| {
            h.events[0].about = "a".repeat(n);
        }),
        ("baseline.memory_devices.locator", MAX_LOCATOR, |h, n| {
            baseline(h).memory_devices = vec![device(n)];
        }),
        ("baseline.smt", MAX_SMT, |h, n| {
            baseline(h).smt = "t".repeat(n)
        }),
        ("baseline.disks.name", MAX_DISK_NAME, |h, n| {
            baseline(h).disks[0].name = "d".repeat(n);
        }),
        ("baseline.nics.name", MAX_NIC_NAME, |h, n| {
            baseline(h).nics[0].name = "n".repeat(n);
        }),
        ("boot_id", MAX_BOOT_ID, |h, n| h.boot_id = "0".repeat(n)),
        ("sources.reason", MAX_SOURCE_REASON, |h, n| {
            h.sources = vec![agent::SourceState {
                reason: "r".repeat(n),
                ..agent::SourceState::default()
            }];
        }),
    ];
    for (field, max, stretch) in texts {
        let mut health = incident();
        stretch(&mut health, max);
        assert_eq!(check_health(&health), Ok(()), "{field} at its bound");
        stretch(&mut health, max + 1);
        assert_eq!(
            check_health(&health),
            Err(HealthError::TooLong {
                field,
                len: max + 1,
                max
            })
        );
    }
    let mut health = incident();
    health.sources = vec![source_at_bound()];
    assert_eq!(check_health(&health), Ok(()));
    health.sources[0].source.push('s');
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooLong {
            field: "sources.source",
            len: MAX_SOURCE + 1,
            max: MAX_SOURCE
        })
    );
}

/// Catches: a record the coordinator could not key in its log taken: a record with an empty
/// `event_id` is refused, after the texts' bounds; an empty `about` is a record about nothing,
/// and is taken.
#[test]
fn a_record_with_no_event_id_is_refused() {
    let mut health = incident();
    health.events[0].event_id.clear();
    assert_eq!(
        check_health(&health),
        Err(HealthError::Empty {
            field: "events.event_id"
        })
    );
    health.events[0].raw = "r".repeat(MAX_RAW + 1);
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooLong {
            field: "events.raw",
            len: MAX_RAW + 1,
            max: MAX_RAW
        }),
        "the texts are checked first"
    );
    let mut health = incident();
    health.events[0].about.clear();
    assert_eq!(check_health(&health), Ok(()));
}

/// Proves the first bound broken is the one named: a list over its count is named before any
/// text, and the texts in the order of the fields.
#[test]
fn the_first_bound_broken_is_named() {
    let mut health = incident();
    health.events[0].raw = "r".repeat(MAX_RAW + 1);
    health.boot_id = "0".repeat(MAX_BOOT_ID + 1);
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooLong {
            field: "events.raw",
            len: MAX_RAW + 1,
            max: MAX_RAW
        })
    );
    health.sources = vec![agent::SourceState::default(); MAX_SOURCES + 1];
    assert_eq!(
        check_health(&health),
        Err(HealthError::TooMany {
            field: "sources",
            count: MAX_SOURCES + 1,
            max: MAX_SOURCES
        })
    );
}

/// Catches: the 4096 bytes not held when every field keeps its bound (6 records, 32 memory
/// devices and 12 sources at theirs come to more), or held one byte short: a health of exactly
/// 4096 bytes is taken, and one of 4097 refused with its length.
#[test]
fn the_whole_is_held_to_4096_bytes_encoded() {
    let mut every = fullest();
    baseline(&mut every).memory_devices = (0..MAX_MEMORY_DEVICES)
        .map(|_| device(MAX_LOCATOR))
        .collect();
    every.sources = (0..MAX_SOURCES).map(|_| source_at_bound()).collect();
    let len = every.encoded_len();
    assert!(len > MAX_HEALTH_BYTES, "{len}");
    assert_eq!(check_health(&every), Err(HealthError::TooLarge { len }));

    // A memory device of a `k`-byte locator and an empty slot is `k + 4` bytes encoded.
    let mut exact = fullest();
    let devices = &mut baseline(&mut exact).memory_devices;
    let mut left = MAX_HEALTH_BYTES - fullest().encoded_len();
    while left > MAX_LOCATOR + 4 {
        devices.push(device(MAX_LOCATOR));
        left -= MAX_LOCATOR + 4;
    }
    if left < 4 {
        devices.pop();
        devices.push(device(28));
        left += MAX_LOCATOR + 4 - 32;
    }
    devices.push(device(left - 4));
    assert!(devices.len() <= MAX_MEMORY_DEVICES);
    assert_eq!(exact.encoded_len(), MAX_HEALTH_BYTES);
    assert_eq!(check_health(&exact), Ok(()));

    let short = baseline(&mut exact)
        .memory_devices
        .iter_mut()
        .find(|d| d.locator.len() < MAX_LOCATOR)
        .expect("a locator under its bound");
    short.locator.push('l');
    assert_eq!(exact.encoded_len(), MAX_HEALTH_BYTES + 1);
    assert_eq!(
        check_health(&exact),
        Err(HealthError::TooLarge {
            len: MAX_HEALTH_BYTES + 1
        })
    );
}

/// Catches: an error whose text says something other than the bound broken; each is compared
/// whole.
#[test]
fn the_errors_say_which_bound() {
    let cases = [
        (
            HealthError::TooMany {
                field: "events",
                count: 7,
                max: 6,
            },
            "events holds 7 entries, more than 6",
        ),
        (
            HealthError::TooLong {
                field: "events.raw",
                len: 257,
                max: 256,
            },
            "events.raw is 257 bytes, more than 256",
        ),
        (
            HealthError::Empty {
                field: "events.event_id",
            },
            "events.event_id is empty",
        ),
        (
            HealthError::TooLarge { len: 4097 },
            "it encodes to 4097 bytes, more than 4096",
        ),
    ];
    for (error, text) in cases {
        assert_eq!(error.to_string(), text);
    }
}

/// Proves a record at every bound is one of the fullest health's, and that the fullest is what
/// the design counts: 6 such records and a baseline with no memory devices.
#[test]
fn the_fullest_health_is_six_records_at_every_bound() {
    let full = fullest();
    assert_eq!(full.events.len(), MAX_EVENTS);
    assert_eq!(full.events[5], record_at_bound(5));
    assert!(
        full.baseline
            .as_ref()
            .is_some_and(|b| b.memory_devices.is_empty())
    );
}

/// Proves a service with its health reason, body and restarts' last result at their bounds, or
/// with none of them, is taken; each one byte over is refused by name, the reason before the
/// body before the last result.
#[test]
fn a_services_health_facts_keep_their_bounds() {
    let at_bound = agent::ServiceBeat {
        name: "js-agent".to_owned(),
        health_reason: Some("r".repeat(MAX_HEALTH_REASON)),
        health_body: Some("b".repeat(MAX_HEALTH_BODY)),
        restarts: Some(agent::ServiceRestarts {
            last_result: "l".repeat(MAX_LAST_RESULT),
            ..agent::ServiceRestarts::default()
        }),
        ..agent::ServiceBeat::default()
    };
    assert_eq!(check_service(&at_bound), Ok(()));
    assert_eq!(check_service(&agent::ServiceBeat::default()), Ok(()));

    let mut over = at_bound.clone();
    if let Some(restarts) = &mut over.restarts {
        restarts.last_result.push('l');
    }
    assert_eq!(
        check_service(&over),
        Err(HealthError::TooLong {
            field: "restarts.last_result",
            len: MAX_LAST_RESULT + 1,
            max: MAX_LAST_RESULT
        })
    );
    if let Some(body) = &mut over.health_body {
        body.push('b');
    }
    assert_eq!(
        check_service(&over),
        Err(HealthError::TooLong {
            field: "health_body",
            len: MAX_HEALTH_BODY + 1,
            max: MAX_HEALTH_BODY
        })
    );
    if let Some(reason) = &mut over.health_reason {
        reason.push('r');
    }
    assert_eq!(
        check_service(&over),
        Err(HealthError::TooLong {
            field: "health_reason",
            len: MAX_HEALTH_REASON + 1,
            max: MAX_HEALTH_REASON
        })
    );
}
