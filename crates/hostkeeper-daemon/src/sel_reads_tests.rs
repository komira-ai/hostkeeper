//! How a [`SelReader`] reads, over the fake BMC of `sel_source_tests.rs`: what it asks while the
//! SEL is unchanged and after it grows, an erase, the hours of corrected errors, a missing or
//! silent BMC, a daemon that may not open the device, refusals and short answers, entries gone or
//! malformed, an SDR that cannot be read, its bounds, the first read's history, a corrupt mark,
//! and its thread.

use super::*;
use crate::host_health::{HISTORY_PREFIX, HISTORY_SECS};

/// Catches a SEL read again in full at every read: while its count, last addition and erase are
/// those of the read before, the reader asks `Get Device ID` and `Get SEL Info` and nothing else,
/// and holds nothing new.
#[test]
fn nothing_is_asked_past_sel_info_while_the_sel_is_unchanged() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-unchanged", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    assert_eq!(rig.take_held().len(), 3);
    bmc.asked();
    assert_eq!(rig.poll(INCIDENT_READ + 60), POLL_SECS);
    assert_eq!(bmc.asked(), [BmcRequest::DeviceId, BmcRequest::SelInfo]);
    assert!(rig.held().is_empty());
}

/// Catches new entries read from the first again (and so the records written already held
/// again), or the SDR read again though it did not change: the read after an addition asks the
/// last entry read for the id after it, reads from there, and holds only the new entry.
#[test]
fn new_entries_are_read_after_the_last_one() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-new", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    rig.take_held();
    let more = fixture(KINDS_SEL)[1].clone();
    let mut entry = more;
    entry[0..2].copy_from_slice(&0x01a6_u16.to_le_bytes());
    bmc.with(|b| {
        b.sel.push(entry);
        b.added += 1;
    });
    bmc.asked();
    rig.poll(INCIDENT_READ + 60);
    assert_eq!(
        bmc.asked(),
        [
            BmcRequest::DeviceId,
            BmcRequest::SelInfo,
            BmcRequest::SelEntry(0x01a5),
            BmcRequest::SdrInfo,
            BmcRequest::SelEntry(0x01a6),
        ]
    );
    let held = rig.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].event_id, "sel:01a6@1790000000");
    assert_eq!(held[0].code, "cpu_ierr");
    assert_eq!(rig.mark().last_id, Some(0x01a6));

    // An addition with no entry after the last one read: nothing more is asked, nothing held.
    rig.take_held();
    bmc.with(|b| b.added += 1);
    bmc.asked();
    rig.poll(INCIDENT_READ + 120);
    assert_eq!(
        bmc.asked(),
        [
            BmcRequest::DeviceId,
            BmcRequest::SelInfo,
            BmcRequest::SelEntry(0x01a6)
        ]
    );
    assert!(rig.held().is_empty());
    assert_eq!(rig.mark().added, INCIDENT_ERASED + 3);
}

/// Catches a transport error of `Get SEL Info` or of the SDR's reads read as an empty SEL or a
/// failed read of the SEL: `Get SEL Info` timing out is `unreadable` naming it; the SDR's state
/// timing out leaves the SEL read, its sensors named by number.
#[test]
fn transport_failures_of_sel_info_and_the_sdr_say_what_failed() {
    struct Failing(Arc<FakeBmc>, BmcRequest);
    impl BmcPort for Failing {
        fn ask(&self, request: BmcRequest) -> io::Result<Vec<u8>> {
            if request == self.1 {
                Err(io::Error::from(io::ErrorKind::TimedOut))
            } else {
                self.0.ask(request)
            }
        }
    }
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let rig = Rig::new("sel-transport", &bmc, INCIDENT_BOOT);
    let read = |fails: BmcRequest| {
        let mut reader = SelReader::new(
            Arc::new(Failing(Arc::clone(&bmc), fails)),
            rig.dir.path().join("proc-stat"),
        );
        crate::hardware_sources::poll(&mut reader, &rig.keeper, &At(INCIDENT_READ));
    };
    read(BmcRequest::SelInfo);
    assert_eq!(
        rig.state(),
        (
            pb::SourceStatus::Unreadable,
            "Get SEL Info: timed out".to_owned()
        )
    );
    assert!(rig.held().is_empty());
    read(BmcRequest::SdrInfo);
    assert_eq!(rig.state(), (pb::SourceStatus::Ok, String::new()));
    assert_eq!(rig.held()[0].subject, "Memory #0x53");
}

/// Catches an erased SEL whose ids start again read as nothing new (its last id read still
/// there, its count fallen) or its records given the ids of the entries before the erase: after
/// an erase the SEL is read from its first entry, each record's id names the new erase time, the
/// conditions open before are forgotten, and a deassertion after it names no assertion.
#[test]
fn an_erased_sel_is_read_again_from_its_first_entry() {
    let bmc = FakeBmc::holding(KINDS_SEL, KINDS_ERASED);
    let mut rig = Rig::new("sel-erased", &bmc, KINDS_BOOT);
    rig.poll(INCIDENT_READ);
    rig.take_held();
    assert!(!rig.mark().open.is_empty());
    let kinds = fixture(KINDS_SEL);
    // The SEL erased, then one entry: the power supply's deassertion, at the id of its failure.
    let mut ended = kinds[5].clone();
    ended[0..2].copy_from_slice(&0x0014_u16.to_le_bytes());
    bmc.with(|b| {
        b.sel = vec![ended];
        b.erased = KINDS_ERASED + 100;
        b.added = KINDS_ERASED + 101;
    });
    rig.poll(INCIDENT_READ + 60);
    let held = rig.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].event_id, format!("sel:0014@{}", KINDS_ERASED + 100));
    assert_eq!(held[0].about, "");
    assert!(rig.mark().open.is_empty());

    // Erased again with the same erase time but fewer entries (a BMC whose erase time does not
    // move): read from the first again.
    rig.take_held();
    bmc.with(|b| {
        b.sel.clear();
        b.added += 1;
    });
    bmc.asked();
    rig.poll(INCIDENT_READ + 120);
    assert_eq!(bmc.asked(), [BmcRequest::DeviceId, BmcRequest::SelInfo]);
    assert_eq!(rig.mark().last_id, None);
    assert_eq!(rig.mark().entries, 0);
}

/// Catches corrected errors held one record each, a count held before its hour is over (and so
/// held again, short, when the hour goes on), or an hour's count lost when the SEL is erased: the
/// hour's count stays in the mark until the hour is over, an entry of a later hour holds the
/// earlier hour's count at once, and an erase holds every count open.
#[test]
fn corrected_errors_are_counted_until_their_hour_is_over() {
    let kinds = fixture(KINDS_SEL);
    let corrected = |id: u16, at: u64| {
        let mut entry = kinds[19].clone();
        entry[0..2].copy_from_slice(&id.to_le_bytes());
        entry[3..7].copy_from_slice(&u32::try_from(at).expect("a time").to_le_bytes());
        entry
    };
    let bmc = FakeBmc::holding("", KINDS_ERASED);
    bmc.with(|b| b.sel = vec![corrected(1, KINDS_T), corrected(2, KINDS_T + 60)]);
    let mut rig = Rig::new("sel-corrected", &bmc, KINDS_BOOT);
    rig.poll(KINDS_T + 120);
    assert!(rig.held().is_empty());
    assert_eq!(
        rig.mark().corrected.values().map(|b| b.count).sum::<u64>(),
        2
    );

    // A third in the same hour, read in a later read: still one count.
    bmc.with(|b| {
        b.sel.push(corrected(3, KINDS_T + 600));
        b.added += 1;
    });
    rig.poll(KINDS_T + 700);
    assert!(rig.held().is_empty());

    // One of the next hour: the first hour's three are held, the next hour's one is open.
    bmc.with(|b| {
        b.sel.push(corrected(4, KINDS_T + 3600));
        b.added += 1;
    });
    rig.poll(KINDS_T + 3610);
    let held = rig.take_held();
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].message,
        "3 corrected ECC errors on DIMM D1 in the hour"
    );
    assert_eq!(held[0].event_id, format!("sel:0001@{KINDS_ERASED}"));
    assert_eq!(held[0].raw.get(..4), Some("0003"));

    // The SEL unchanged, the next hour over: its one is held, in the singular.
    rig.poll(KINDS_T + 7200);
    let held = rig.take_held();
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].message,
        "1 corrected ECC error on DIMM D1 in the hour"
    );

    // An hour open when the SEL is erased is held at the erase.
    bmc.with(|b| {
        b.sel.push(corrected(5, KINDS_T + 7300));
        b.added += 1;
    });
    rig.poll(KINDS_T + 7400);
    assert!(rig.held().is_empty());
    bmc.with(|b| {
        b.sel.clear();
        b.erased += 1;
    });
    rig.poll(KINDS_T + 7500);
    let held = rig.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].event_id, format!("sel:0005@{KINDS_ERASED}"));
}

/// Catches a host with no BMC device read as one with nothing wrong, `bmc_missing` held at the
/// first miss (a BMC slow at start) or at every miss, never ended, or ended without naming its
/// assertion: the source is `unsupported` at once, the second read in a row with no device holds
/// one `bmc_missing` record, later misses none, the waits grow to their bound, and the first read
/// that a BMC answers holds its deassertion and reads the SEL.
#[test]
fn a_host_with_no_bmc_device_is_bmc_missing_until_one_answers() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    bmc.with(|b| b.down = Some(io::ErrorKind::NotFound));
    let mut rig = Rig::new("sel-no-device", &bmc, INCIDENT_BOOT);
    assert_eq!(rig.poll(1_795_700_000), 120);
    assert_eq!(
        rig.state(),
        (
            pb::SourceStatus::Unsupported,
            "/dev/ipmi0 is absent".to_owned()
        )
    );
    assert!(rig.held().is_empty());
    assert_eq!(rig.poll(1_795_700_120), 240);
    let missing = pb::HardwareEvent {
        event_id: "bmc_missing@1795700120".to_owned(),
        time_unix: 1_795_700_120,
        of_previous_boot: false,
        component: C::Host.into(),
        subject: "BMC".to_owned(),
        severity: Sv::Warning.into(),
        state: St::Asserted.into(),
        source: "sel".to_owned(),
        code: "bmc_missing".to_owned(),
        message: "No BMC device (/dev/ipmi0 is absent); hardware events cannot be read".to_owned(),
        raw: "/dev/ipmi0: entity not found".to_owned(),
        about: String::new(),
    };
    assert_eq!(rig.take_held(), [missing]);
    assert_eq!(rig.poll(1_795_700_360), 300);
    assert_eq!(rig.poll(1_795_700_660), 300);
    assert!(rig.held().is_empty());

    bmc.with(|b| b.down = None);
    assert_eq!(rig.poll(1_795_700_960), POLL_SECS);
    let held = rig.held();
    assert_eq!(
        held.len(),
        4,
        "the deassertion and the SEL's three: {held:?}"
    );
    assert_eq!(
        held[0],
        pb::HardwareEvent {
            event_id: "bmc_missing@1795700120:end".to_owned(),
            time_unix: 1_795_700_960,
            of_previous_boot: false,
            component: C::Host.into(),
            subject: "BMC".to_owned(),
            severity: Sv::Info.into(),
            state: St::Deasserted.into(),
            source: "sel".to_owned(),
            code: "bmc_missing".to_owned(),
            message: "A BMC answers again".to_owned(),
            raw: "Get Device ID answered".to_owned(),
            about: "bmc_missing@1795700120".to_owned(),
        }
    );
    assert_eq!(rig.state(), (pb::SourceStatus::Ok, String::new()));
}

/// Catches a BMC that does not answer `Get Device ID` (a timeout, or an empty answer) read as a
/// BMC, or as a host with no device: the source is `unreadable`, and the second such read holds
/// `bmc_missing` with "No BMC answers".
#[test]
fn a_bmc_that_does_not_answer_is_bmc_missing() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    bmc.with(|b| b.down = Some(io::ErrorKind::TimedOut));
    let mut rig = Rig::new("sel-silent", &bmc, INCIDENT_BOOT);
    rig.poll(1_795_700_000);
    assert_eq!(
        rig.state(),
        (
            pb::SourceStatus::Unreadable,
            "the BMC does not answer Get Device ID".to_owned()
        )
    );
    bmc.with(|b| {
        b.down = None;
        b.answer.insert((0x06, 0x01), Vec::new());
    });
    rig.poll(1_795_700_120);
    let held = rig.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].code, "bmc_missing");
    assert_eq!(
        held[0].message,
        "No BMC answers; hardware events cannot be read"
    );
    assert_eq!(held[0].raw, "Get Device ID: an empty answer");
}

/// Catches a daemon that may not open the device (not root) read as a host with no BMC: the
/// source is `unreadable` with its reason, and no read holds `bmc_missing`.
#[test]
fn a_daemon_that_may_not_open_the_device_raises_nothing() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    bmc.with(|b| b.down = Some(io::ErrorKind::PermissionDenied));
    let mut rig = Rig::new("sel-denied", &bmc, INCIDENT_BOOT);
    for at in 0..3 {
        rig.poll(1_795_700_000 + at * 300);
    }
    assert_eq!(
        rig.state(),
        (
            pb::SourceStatus::Unreadable,
            "the daemon may not open /dev/ipmi0".to_owned()
        )
    );
    assert!(rig.held().is_empty());
    assert_eq!(rig.keeper.mark(SOURCE), None);
}

/// Catches a refusal or a short answer read as an empty SEL, or a failed read that keeps what it
/// read before the failure from being held: each is `unreadable` with its reason, a refused
/// `Get Device ID` is still a BMC that answers (no `bmc_missing`), and an entry refused in the
/// middle keeps the records before it, the next read going on from the last one read.
#[test]
fn refusals_and_short_answers_are_unreadable_with_their_reason() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-refusals", &bmc, INCIDENT_BOOT);
    let unreadable = |reason: &str| (pb::SourceStatus::Unreadable, reason.to_owned());

    bmc.with(|b| {
        b.refuse.insert((0x06, 0x01), 0xc1);
    });
    rig.poll(1_795_700_000);
    rig.poll(1_795_700_120);
    assert_eq!(
        rig.state(),
        unreadable("the BMC refused Get Device ID (0xc1)")
    );
    assert!(rig.held().is_empty());

    bmc.with(|b| {
        b.refuse.clear();
        b.refuse.insert((0x0A, 0x40), 0xd5);
    });
    rig.poll(1_795_700_240);
    assert_eq!(
        rig.state(),
        unreadable("the BMC refused Get SEL Info (0xd5)")
    );

    bmc.with(|b| {
        b.refuse.clear();
        b.answer.insert((0x0A, 0x40), vec![0, 0x51, 4, 0]);
    });
    rig.poll(1_795_700_360);
    assert_eq!(rig.state(), unreadable("a short answer to Get SEL Info"));

    bmc.with(|b| {
        b.answer.clear();
        b.refuse_entry.insert(0x01a4, 0xc5);
    });
    assert_eq!(
        rig.poll(1_795_700_480),
        MAX_WAIT_SECS,
        "the fifth failure in a row"
    );
    assert_eq!(
        rig.state(),
        unreadable("the BMC refused Get SEL Entry 0x01a4 (0xc5)")
    );
    assert_eq!(rig.mark().last_id, Some(0x01a3));
    assert_eq!(rig.mark().entries, 0, "a read cut short is not the SEL's");
    assert_eq!(
        rig.take_held().len(),
        1,
        "the hour of corrected errors is over and held"
    );

    bmc.with(|b| {
        b.refuse_entry.clear();
        b.answer.insert((0x0A, 0x43), vec![0, 0xa5]);
    });
    rig.poll(1_795_700_600);
    assert_eq!(
        rig.state(),
        unreadable("a short answer to Get SEL Entry 0x01a3")
    );

    bmc.with(|b| {
        b.answer.clear();
        b.down = Some(io::ErrorKind::BrokenPipe);
    });
    rig.poll(1_795_700_720);
    rig.poll(1_795_700_840);
    bmc.with(|b| b.down = None);
    rig.poll(1_795_700_960);
    let held = rig.take_held();
    assert_eq!(
        held.iter().map(|e| e.code.as_str()).collect::<Vec<_>>(),
        [
            "bmc_missing",
            "bmc_missing",
            "memory_uncorrectable",
            "system_boot"
        ],
        "a missing BMC, its return, and the entries after the last one read"
    );
    assert_eq!(rig.state(), (pb::SourceStatus::Ok, String::new()));
}

/// Catches an error of the transport while the last entry read is asked for read as a SEL to read
/// from its first entry: the read fails `unreadable`, naming the entry, and holds nothing.
#[test]
fn a_failure_asking_the_last_entry_is_unreadable() {
    struct Failing(Arc<FakeBmc>);
    impl BmcPort for Failing {
        fn ask(&self, request: BmcRequest) -> io::Result<Vec<u8>> {
            match request {
                BmcRequest::SelEntry(0x01a5) => Err(io::Error::from(io::ErrorKind::TimedOut)),
                other => self.0.ask(other),
            }
        }
    }
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-last-fails", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    rig.take_held();
    bmc.with(|b| b.added += 1);
    let mut failing = SelReader::new(
        Arc::new(Failing(Arc::clone(&bmc))),
        rig.dir.path().join("proc-stat"),
    );
    crate::hardware_sources::poll(&mut failing, &rig.keeper, &At(INCIDENT_READ + 60));
    assert_eq!(
        rig.state(),
        (
            pb::SourceStatus::Unreadable,
            "Get SEL Entry 0x01a5: timed out".to_owned()
        )
    );
    assert!(rig.held().is_empty());
}

/// Catches the walk read again from an entry the BMC no longer has, or a SEL whose last entry
/// read is gone read as unchanged: a last entry gone is read from the first entry (its records
/// held once more, under the same ids, which the log writes once), and an entry gone in the
/// middle of a walk ends the read, to be read again at the next.
#[test]
fn entries_gone_are_read_from_the_first() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-gone", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    rig.take_held();
    // The last entry deleted by another tool, and one added.
    bmc.with(|b| {
        b.sel.pop();
        let mut entry = fixture(KINDS_SEL)[1].clone();
        entry[0..2].copy_from_slice(&0x01a6_u16.to_le_bytes());
        b.sel.push(entry);
        b.added += 1;
        b.refuse_entry.insert(0x01a3, NOT_PRESENT);
    });
    rig.poll(INCIDENT_READ + 60);
    assert_eq!(
        (rig.mark().last_id, rig.mark().added),
        (Some(0x01a2), INCIDENT_ERASED + 1),
        "the walk ended at the entry gone, and the SEL is not taken as read"
    );
    bmc.with(|b| {
        b.refuse_entry.clear();
    });
    rig.poll(INCIDENT_READ + 120);
    assert_eq!(rig.mark().last_id, Some(0x01a6));
    assert_eq!(rig.mark().entries, 4);
    let codes: Vec<String> = rig.held().into_iter().map(|e| e.event_id).collect();
    assert!(
        codes.contains(&"sel:01a6@1790000000".to_owned()),
        "{codes:?}"
    );
}

/// Catches an entry that is not 16 bytes decoded from what it has, or ending the read: it is
/// passed over, and the entries after it are read.
#[test]
fn an_entry_of_the_wrong_length_is_passed_over() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    bmc.with(|b| b.sel.insert(1, vec![0x07, 0x00, 0x02, 0x00]));
    let mut rig = Rig::new("sel-short-entry", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    assert_eq!(rig.held().len(), 3);
    assert_eq!(rig.mark().last_id, Some(0x01a5));
}

/// Catches an SDR that cannot be read failing the SEL's read, or an SDR read cut short replacing
/// the names of a whole one: the SEL is read and its sensors named by type and number; a short or refused record ends the SDR's read, which is read again at the
/// next change of the SEL; an SDR whose state is unchanged is not read again.
#[test]
fn an_sdr_that_cannot_be_read_names_sensors_by_number() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    bmc.with(|b| {
        b.refuse.insert((0x0A, 0x20), 0xc1);
    });
    let mut rig = Rig::new("sel-no-sdr", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    let held = rig.take_held();
    assert_eq!(held[0].subject, "Memory #0x53");
    assert_eq!(
        held[0].raw,
        "01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 | Uncorrectable ECC | Asserted"
    );
    assert_eq!(rig.state(), (pb::SourceStatus::Ok, String::new()));

    let add = |rig: &mut Rig, at: u64| {
        bmc.with(|b| {
            let mut entry = fixture(INCIDENT_SEL)[2].clone();
            let id = u16::try_from(0x0200 + at % 1000).expect("an id");
            entry[0..2].copy_from_slice(&id.to_le_bytes());
            b.sel.push(entry);
            b.added += 1;
        });
        rig.poll(INCIDENT_READ + at);
        rig.take_held()
    };

    // A record refused in the middle: the names read before it serve.
    bmc.with(|b| {
        b.refuse.clear();
        b.refuse_sdr.insert(0x0003, 0xc5);
    });
    assert_eq!(add(&mut rig, 60)[0].subject, "DIMM D1");

    // Read whole, then not again while its state is unchanged.
    bmc.with(|b| b.refuse_sdr.clear());
    add(&mut rig, 120);
    bmc.asked();
    add(&mut rig, 180);
    assert!(!bmc.asked().iter().any(|r| matches!(r, BmcRequest::Sdr(_))));

    // Changed, and cut short: the whole SDR's names are kept.
    bmc.with(|b| {
        b.sdr_added += 1;
        b.answer.insert((0x0A, 0x23), vec![0]);
    });
    assert_eq!(add(&mut rig, 240)[0].subject, "DIMM D1");

    // A short SDR state: the SDR is read again at every change of the SEL.
    bmc.with(|b| {
        b.answer.clear();
        b.answer.insert((0x0A, 0x20), vec![0, 0x51]);
    });
    add(&mut rig, 300);
    bmc.asked();
    add(&mut rig, 360);
    assert!(bmc.asked().iter().any(|r| matches!(r, BmcRequest::Sdr(_))));
}

/// Catches an SDR or a SEL whose next id loops, or that has more records than a read may ask
/// for, holding the reader: a record whose next id is its own ends the read, and a read asks for
/// at most `MAX_SDR` records and `MAX_WALK` entries, the rest at the next read.
#[test]
fn reads_are_bounded() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let template = fixture(NAMES_SDR)[1].clone();
    let sdr: Vec<Vec<u8>> = (1..=u16::try_from(MAX_SDR + 1).expect("a count"))
        .map(|id| {
            let mut record = template.clone();
            record[0..2].copy_from_slice(&id.to_le_bytes());
            record
        })
        .collect();
    let entry = fixture(INCIDENT_SEL)[3].clone();
    let sel: Vec<Vec<u8>> = (1..=u16::try_from(MAX_WALK + 1).expect("a count"))
        .map(|id| {
            let mut record = entry.clone();
            record[0..2].copy_from_slice(&id.to_le_bytes());
            record
        })
        .collect();
    bmc.with(|b| {
        b.sdr = sdr;
        b.sel = sel;
    });
    let mut rig = Rig::new("sel-bounded", &bmc, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    let asked = bmc.asked();
    let count = |sdr: bool| {
        asked
            .iter()
            .filter(|r| {
                matches!(r, BmcRequest::Sdr(_)) == sdr
                    && !matches!(
                        r,
                        BmcRequest::DeviceId | BmcRequest::SelInfo | BmcRequest::SdrInfo
                    )
            })
            .count()
    };
    assert_eq!(count(true), MAX_SDR);
    assert_eq!(count(false), MAX_WALK);
    assert_eq!(
        rig.mark().last_id,
        Some(u16::try_from(MAX_WALK).expect("an id"))
    );
    assert_eq!(rig.mark().entries, 0);
    rig.poll(INCIDENT_READ + 60);
    assert_eq!(
        rig.mark().entries,
        u16::try_from(MAX_WALK + 1).expect("a count")
    );

    // A SEL entry and an SDR record whose next id is their own.
    let looping = FakeBmc::default();
    looping.with(|b| {
        let mut entry = fixture(INCIDENT_SEL)[2].clone();
        entry[0..2].copy_from_slice(&0_u16.to_le_bytes());
        b.sel = vec![entry.clone(), entry];
        let mut record = template.clone();
        record[0..2].copy_from_slice(&0_u16.to_le_bytes());
        b.sdr = vec![record.clone(), record];
        b.erased = INCIDENT_ERASED;
        b.added = INCIDENT_ERASED;
    });
    let looping = Arc::new(looping);
    let mut rig = Rig::new("sel-looping", &looping, INCIDENT_BOOT);
    rig.poll(INCIDENT_READ);
    assert_eq!(
        looping.asked().len(),
        5,
        "Device ID, SEL Info, SDR Info, one SDR, one entry"
    );
    assert_eq!(rig.held()[0].subject, "DIMM D1");
    assert_eq!(rig.mark().entries, 2);
}

/// Catches conditions kept without bound in the mark: past `MAX_OPEN` the first by key is
/// forgotten, and a condition asserted again while the mark is full is kept once.
#[test]
fn open_conditions_are_bounded() {
    let bmc = FakeBmc::holding("", KINDS_ERASED);
    let failure = fixture(KINDS_SEL)[4].clone();
    let sel: Vec<Vec<u8>> = (0..=MAX_OPEN)
        .map(|n| {
            let mut entry = failure.clone();
            entry[0..2].copy_from_slice(&u16::try_from(n + 1).expect("an id").to_le_bytes());
            entry[11] = u8::try_from(n).expect("a sensor number");
            entry
        })
        .collect();
    bmc.with(|b| {
        b.sel = sel;
        b.sel.push({
            let mut again = b.sel[MAX_OPEN].clone();
            again[0..2].copy_from_slice(&0x0fff_u16.to_le_bytes());
            again
        });
    });
    let mut rig = Rig::new("sel-open-bound", &bmc, KINDS_BOOT);
    rig.poll(INCIDENT_READ);
    let open = rig.mark().open;
    assert_eq!(open.len(), MAX_OPEN);
    assert!(
        !open.contains_key("20.0:00:6f:1"),
        "the first by key is forgotten"
    );
    assert_eq!(
        open.get(&format!("20.0:{MAX_OPEN:02x}:6f:1"))
            .map(String::as_str),
        Some(format!("sel:0fff@{KINDS_ERASED}").as_str())
    );
}

/// Catches the first read on a host that had no BMC at first consuming the SEL's first read, so
/// an old SEL read later would raise as news: the SEL's entries older than seven days, read at
/// the first read that reaches the BMC, are history, and a recent one is not.
#[test]
fn an_old_sel_first_read_after_a_missing_bmc_is_history() {
    let bmc = FakeBmc::holding(KINDS_SEL, KINDS_ERASED);
    let later = KINDS_T + 30 * 24 * 3600;
    bmc.with(|b| {
        b.down = Some(io::ErrorKind::NotFound);
        let mut recent = fixture(INCIDENT_SEL)[2].clone();
        recent[0..2].copy_from_slice(&0x0300_u16.to_le_bytes());
        recent[3..7].copy_from_slice(&u32::try_from(later + 100).expect("a time").to_le_bytes());
        b.sel.push(recent);
    });
    let mut rig = Rig::new("sel-history", &bmc, later - 60);
    rig.poll(later);
    rig.poll(later + 120);
    assert_eq!(rig.take_held().len(), 1, "bmc_missing");
    bmc.with(|b| b.down = None);
    rig.poll(later + 360);
    let held = rig.held();
    let sel: Vec<&pb::HardwareEvent> = held
        .iter()
        .filter(|e| e.code != "bmc_missing" && e.time_unix != 0)
        .collect();
    assert!(sel.len() > 1);
    for event in sel {
        let old = event.time_unix + HISTORY_SECS < later + 360;
        assert_eq!(event.message.starts_with(HISTORY_PREFIX), old, "{event:?}");
        if old {
            assert_eq!(event.severity(), Sv::Info, "{event:?}");
        } else {
            assert_eq!(event.code, "memory_uncorrectable");
            assert_eq!(event.severity(), Sv::Critical);
        }
    }
}

/// Catches a record of a boot whose start is not known marked as before it.
#[test]
fn a_boot_not_known_marks_nothing_as_before_it() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-no-boot", &bmc, 0);
    rig.poll(INCIDENT_READ);
    assert!(rig.held().iter().all(|e| !e.of_previous_boot));
}

/// Catches a corrupt mark failing the read: it is read as no mark, and the SEL read from its
/// first entry.
#[test]
fn a_corrupt_mark_reads_the_sel_from_its_first_entry() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-corrupt-mark", &bmc, INCIDENT_BOOT);
    rig.keeper
        .record(SOURCE, "{not json", Vec::new(), INCIDENT_READ);
    rig.poll(INCIDENT_READ);
    assert_eq!(rig.held().len(), 3);
    assert_eq!(rig.mark().last_id, Some(0x01a5));
}

/// Catches the worker's loop ending before its sleeper says stop, or waiting another time than
/// the read says: three reads through the daemon's worker, each followed by the wait it gave.
#[test]
fn the_reader_reads_at_each_wait_until_stopped() {
    let bmc = FakeBmc::holding(INCIDENT_SEL, INCIDENT_ERASED);
    let mut rig = Rig::new("sel-run", &bmc, INCIDENT_BOOT);
    let sleeper = ScriptedSleeper::new(2);
    crate::hardware_sources::run(&mut rig.reader, &rig.keeper, &At(INCIDENT_READ), &sleeper);
    assert_eq!(sleeper.waits(), [POLL_SECS, POLL_SECS, POLL_SECS]);
    let asked = bmc.asked();
    assert_eq!(
        asked.iter().filter(|r| **r == BmcRequest::DeviceId).count(),
        3
    );
    assert_eq!(rig.reader.name(), "sel");
}
