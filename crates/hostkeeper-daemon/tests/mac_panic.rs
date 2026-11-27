//! A Mac's kernel panic reports parsed and classified (`hostkeeper_daemon::mac_panic`): each
//! fixture of `tests/fixtures/mac_health` (the JSON `.panic` and `.ips` forms, the text form, a
//! report cut short, an empty one, an app's crash, two reports of one panic) gives exactly the
//! expected report and record, compared whole; the timestamps, the ids and the classes at their
//! edges. The fixtures are written in the forms macOS writes, with generic ids and serials.

use hostkeeper_daemon::mac_panic::{
    BOARD, FILE_ID_PREFIX, MAX_INCIDENT, PANIC_HARDWARE, PANIC_SOFTWARE, PANIC_SOURCE, PanicClass,
    PanicReport, classify, file_event_id, is_candidate, parse, parse_timestamp,
};
use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

/// When the fixtures' files were last modified, as their reader would say.
const MODIFIED: u64 = 1_795_000_123;

/// The boot the records are read in: after every fixture's panic.
const BOOT: u64 = 1_795_660_000;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/mac_health/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn parsed(name: &str) -> PanicReport {
    parse(name, &fixture(name), MODIFIED).unwrap_or_else(|| panic!("{name} is a panic report"))
}

/// Proves the SError report gives the Mac record of the design's section 2.2: board, critical,
/// `panic_hardware`, its incident id in lower case, the header's time and the panic line as `raw`.
#[test]
fn an_serror_report_is_a_critical_hardware_record() {
    let report = parsed("serror.panic");
    assert_eq!(
        report,
        PanicReport {
            event_id: "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44".to_owned(),
            time_unix: 1_795_143_600,
            panic_line: "panic(cpu 4 caller 0xfffffe0012345678): \"SError interrupt, ESR \
                         0xbe000011\" @AppleARMPlatform.cpp:1234"
                .to_owned(),
            class: PanicClass::SError,
        }
    );
    assert_eq!(
        report.event(BOOT),
        pb::HardwareEvent {
            event_id: "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44".to_owned(),
            time_unix: 1_795_143_600,
            of_previous_boot: true,
            component: pb::HardwareComponent::Board.into(),
            subject: BOARD.to_owned(),
            severity: pb::HardwareSeverity::Critical.into(),
            state: pb::HardwareEventState::Event.into(),
            source: PANIC_SOURCE.to_owned(),
            code: PANIC_HARDWARE.to_owned(),
            message: "Kernel panic (SError), suspected hardware fault".to_owned(),
            raw: "panic(cpu 4 caller 0xfffffe0012345678): \"SError interrupt, ESR 0xbe000011\" \
                  @AppleARMPlatform.cpp:1234"
                .to_owned(),
            about: String::new(),
        }
    );
}

/// Proves a `.ips` report whose header says `bug_type` 210 is read as a panic, and that an AMCC
/// fault is hardware; its time is read with its offset from UTC.
#[test]
fn an_amcc_report_in_ips_form_is_hardware() {
    let report = parsed("amcc.ips");
    assert_eq!(
        report.event_id,
        "panic:0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"
    );
    assert_eq!(report.time_unix, 1_795_260_600);
    assert_eq!(report.class, PanicClass::Amcc);
    let event = report.event(BOOT);
    assert_eq!(event.code, PANIC_HARDWARE);
    assert_eq!(event.severity, i32::from(pb::HardwareSeverity::Critical));
    assert_eq!(
        event.message,
        "Kernel panic (AMCC memory controller fault), suspected hardware fault"
    );
}

/// Proves every software class is a warning of `panic_software`, with its own message: the SoC
/// watchdog, a userspace watchdog naming its process, a kernel extension that is not Apple's,
/// and anything else.
#[test]
fn watchdogs_kernel_extensions_and_other_panics_are_software_warnings() {
    let cases = [
        (
            "soc-watchdog.panic",
            "panic:1a2b3c4d-2222-3333-4444-555555555555",
            1_795_353_330,
            PanicClass::SocWatchdog,
            "Kernel panic (SoC watchdog), software or load",
        ),
        (
            "userspace-watchdog.panic",
            "panic:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            1_795_471_262,
            PanicClass::UserspaceWatchdog("WindowServer".to_owned()),
            "Kernel panic (userspace watchdog: WindowServer), software or load",
        ),
        (
            "kext.panic",
            "panic:12345678-90ab-cdef-1234-567890abcdef",
            1_795_514_400,
            PanicClass::Kext("com.example.driver.netcard".to_owned()),
            "Kernel panic in kernel extension com.example.driver.netcard",
        ),
        (
            "software.panic",
            "panic:fedcba98-7654-3210-fedc-ba9876543210",
            1_795_579_506,
            PanicClass::Software,
            "Kernel panic (software)",
        ),
    ];
    for (name, id, time, class, message) in cases {
        let report = parsed(name);
        assert_eq!(report.event_id, id, "{name}");
        assert_eq!(report.time_unix, time, "{name}");
        assert_eq!(report.class, class, "{name}");
        assert!(report.panic_line.starts_with("panic(cpu "), "{name}");
        let event = report.event(BOOT);
        assert_eq!(event.code, PANIC_SOFTWARE, "{name}");
        assert_eq!(
            event.severity,
            i32::from(pb::HardwareSeverity::Warning),
            "{name}"
        );
        assert_eq!(event.message, message, "{name}");
        assert_eq!(event.raw, report.panic_line, "{name}");
    }
}

/// Proves a report in the text form, with no JSON and no incident id, is known by its file's name
/// and timed by the file, its panic line read as printed.
#[test]
fn a_text_report_is_known_by_its_file_name_and_time() {
    let report = parsed("text.panic");
    assert_eq!(
        report,
        PanicReport {
            event_id: "panic:file:28d4040ef8ab122a".to_owned(),
            time_unix: MODIFIED,
            panic_line: "panic(cpu 0 caller 0xffffff8012345678): Kernel trap at \
                         0xffffff8012345678, type 14=page fault, registers:"
                .to_owned(),
            class: PanicClass::Software,
        }
    );
    assert_eq!(file_event_id("text.panic"), report.event_id);
}

/// Proves a report cut short in its body keeps its header's id and time, and its panic line is
/// read from the JSON text there is (to the escaped line end, escaped quotes read as quotes),
/// and classified.
#[test]
fn a_report_cut_short_keeps_its_header_and_its_panic_line() {
    assert_eq!(
        parsed("truncated.panic"),
        PanicReport {
            event_id: "panic:99999999-8888-7777-6666-555555555555".to_owned(),
            time_unix: 1_795_654_923,
            panic_line: "panic(cpu 5 caller 0xfffffe0078901234): \"SError interrupt, ESR \
                         0xbe000000\" @AppleARMPlatform.cpp:1234"
                .to_owned(),
            class: PanicClass::SError,
        }
    );
}

/// Proves an empty `.panic` file is still a panic, with no panic line: a warning, no `raw`.
#[test]
fn an_empty_panic_report_is_a_record_with_no_raw() {
    let report = parsed("empty.panic");
    assert_eq!(report.event_id, "panic:file:338fed85e676a0e7");
    assert_eq!(report.class, PanicClass::Unreadable);
    let event = report.event(BOOT);
    assert_eq!(event.raw, "");
    assert_eq!(event.code, PANIC_SOFTWARE);
    assert_eq!(event.message, "Kernel panic (its report has no panic line)");
}

/// Proves a `.ips` report that is not a kernel panic (an app's crash), one whose header is not
/// JSON, one with no `bug_type` and one that is empty are no panic reports; a number `bug_type`
/// of 210 is one.
#[test]
fn ips_reports_that_are_not_panics_are_passed_over() {
    assert_eq!(
        parse("app-crash.ips", &fixture("app-crash.ips"), MODIFIED),
        None
    );
    assert_eq!(parse("x.ips", b"not json\n{}", MODIFIED), None);
    assert_eq!(parse("x.ips", b"{\"timestamp\":\"x\"}\n{}", MODIFIED), None);
    assert_eq!(parse("x.ips", b"", MODIFIED), None);
    assert_eq!(parse("x.ips", b"{\"bug_type\":[210]}\n{}", MODIFIED), None);
    assert_eq!(parse("x.ips", b"{\"bug_type\":211}\n{}", MODIFIED), None);
    let numbered = parse(
        "x.ips",
        b"{\"bug_type\":210}\npanic(cpu 1 caller 0x1): watchdog\n",
        MODIFIED,
    )
    .expect("a number bug_type of 210 is a panic");
    assert_eq!(numbered.class, PanicClass::SocWatchdog);
    assert_eq!(
        numbered.time_unix, MODIFIED,
        "no timestamp: the file's time"
    );
}

/// Proves the `.panic` and the `.ips` report of one panic (ids differing only by case) give one
/// record id, and the record is the same.
#[test]
fn two_reports_of_one_panic_are_one_record() {
    let panic = parsed("serror.panic");
    let ips = parsed("serror-copy.ips");
    assert_eq!(panic.event_id, ips.event_id);
    assert_eq!(panic.event(BOOT), ips.event(BOOT));
}

/// Proves an incident id that cannot be the record's id (too long, not hex, empty) gives way to
/// the body's `incident`, then to the file's name; the header's time gives way to the body's
/// `date`, then to the file's.
#[test]
fn an_unusable_incident_id_or_time_falls_back_in_order() {
    let long = "a".repeat(MAX_INCIDENT + 1);
    let report = parse(
        "r.panic",
        format!(
            "{{\"bug_type\":\"210\",\"incident_id\":\"{long}\",\"timestamp\":\"yesterday\"}}\n\
             {{\"incident\":\"ABCD-EF01\",\"date\":\"2026-11-20 03:00:00.41 +0000\",\
             \"panicString\":\"panic(cpu 0 caller 0x1): x\"}}"
        )
        .as_bytes(),
        MODIFIED,
    )
    .expect("a panic report");
    assert_eq!(report.event_id, "panic:abcd-ef01");
    assert_eq!(report.time_unix, 1_795_143_600);
    for id in ["", "   ", "2b6f9c1e/../x", &long] {
        let report = parse(
            "r.panic",
            format!("{{\"bug_type\":\"210\",\"incident_id\":\"{id}\"}}\n{{}}").as_bytes(),
            MODIFIED,
        )
        .expect("a panic report");
        assert_eq!(report.event_id, file_event_id("r.panic"), "{id:?}");
        assert!(report.event_id.starts_with(FILE_ID_PREFIX));
        assert!(report.event_id.len() <= MAX_EVENT_ID);
        assert_eq!(report.time_unix, MODIFIED);
        assert_eq!(report.class, PanicClass::Unreadable, "no panic line");
    }
    let longest = "b".repeat(MAX_INCIDENT);
    let report = parse(
        "r.panic",
        format!("{{\"bug_type\":\"210\",\"incident_id\":\"{longest}\"}}").as_bytes(),
        MODIFIED,
    )
    .expect("a panic report");
    assert_eq!(report.event_id, format!("panic:{longest}"));
    assert_eq!(report.event_id.len(), MAX_EVENT_ID);
}

/// Proves a panic body whose `panicString` holds no panic line is read by the panic line found
/// elsewhere in the file, and with none at all is unreadable.
#[test]
fn a_body_with_no_panic_line_in_its_panic_string_is_scanned() {
    let report = parse(
        "r.panic",
        b"{\"bug_type\":\"210\"}\n{\"panicString\":\"Debugger message\",\"otherString\":\"panic(cpu 9 caller 0x2): AMCC fault\"}",
        MODIFIED,
    )
    .expect("a panic report");
    assert_eq!(report.panic_line, "panic(cpu 9 caller 0x2): AMCC fault\"}");
    assert_eq!(report.class, PanicClass::Amcc);
}

/// Proves a record is of the boot before only when it happened before this boot began, and
/// never when the boot's start is not known.
#[test]
fn a_record_is_of_the_boot_before_only_when_it_happened_before_it() {
    let report = parsed("serror.panic");
    assert!(report.event(report.time_unix + 1).of_previous_boot);
    assert!(!report.event(report.time_unix).of_previous_boot);
    assert!(!report.event(0).of_previous_boot);
}

/// Proves the classes at their edges: case is not minded; hardware wins over a watchdog and a
/// kernel extension; a userspace watchdog with no process named; Apple's kernel extensions are
/// not named, a list that ends at a blank line or that names only Apple's is software; a
/// process name is cut to its bound.
#[test]
fn the_classes_at_their_edges() {
    assert_eq!(
        classify("panic(cpu 1): serror on watchdog"),
        PanicClass::SError
    );
    assert_eq!(classify("panic(cpu 1): Amcc0 fault"), PanicClass::Amcc);
    assert_eq!(
        classify("panic(cpu 1): Userspace Watchdog timeout"),
        PanicClass::UserspaceWatchdog(String::new())
    );
    assert_eq!(
        PanicClass::UserspaceWatchdog(String::new()).message(),
        "Kernel panic (userspace watchdog), software or load"
    );
    assert_eq!(
        classify("panic(cpu 1): watchdog timeout: no successful checkins from "),
        PanicClass::UserspaceWatchdog(String::new())
    );
    let long = "p".repeat(100);
    match classify(&format!(
        "panic(cpu 1): no successful checkins from {long} in 90 seconds"
    )) {
        PanicClass::UserspaceWatchdog(process) => assert_eq!(process, "p".repeat(48)),
        other => panic!("{other:?}"),
    }
    let apple_only = "panic(cpu 1): data abort\nKernel Extensions in backtrace:\n  \
                      com.apple.iokit.IOPCIFamily(2.9)\n\n  com.example.later(1.0)\n";
    assert_eq!(classify(apple_only), PanicClass::Software);
    let no_list = "panic(cpu 1): data abort\nKernel Extensions in backtrace:\n";
    assert_eq!(classify(no_list), PanicClass::Software);
    let blank_name =
        "panic(cpu 1): data abort\nKernel Extensions in backtrace:\n  (1.0)\n  com.example.x(2)\n";
    assert_eq!(
        classify(blank_name),
        PanicClass::Kext("com.example.x".to_owned())
    );
    assert_eq!(classify(""), PanicClass::Software);
    let words = [
        (PanicClass::SError, "SError"),
        (PanicClass::Amcc, "AMCC"),
        (
            PanicClass::UserspaceWatchdog("x".to_owned()),
            "userspace watchdog",
        ),
        (PanicClass::SocWatchdog, "SoC watchdog"),
        (PanicClass::Kext("x".to_owned()), "kernel extension"),
        (PanicClass::Software, "software"),
        (PanicClass::Unreadable, "unreadable"),
    ];
    for (class, word) in words {
        assert_eq!(class.word(), word);
        assert!(class.message().len() <= MAX_MESSAGE);
    }
    let kext = PanicClass::Kext("k".repeat(200));
    assert_eq!(kext.message().len(), MAX_MESSAGE, "cut to its bound");
}

/// Proves the report's time at its edges: fractions dropped, offsets either way, a leap day, a
/// January, and each malformed part refused.
#[test]
fn timestamps_are_read_with_their_offset_and_refused_when_malformed() {
    assert_eq!(
        parse_timestamp("2026-11-20 03:00:00.00 +0000"),
        Some(1_795_143_600)
    );
    assert_eq!(
        parse_timestamp("2026-11-20 03:00:00 +0000"),
        Some(1_795_143_600)
    );
    assert_eq!(
        parse_timestamp("2026-11-22 08:15:30.99 -0500"),
        Some(1_795_353_330)
    );
    assert_eq!(
        parse_timestamp("2028-02-29 00:00:00 +0000"),
        Some(1_835_395_200)
    );
    assert_eq!(
        parse_timestamp("2027-01-01 00:30:00 +0000"),
        Some(1_798_763_400)
    );
    for bad in [
        "",
        "2026-11-20",
        "2026-11-20 03:00:00",
        "2026-11-20 03:00:00 +0000 extra",
        "2026-11 03:00:00 +0000",
        "2026-11-20-01 03:00:00 +0000",
        "2026-13-20 03:00:00 +0000",
        "2026-00-20 03:00:00 +0000",
        "2026-11-32 03:00:00 +0000",
        "2026-11-00 03:00:00 +0000",
        "26-11-20 03:00:00 +0000",
        "2026-9-20 03:00:00 +0000",
        "2026-11-2x 03:00:00 +0000",
        "2026-11-20 24:00:00 +0000",
        "2026-11-20 03:60:00 +0000",
        "2026-11-20 03:00:61 +0000",
        "2026-11-20 03:00 +0000",
        "2026-11-20 03:00:00:00 +0000",
        "2026-11-20 03:00:00 0000",
        "2026-11-20 03:00:00 +000",
        "2026-11-20 03:00:00 +00x0",
        "2026-11-20 03:00:00 +",
        "2026-11-20 03:00:00 +0\u{e9}0",
        "2026-11-20 03:00:00 \u{e9}000",
    ] {
        assert_eq!(parse_timestamp(bad), None, "{bad:?}");
    }
}

/// Proves only `.panic` and `.ips` files are read.
#[test]
fn only_panic_and_ips_files_are_candidates() {
    assert!(is_candidate("panic-full-2026-11-20-030000.panic"));
    assert!(is_candidate("Kernel-2026-11-20.ips"));
    assert!(!is_candidate("x.diag"));
    assert!(!is_candidate("x.panic.gz"));
    assert!(!is_candidate("Retired"));
}
