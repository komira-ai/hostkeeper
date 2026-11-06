//! The clock: a stamp from another boot, or from an unknown one, measures nothing; the boot id
//! is read through its port and is never empty; and the host's clock answers.

mod support;

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::sync::Mutex;

use hostkeeper_steps::{BootId, BootIdError, BootIdSource, Clock, ProcBootId, Stamp, SystemClock};
use support::{FakeClock, Scratch};

const A: &str = "0e1c6a3f-5b7d-4c11-9a2e-6f0d3b8c4a71";
const B: &str = "7d2f9b04-1e6a-4f3c-8b5d-2a9c0e7f1b36";

/// A boot-id port that answers from a script, one answer per read: an id, or (`None`) an error.
struct Script(Mutex<VecDeque<Option<&'static str>>>);

impl Script {
    fn clock(answers: &[Option<&'static str>]) -> SystemClock {
        SystemClock::new(Box::new(Script(Mutex::new(
            answers.iter().copied().collect(),
        ))))
    }
}

impl BootIdSource for Script {
    fn read(&self) -> Result<BootId, BootIdError> {
        let answer = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted answer");
        match answer {
            Some(id) => BootId::uuid("the script", id),
            None => Err(BootIdError::Read {
                from: "the script",
                source: io::Error::other("no boot id"),
            }),
        }
    }
}

fn id(text: &str) -> Option<BootId> {
    BootId::new("a test", text).ok()
}

/// Catches elapsed time carried across a reboot: the monotonic clock restarts, so a stamp of
/// another boot measures no time rather than a difference of unrelated counters.
#[test]
fn a_stamp_of_another_boot_has_no_elapsed_time() {
    let clock = FakeClock::new();
    let stamp = Stamp::now(&*clock);
    clock.advance(40);
    assert_eq!(stamp.elapsed_secs(&*clock), Some(40));
    clock.reboot("boot-b");
    clock.advance(1000);
    assert_eq!(stamp.elapsed_secs(&*clock), None);
}

/// Catches a power cut read as no reboot: the port names boot A when the stamp is taken and
/// boot B at the next read, and the host's clock then reports no elapsed time at all, whatever
/// its monotonic counter says; under the same boot it does report one.
#[test]
fn a_stamp_read_under_another_boot_id_of_the_port_reports_no_elapsed_time() {
    let clock = Script::clock(&[Some(A), Some(B)]);
    let stamp = Stamp::now(&clock);
    assert_eq!(stamp.boot_id, id(A));
    assert_eq!(stamp.elapsed_secs(&clock), None);

    let clock = Script::clock(&[Some(A), Some(A)]);
    let stamp = Stamp::now(&clock);
    assert!(stamp.elapsed_secs(&clock).is_some());
}

/// Catches an unreadable boot id taken as the same boot: a stamp taken while the port fails is
/// of an unknown boot, and a stamp whose boot cannot be read now, or was not readable when it
/// was taken, reports no elapsed time; two stamps of unknown boots are not of the same boot.
#[test]
fn a_boot_id_the_port_cannot_read_is_never_the_same_boot() {
    let clock = Script::clock(&[None, None, Some(A)]);
    let unknown = Stamp::now(&clock);
    assert_eq!(unknown.boot_id, None);
    assert_eq!(unknown.elapsed_secs(&clock), None);
    assert_eq!(unknown.elapsed_secs(&clock), None);

    let clock = Script::clock(&[Some(A), None]);
    let known = Stamp::now(&clock);
    assert_eq!(known.boot_id, id(A));
    assert_eq!(known.elapsed_secs(&clock), None);

    assert!(!unknown.same_boot(&unknown.clone()));
    assert!(!unknown.same_boot(&known));
    assert!(!known.same_boot(&unknown));
    assert!(known.same_boot(&known.clone()));
}

/// Catches an empty or malformed boot id slipping through the Linux reader: the kernel's file
/// with its trailing newline gives the UUID; a missing file, an empty one and one that is not a
/// UUID are each refused with what was read.
#[test]
fn the_linux_reader_gives_the_uuid_of_its_file_and_refuses_anything_else() {
    let scratch = Scratch::new("clock-proc-boot-id");
    let path = scratch.path().join("boot_id");
    let read = || ProcBootId::at(&path).read();

    fs::write(&path, format!("{A}\n")).expect("the fixture");
    assert_eq!(read().expect("the boot id").as_str(), A);

    fs::write(&path, " \n").expect("the fixture");
    assert_eq!(
        read().expect_err("an empty id").to_string(),
        "the boot id read from the kernel's boot_id file is empty"
    );

    fs::write(&path, "boot-1\n").expect("the fixture");
    assert_eq!(
        read().expect_err("not a UUID").to_string(),
        "the boot id read from the kernel's boot_id file is not a UUID: \"boot-1\""
    );

    fs::remove_file(&path).expect("remove the fixture");
    let error = read().expect_err("no file");
    assert!(matches!(&error, BootIdError::Read { source, .. }
        if source.kind() == io::ErrorKind::NotFound));
    assert!(
        error
            .to_string()
            .starts_with("the boot id cannot be read from the kernel's boot_id file: "),
        "{error}"
    );
}

/// Catches a UUID check that takes the wrong shape: either case of the hexadecimal digits is a
/// UUID; a group too short or too long, a missing or extra group, a digit that is not
/// hexadecimal, and white space inside are not.
#[test]
fn a_uuid_is_five_groups_of_hexadecimal_digits() {
    assert_eq!(
        BootId::uuid("x", &A.to_uppercase())
            .expect("upper case")
            .as_str(),
        A.to_uppercase()
    );
    assert_eq!(
        BootId::uuid("x", &format!("\t{A} \n"))
            .expect("trimmed")
            .to_string(),
        A
    );
    for text in [
        "0e1c6a3-5b7d-4c11-9a2e-6f0d3b8c4a71",
        "0e1c6a3f0-5b7d-4c11-9a2e-6f0d3b8c4a71",
        "0e1c6a3f-5b7d-4c11-9a2e",
        "0e1c6a3f-5b7d-4c11-9a2e-6f0d3b8c4a71-00",
        "0e1c6a3f5b7d4c119a2e6f0d3b8c4a71",
        "0e1c6a3g-5b7d-4c11-9a2e-6f0d3b8c4a71",
        "0e1c6a3f-5b7d-4c11-9a2e-6f0d3b8c4a7z",
        "0e1c6a3f-5b7d-4c11 9a2e-6f0d3b8c4a71",
    ] {
        assert!(
            matches!(BootId::uuid("x", text), Err(BootIdError::NotUuid { .. })),
            "{text:?}"
        );
    }
    assert!(matches!(
        BootId::uuid("x", ""),
        Err(BootIdError::Empty { from: "x" })
    ));
}

/// Catches a stamp format that changed with the unknown boot: a stamp of an unknown boot is
/// written with the empty `boot_id` older releases wrote, a stamp with an empty or blank
/// `boot_id` (one such a release wrote) reads as of an unknown boot, and a known boot id is
/// written and read back as it is.
#[test]
fn an_unknown_boot_is_the_empty_boot_id_on_disk() {
    let unknown = Stamp {
        boot_id: None,
        mono_secs: 7,
    };
    let text = serde_json::to_string(&unknown).expect("encode");
    assert_eq!(text, r#"{"boot_id":"","mono_secs":7}"#);
    assert_eq!(
        serde_json::from_str::<Stamp>(&text).expect("decode"),
        unknown
    );
    let blank: Stamp = serde_json::from_str(r#"{"boot_id":"  ","mono_secs":7}"#).expect("decode");
    assert_eq!(blank, unknown);

    let known = Stamp {
        boot_id: id("b"),
        mono_secs: 9,
    };
    let text = serde_json::to_string(&known).expect("encode");
    assert_eq!(text, r#"{"boot_id":"b","mono_secs":9}"#);
    assert_eq!(serde_json::from_str::<Stamp>(&text).expect("decode"), known);
    assert!(serde_json::from_str::<Stamp>(r#"{"boot_id":3,"mono_secs":9}"#).is_err());
}

/// Catches a host clock that answers nothing or has no boot id (the record's boot comparison
/// would then treat every boot alike): the host's clock is read and slept on without comparing
/// real time, and its boot id, read from the kernel, is a UUID and the same on two reads.
#[test]
fn the_system_clock_answers_and_names_the_boot() {
    let clock = SystemClock::new(Box::new(ProcBootId::host()));
    let _ = clock.mono_secs();
    clock.sleep_secs(0);
    let first = clock.boot_id().expect("the host's boot id");
    assert!(BootId::uuid("the host", first.as_str()).is_ok(), "{first}");
    assert_eq!(clock.boot_id().expect("the host's boot id"), first);
}

/// Catches stamps that do not order or measure: on an injected clock, the elapsed time is the
/// advance, a later stamp is not below an earlier one, and a sleep advances the clock by exactly
/// its argument and is recorded.
#[test]
fn the_injected_clock_orders_stamps_and_measures_elapsed_time() {
    let clock = FakeClock::new();
    let first = Stamp::now(&*clock);
    clock.advance(25);
    let second = Stamp::now(&*clock);
    assert_eq!(first.elapsed_secs(&*clock), Some(25));
    assert_eq!(second.elapsed_secs(&*clock), Some(0));
    assert!(second.same_boot(&first));
    assert!(second.mono_secs >= first.mono_secs);
    assert_eq!(second.mono_secs - first.mono_secs, 25);

    assert!(clock.slept().is_empty());
    clock.sleep_secs(7);
    assert_eq!(clock.slept(), [7]);
    assert_eq!(first.elapsed_secs(&*clock), Some(32));
    assert_eq!(second.elapsed_secs(&*clock), Some(7));
}
