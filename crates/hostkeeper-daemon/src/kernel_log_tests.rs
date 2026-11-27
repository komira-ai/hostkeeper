//! Tests of `kernel_log`: the reading of `/dev/kmsg` records and of `journalctl --output=json`
//! lines (escapes, continuation lines, cut lines, stamps), the boot and device helpers, the
//! format of a record's id, and the classifying of kernel log lines into hardware records (machine
//! checks, EDAC, AER, APEI/BERT groups, AMD's decoder, I/O errors) and their coalescing, over the
//! fixtures of `tests/fixtures/kernel` and over single lines.

use super::*;

use pb::HardwareComponent::{Board, Cpu, Disk, Memory, Pcie};
use pb::HardwareSeverity::{Critical, Info, Warning};

const BOOT: &str = "6d3a1c0e-9b2f-4e57-a1c4-2f8e7d6b5a49";
const BOOT_UNIX: u64 = 1_795_600_000;
const INTEL: &str = include_str!("../tests/fixtures/kernel/kmsg-intel.txt");
const AMD: &str = include_str!("../tests/fixtures/kernel/kmsg-amd.txt");
const JOURNAL: &[u8] = include_bytes!("../tests/fixtures/kernel/journal-previous-boot.json");

/// The lines of a `/dev/kmsg` fixture, of the boot `BOOT`.
fn kmsg_lines(fixture: &str) -> Vec<KernelLine> {
    parse_kmsg(fixture.as_bytes())
        .into_iter()
        .map(|record| KernelLine {
            boot_id: BOOT.to_owned(),
            usec: record.usec,
            time_unix: BOOT_UNIX + record.usec / 1_000_000,
            text: record.text,
        })
        .collect()
}

/// Lines of `BOOT` with the texts given, one second apart.
fn lines_of(texts: &[&str]) -> Vec<KernelLine> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let usec = (u64::try_from(index).expect("an index") + 1) * 1_000_000;
            KernelLine {
                boot_id: BOOT.to_owned(),
                usec,
                time_unix: BOOT_UNIX + usec / 1_000_000,
                text: (*text).to_owned(),
            }
        })
        .collect()
}

/// The record a finding of the first line `line` is, of this boot.
fn rec(
    line: &KernelLine,
    raw: &str,
    component: pb::HardwareComponent,
    severity: pb::HardwareSeverity,
    subject: &str,
    code: &str,
    message: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: line_id(line),
        time_unix: line.time_unix,
        of_previous_boot: false,
        component: component.into(),
        subject: subject.to_owned(),
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "journal".to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: String::new(),
    }
}

/// `event` marked as of the boot before.
fn previous(mut event: pb::HardwareEvent) -> pb::HardwareEvent {
    event.of_previous_boot = true;
    event
}

fn kmsg(seq: u64, usec: u64, text: &str) -> KmsgRecord {
    KmsgRecord {
        seq,
        usec,
        text: text.to_owned(),
    }
}

/// Each record has its sequence number, stamp and text.
#[test]
fn kmsg_records_carry_sequence_stamp_and_text() {
    let records = parse_kmsg(b"6,0,0,-;first\n4,1,523000,-;second line\n");
    assert_eq!(
        records,
        [kmsg(0, 0, "first"), kmsg(1, 523_000, "second line")]
    );
}

/// Continuation lines (a leading space), blank lines and lines with no `;`, a bad priority,
/// sequence or stamp, or too few fields are skipped.
#[test]
fn kmsg_continuation_and_unparsable_lines_are_skipped() {
    let input = b"no semicolon at all\nx,1,2,-;bad priority\n6,x,2,-;bad seq\n\
                  6,1,x,-;bad usec\n6,1;short\n\n6,0,5,-;a\n KEY=val\n SECOND=x\n6,2,3,-;ok\n";
    assert_eq!(parse_kmsg(input), [kmsg(0, 5, "a"), kmsg(2, 3, "ok")]);
    assert_eq!(parse_kmsg(b""), Vec::new());
}

/// The text is everything after the first `;`, fields after the stamp are accepted, and bytes
/// that are not UTF-8 become the replacement character.
#[test]
fn kmsg_text_keeps_later_semicolons_and_lossy_utf8() {
    let records = parse_kmsg(b"6,0,1,-,caller=T1;a;b\n6,1,2,-;a\xffb\n");
    assert_eq!(records, [kmsg(0, 1, "a;b"), kmsg(1, 2, "a\u{fffd}b")]);
}

/// `\xNN` escapes of either hex case become their byte.
#[test]
fn unescape_undoes_hex_escapes() {
    assert_eq!(unescape("tab\\x09end"), "tab\tend");
    assert_eq!(unescape("a\\x5cb"), "a\\b");
    assert_eq!(unescape("\\x4A\\x4a"), "JJ");
    assert_eq!(unescape("caf\\xc3\\xa9"), "caf\u{e9}");
    assert_eq!(unescape("plain"), "plain");
    assert_eq!(unescape(""), "");
}

/// An escape that is not two hex digits (short, not hex, or not ASCII) is kept as written.
#[test]
fn unescape_keeps_malformed_escapes() {
    for kept in ["\\xZZ", "\\x4", "end\\x", "\\x1g", "a\\", "\\x\u{e9}"] {
        assert_eq!(unescape(kept), kept);
    }
}

/// A journal object with a string message gives its boot, stamp, seconds and text.
#[test]
fn journal_string_message_becomes_a_line() {
    let input = br#"{"MESSAGE":"hello","_BOOT_ID":"abc","_SOURCE_MONOTONIC_TIMESTAMP":"5","__REALTIME_TIMESTAMP":"1795661999123456"}
"#;
    assert_eq!(
        parse_journal(input),
        [KernelLine {
            boot_id: "abc".to_owned(),
            usec: 5,
            time_unix: 1_795_661_999,
            text: "hello".to_owned(),
        }]
    );
}

/// A message that is an array of bytes is decoded, its entries that are not bytes dropped; with
/// no boot id the boot is empty, and with no realtime stamp the seconds are 0.
#[test]
fn journal_byte_array_message_is_decoded() {
    let input = br#"{"MESSAGE":[104,105,300,"x",33],"_SOURCE_MONOTONIC_TIMESTAMP":"1"}"#;
    let lines = parse_journal(input);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "hi!");
    assert_eq!((lines[0].boot_id.as_str(), lines[0].time_unix), ("", 0));
}

/// An object whose message is null, missing, a number or an object, or with no usable stamp, is skipped.
#[test]
fn journal_object_with_no_text_or_no_stamp_is_skipped() {
    let input = br#"{"MESSAGE":null,"_SOURCE_MONOTONIC_TIMESTAMP":"1"}
{"_SOURCE_MONOTONIC_TIMESTAMP":"2"}
{"MESSAGE":5,"_SOURCE_MONOTONIC_TIMESTAMP":"3"}
{"MESSAGE":{"a":1},"_SOURCE_MONOTONIC_TIMESTAMP":"4"}
{"MESSAGE":"kept","_SOURCE_MONOTONIC_TIMESTAMP":"5"}
{"MESSAGE":"no stamp"}
{"MESSAGE":"stamp not a number","__MONOTONIC_TIMESTAMP":"abc"}
{"MESSAGE":"stamp not a string","_SOURCE_MONOTONIC_TIMESTAMP":5}
"#;
    let lines = parse_journal(input);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "kept");
}

/// The source stamp is used when it is a number, else the journal's own monotonic stamp.
#[test]
fn journal_monotonic_stamp_is_the_fallback_of_the_source_stamp() {
    let input = br#"{"MESSAGE":"a","__MONOTONIC_TIMESTAMP":"7"}
{"MESSAGE":"b","_SOURCE_MONOTONIC_TIMESTAMP":"3","__MONOTONIC_TIMESTAMP":"9"}
{"MESSAGE":"c","_SOURCE_MONOTONIC_TIMESTAMP":"x","__MONOTONIC_TIMESTAMP":"11"}
"#;
    let got: Vec<(String, u64)> = parse_journal(input)
        .into_iter()
        .map(|line| (line.text, line.usec))
        .collect();
    assert_eq!(
        got,
        [
            ("b".to_owned(), 3),
            ("a".to_owned(), 7),
            ("c".to_owned(), 11)
        ]
    );
}

/// Lines come out in the order of the kernel's stamps, not of the output.
#[test]
fn journal_lines_are_sorted_by_kernel_stamp() {
    let input = br#"{"MESSAGE":"third","__MONOTONIC_TIMESTAMP":"30"}
{"MESSAGE":"first","__MONOTONIC_TIMESTAMP":"10"}
{"MESSAGE":"second","__MONOTONIC_TIMESTAMP":"20"}
"#;
    let texts: Vec<String> = parse_journal(input)
        .into_iter()
        .map(|line| line.text)
        .collect();
    assert_eq!(texts, ["first", "second", "third"]);
}

/// A cut last line, text that is not JSON, JSON that is not an object and blank lines are skipped.
#[test]
fn journal_cut_last_line_and_other_lines_are_skipped() {
    let input = br#"{"MESSAGE":"ok","__MONOTONIC_TIMESTAMP":"1"}
not json
[1,2]

{"MESSAGE":"cut","__MONOTONIC_TIMESTAMP":"9","__REALT"#;
    let lines = parse_journal(input);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "ok");
    assert_eq!(parse_journal(b""), Vec::new());
}

/// A boot key is 16 lowercase hex digits with the dashes dropped; a short id keeps what it has,
/// and an id with no hex digit is `unknown`.
#[test]
fn boot_key_strips_dashes_lowercases_and_keeps_sixteen_digits() {
    assert_eq!(boot_key(BOOT), "6d3a1c0e9b2f4e57");
    assert_eq!(
        boot_key("6D3A1C0E-9B2F-4E57-A1C4-2F8E7D6B5A49"),
        "6d3a1c0e9b2f4e57"
    );
    assert_eq!(
        boot_key("6d3a1c0e9b2f4e57a1c42f8e7d6b5a49"),
        "6d3a1c0e9b2f4e57"
    );
    assert_eq!(boot_key("ABC"), "abc");
    assert_eq!(boot_key(""), "unknown");
    assert_eq!(boot_key("unknown"), "unknown");
    assert_eq!(boot_key("--"), "unknown");
}

/// The same boot is recognised with or without dashes and in either case; an empty id names none.
#[test]
fn same_boot_ignores_dashes_and_case() {
    assert!(same_boot(BOOT, "6d3a1c0e9b2f4e57a1c42f8e7d6b5a49"));
    assert!(same_boot("6D3A1C0E9B2F4E57A1C42F8E7D6B5A49", BOOT));
    assert!(same_boot(BOOT, BOOT));
    assert!(!same_boot(BOOT, "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"));
    assert!(!same_boot("", ""));
    assert!(!same_boot("", BOOT));
    assert!(!same_boot(BOOT, ""));
}

/// A PCI address is exactly `dddd:bb:dd.f` in hex, with a dot before the function.
#[test]
fn is_pci_address_accepts_only_full_addresses() {
    assert!(is_pci_address("0000:3b:00.0"));
    assert!(is_pci_address("0000:AF:00.1"));
    for bad in [
        "",
        "000:3b:00.0",
        "0000:3b:00.00",
        "0000-3b-00.0",
        "0000:3b:00:0",
        "0000:3g:00.0",
        "id=00e0",
    ] {
        assert!(!is_pci_address(bad), "{bad:?} is not an address");
    }
}

/// Disks are physical; empty names and loop, ram, zram, dm-, md, nbd and sr devices are not.
#[test]
fn is_physical_rejects_virtual_block_devices_and_empty_names() {
    for disk in ["sda", "nvme0n1", "vda", "mmcblk0"] {
        assert!(is_physical(disk), "{disk} is physical");
    }
    for other in ["", "loop3", "ram0", "zram0", "dm-0", "md127", "nbd0", "sr0"] {
        assert!(!is_physical(other), "{other:?} is not physical");
    }
}

/// A record id is `journal:<boot key>:<usec>:<8 hex digits of the text's SHA-256>`, the digest
/// being the first four bytes of the hash.
#[test]
fn line_id_has_the_pinned_format() {
    let line = KernelLine {
        boot_id: BOOT.to_owned(),
        usec: 523_100,
        time_unix: 1,
        text: "[Hardware Error]: event severity: fatal".to_owned(),
    };
    assert_eq!(line_id(&line), "journal:6d3a1c0e9b2f4e57:523100:436d24d1");
    assert_eq!(digest8(""), "e3b0c442");
    assert_eq!(digest8("abc"), "ba7816bf");
}

/// The same line has the same id whether its boot is written with dashes (kmsg) or without
/// (journal), and whatever its wall-clock time.
#[test]
fn line_id_is_the_same_for_a_line_read_from_kmsg_and_from_the_journal() {
    let from_kmsg = KernelLine {
        boot_id: BOOT.to_owned(),
        usec: 42,
        time_unix: 1,
        text: "x".to_owned(),
    };
    let from_journal = KernelLine {
        boot_id: "6D3A1C0E9B2F4E57A1C42F8E7D6B5A49".to_owned(),
        time_unix: 999,
        ..from_kmsg.clone()
    };
    assert_eq!(line_id(&from_kmsg), line_id(&from_journal));
    let other_text = KernelLine {
        text: "y".to_owned(),
        ..from_kmsg.clone()
    };
    assert_ne!(line_id(&from_kmsg), line_id(&other_text));
}

/// The Intel fixture parses to 33 records in order, its continuation lines passed over and its
/// escapes undone.
#[test]
fn intel_kmsg_fixture_parses_every_record_and_undoes_escapes() {
    let records = parse_kmsg(INTEL.as_bytes());
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (0..33).collect::<Vec<u64>>());
    assert_eq!(
        records[1],
        kmsg(1, 523_000, "BERT: Error records from previous boot:")
    );
    assert_eq!(
        records[30],
        kmsg(
            30,
            42_000_000,
            "systemd[1]: a tab\tand a backslash \\ in a line"
        )
    );
}

/// The Intel fixture's 33 lines are 13 records: BERT's memory error of the boot before, machine
/// checks, coalesced EDAC corrected errors, AER, an APEI PCIe error, coalesced I/O errors of one
/// disk (not the loop device), and the panic and memory machine check at the end.
#[test]
fn intel_kmsg_fixture_classifies_into_thirteen_records() {
    let lines = kmsg_lines(INTEL);
    let t = |i: usize| lines[i].text.as_str();
    let expected = [
        previous(rec(
            &lines[2],
            "[Hardware Error]: event severity: fatal | [Hardware Error]:   section_type: memory error | \
             [Hardware Error]:   DIMM location: DIMM D1",
            Memory,
            Critical,
            "DIMM D1",
            "memory_uncorrectable",
            "Uncorrectable memory error on DIMM D1 (BERT)",
        )),
        rec(
            &lines[7],
            t(7),
            Cpu,
            Info,
            "CPU",
            "cpu_corrected",
            "Corrected machine check events logged",
        ),
        rec(
            &lines[8],
            t(8),
            Cpu,
            Info,
            "CPU 3",
            "cpu_corrected",
            "Corrected machine check on CPU 3, bank 7",
        ),
        rec(
            &lines[11],
            t(11),
            Cpu,
            Critical,
            "CPU 12",
            "cpu_machine_check",
            "Uncorrected machine check on CPU 12, bank 4",
        ),
        rec(
            &lines[12],
            t(13),
            Memory,
            Info,
            "CPU_SrcID#0_MC#0_Chan#1_DIMM#0",
            "memory_corrected",
            "Corrected memory error on CPU_SrcID#0_MC#0_Chan#1_DIMM#0 (EDAC), 4 times",
        ),
        rec(
            &lines[14],
            t(14),
            Memory,
            Critical,
            "CPU_SrcID#1_MC#1_Chan#2_DIMM#0",
            "memory_uncorrectable",
            "Uncorrectable memory error on CPU_SrcID#1_MC#1_Chan#2_DIMM#0 (EDAC)",
        ),
        rec(
            &lines[15],
            t(15),
            Pcie,
            Info,
            "0000:3b:00.0",
            "pcie_corrected",
            "Corrected PCIe error on 0000:3b:00.0 (AER)",
        ),
        rec(
            &lines[17],
            t(17),
            Pcie,
            Critical,
            "0000:af:00.0",
            "pcie_fatal",
            "Fatal PCIe error on 0000:af:00.0 (AER)",
        ),
        rec(
            &lines[18],
            t(18),
            Pcie,
            Critical,
            "0000:af:00.1",
            "pcie_uncorrected",
            "Uncorrected PCIe error on 0000:af:00.1 (AER)",
        ),
        rec(
            &lines[19],
            "{1}[Hardware Error]: event severity: corrected | {1}[Hardware Error]:   section_type: PCIe error | \
             {1}[Hardware Error]:   device_id: 0000:5e:00.0",
            Pcie,
            Info,
            "0000:5e:00.0",
            "pcie_corrected",
            "Corrected PCIe error on 0000:5e:00.0 (APEI)",
        ),
        rec(
            &lines[25],
            t(26),
            Disk,
            Warning,
            "sdb",
            "io_error",
            "I/O error on disk sdb, 2 times",
        ),
        rec(
            &lines[31],
            t(31),
            Cpu,
            Critical,
            "CPU",
            "cpu_machine_check",
            "Kernel panic on a fatal machine check",
        ),
        rec(
            &lines[32],
            t(32),
            Memory,
            Critical,
            "memory",
            "memory_uncorrectable",
            "Uncorrected hardware memory error (machine check)",
        ),
    ];
    assert_eq!(classify(&lines, false), expected);
}

/// Every record of a read of the boot before is marked, BERT's and the others.
#[test]
fn classify_marks_every_record_of_the_previous_boot_when_asked() {
    let lines = kmsg_lines(INTEL);
    let current = classify(&lines, false);
    let before = classify(&lines, true);
    assert_eq!(before.len(), 13);
    assert!(before.iter().all(|event| event.of_previous_boot));
    assert_eq!(current.iter().filter(|e| e.of_previous_boot).count(), 1);
    let ids = |events: &[pb::HardwareEvent]| -> Vec<String> {
        events.iter().map(|e| e.event_id.clone()).collect()
    };
    assert_eq!(ids(&current), ids(&before));
}

/// AMD's decoder lines are records: the corrected line folds into the machine check events line
/// before it, the uncorrected line is critical, and the decoder's detail lines give nothing.
#[test]
fn amd_kmsg_fixture_classifies_decoder_lines() {
    let lines = kmsg_lines(AMD);
    let expected = [
        rec(
            &lines[0],
            &lines[1].text,
            Cpu,
            Info,
            "CPU",
            "cpu_corrected",
            "Corrected machine check events logged, 2 times",
        ),
        rec(
            &lines[6],
            &lines[6].text,
            Cpu,
            Critical,
            "CPU",
            "cpu_machine_check",
            "Uncorrected machine check error (MCE decoder)",
        ),
    ];
    assert_eq!(classify(&lines, false), expected);
}

/// The journal fixture parses to six lines in stamp order, the object with no source stamp
/// using the journal's own stamp.
#[test]
fn journal_fixture_parses_sorted_lines_with_the_stamp_fallback() {
    let lines = parse_journal(JOURNAL);
    let usecs: Vec<u64> = lines.iter().map(|l| l.usec).collect();
    let sorted = [85_900_000_000, 86_000_000_000, 86_250_000_000];
    assert_eq!(usecs[..3], sorted);
    assert_eq!(usecs.len(), 6);
    assert_eq!(lines[0].boot_id, "0f1e2d3c4b5a69788796a5b4c3d2e1f0");
    assert_eq!(lines[0].time_unix, 1_795_661_500);
    assert_eq!(
        lines[3].text,
        "I/O error, dev nvme1n1, sector 2048 op 0x1:(WRITE) flags 0x0 phys_seg 1 prio class 2"
    );
}

/// The journal of the boot before is five records, all of the previous boot: the NIC line and the
/// null message give nothing.
#[test]
fn journal_fixture_classifies_as_records_of_the_previous_boot() {
    let lines = parse_journal(JOURNAL);
    let t = |i: usize| lines[i].text.as_str();
    let expected = [
        previous(rec(
            &lines[0],
            t(0),
            Pcie,
            Info,
            "0000:3b:00.0",
            "pcie_corrected",
            "Corrected PCIe error on 0000:3b:00.0 (AER)",
        )),
        previous(rec(
            &lines[1],
            t(1),
            Memory,
            Info,
            "DIMM_D1",
            "memory_corrected",
            "Corrected memory error on DIMM_D1 (EDAC)",
        )),
        previous(rec(
            &lines[3],
            t(3),
            Disk,
            Warning,
            "nvme1n1",
            "io_error",
            "I/O error on disk nvme1n1",
        )),
        previous(rec(
            &lines[4],
            t(4),
            Cpu,
            Critical,
            "CPU 0",
            "cpu_machine_check",
            "Uncorrected machine check on CPU 0, bank 13",
        )),
        previous(rec(
            &lines[5],
            t(5),
            Memory,
            Critical,
            "DIMM_D1",
            "memory_uncorrectable",
            "Uncorrectable memory error on DIMM_D1 (EDAC)",
        )),
    ];
    assert_eq!(classify(&lines, true), expected);
    assert_eq!(
        expected[0].event_id,
        format!("journal:0f1e2d3c4b5a6978:85900000000:{}", digest8(t(0)))
    );
}

/// The records of `texts`, and the lines they were read from.
fn read(texts: &[&str]) -> (Vec<KernelLine>, Vec<pb::HardwareEvent>) {
    let lines = lines_of(texts);
    let events = classify(&lines, false);
    (lines, events)
}

/// Asserts that `texts` are one record of the current boot, first line `texts[0]`, whose raw text
/// is all of them joined by ` | `.
fn check_one(
    texts: &[&str],
    component: pb::HardwareComponent,
    severity: pb::HardwareSeverity,
    subject: &str,
    code: &str,
    message: &str,
) {
    let (lines, events) = read(texts);
    let raw = texts.join(" | ");
    let expected = rec(&lines[0], &raw, component, severity, subject, code, message);
    assert_eq!(events, [expected], "{texts:?}");
}

/// The subject, code and message of each of `events`.
fn brief(events: &[pb::HardwareEvent]) -> Vec<(&str, &str, &str)> {
    events
        .iter()
        .map(|e| (e.subject.as_str(), e.code.as_str(), e.message.as_str()))
        .collect()
}

/// Asserts that `texts` give no record.
fn check_none(texts: &[&str]) {
    assert_eq!(read(texts).1, Vec::new(), "{texts:?}");
}

/// EDAC lines with no ` on <label>` are about `memory`, a count of 0 counts as one error (no
/// count in the message), and the first of ` UE ` and ` CE ` decides the kind.
#[test]
fn edac_label_count_and_kind_edge_cases() {
    check_one(
        &["EDAC MC0: 1 CE memory read error (channel:1 slot:0)"],
        Memory,
        Info,
        "memory",
        "memory_corrected",
        "Corrected memory error on memory (EDAC)",
    );
    check_one(
        &["EDAC MC0: 0 CE memory read error on DIMM_A1 (channel:1)"],
        Memory,
        Info,
        "DIMM_A1",
        "memory_corrected",
        "Corrected memory error on DIMM_A1 (EDAC)",
    );
    check_one(
        &["EDAC MC0: 1 UE memory error on DIMM_B2 (note: 1 CE logged)"],
        Memory,
        Critical,
        "DIMM_B2",
        "memory_uncorrectable",
        "Uncorrectable memory error on DIMM_B2 (EDAC)",
    );
}

/// An EDAC line with no count, with neither kind, or that does not begin with `EDAC ` gives nothing.
#[test]
fn edac_lines_that_do_not_say_a_count_and_a_kind_give_nothing() {
    check_none(&["EDAC MC0: CE memory read error on DIMM_A1 (x)"]);
    check_none(&["EDAC MC0: 1 XX memory read error on DIMM_A1 (x)"]);
    check_none(&["xEDAC MC0: 1 CE memory read error on DIMM_A1 (x)"]);
    check_none(&["EDAC MC0: Giving out device to module sb_edac controller"]);
}

/// An AER line that names no PCI address falls back to the port that logged it, and with no
/// address at all is about `pcie`; unknown kinds and other AER lines give nothing.
#[test]
fn aer_subject_fallbacks_and_unknown_kinds() {
    check_one(
        &["pcieport 0000:00:1c.0: AER: Corrected error received: id=00e0"],
        Pcie,
        Info,
        "0000:00:1c.0",
        "pcie_corrected",
        "Corrected PCIe error on 0000:00:1c.0 (AER)",
    );
    check_one(
        &["AER: Corrected error received: id=00e0"],
        Pcie,
        Info,
        "pcie",
        "pcie_corrected",
        "Corrected PCIe error on pcie (AER)",
    );
    check_none(&["pcieport 0000:00:1c.0: AER: Multiple error received: 0000:3b:00.0"]);
    check_none(&["pcieport 0000:00:1c.0: AER: aer_status: 0x00000001, aer_mask: 0x0"]);
}

/// A status that does not parse is judged by the word `Exception`; a status that parses decides
/// by its UC bit (with or without `0x`) over the wording.
#[test]
fn machine_check_severity_from_status_or_wording() {
    let mce = |rest: &str| format!("mce: [Hardware Error]: CPU 4: Machine Check{rest}");
    let unparsable_exception = mce(" Exception: 5 Bank 2: zzzz");
    let unparsable = mce(": 0 Bank 2: zzzz");
    let uc_bit = mce(": 0 Bank 1: 0x2000000000000000");
    let exception_without_uc = mce(" Exception: 5 Bank 1: 0000000000000001");
    let message = |what: &str, bank: &str| format!("{what} machine check on CPU 4, bank {bank}");
    let cases = [
        (
            &unparsable_exception,
            Critical,
            "cpu_machine_check",
            message("Uncorrected", "2"),
        ),
        (
            &unparsable,
            Info,
            "cpu_corrected",
            message("Corrected", "2"),
        ),
        (
            &uc_bit,
            Critical,
            "cpu_machine_check",
            message("Uncorrected", "1"),
        ),
        (
            &exception_without_uc,
            Info,
            "cpu_corrected",
            message("Corrected", "1"),
        ),
    ];
    for (text, severity, code, message) in cases {
        check_one(&[text.as_str()], Cpu, severity, "CPU 4", code, &message);
    }
}

/// A machine check line with no `Bank` has a message with no bank.
#[test]
fn machine_check_without_a_bank_has_no_bank_in_its_message() {
    check_one(
        &["mce: [Hardware Error]: CPU 5: Machine Check Exception: 4"],
        Cpu,
        Critical,
        "CPU 5",
        "cpu_machine_check",
        "Uncorrected machine check on CPU 5",
    );
    check_one(
        &["mce: [Hardware Error]: CPU 6: Machine Check: 0"],
        Cpu,
        Info,
        "CPU 6",
        "cpu_corrected",
        "Corrected machine check on CPU 6",
    );
}

/// The other `mce:` lines, a CPU line that is no machine check, and a panic that names none give nothing.
#[test]
fn machine_check_detail_lines_and_other_panics_give_nothing() {
    check_none(&["mce: [Hardware Error]: TSC 0 ADDR 4a2b3c4000 MISC 2140040486"]);
    check_none(&["mce: [Hardware Error]: PROCESSOR 0:50654 TIME 1 SOCKET 0 APIC 6"]);
    check_none(&["mce: [Hardware Error]: CPU 5: something else entirely"]);
    check_none(&["Kernel panic - not syncing: VFS: Unable to mount root fs"]);
}

/// A fatal general processor error is a critical `cpu_machine_check` of `CPU`; a recoverable
/// error of an unknown section is a critical `board` record naming the section as written; a
/// corrected one with no section has no section in its message.
#[test]
fn apei_processor_unknown_and_missing_sections() {
    check_one(
        &[
            "{2}[Hardware Error]: event severity: fatal",
            "{2}[Hardware Error]:   section_type: general processor error",
        ],
        Cpu,
        Critical,
        "CPU",
        "cpu_machine_check",
        "Uncorrected processor error on CPU (APEI)",
    );
    check_one(
        &[
            "{3}[Hardware Error]: event severity: recoverable",
            "{3}[Hardware Error]:   section_type: Vendor Specific",
        ],
        Board,
        Critical,
        "board",
        "hardware_error",
        "Uncorrected hardware error (Vendor Specific) (APEI)",
    );
    check_one(
        &["{4}[Hardware Error]: event severity: corrected"],
        Board,
        Info,
        "board",
        "hardware_corrected",
        "Corrected hardware error (APEI)",
    );
}

/// An APEI or BERT error with no `event severity:` line gives nothing.
#[test]
fn apei_and_bert_errors_without_a_severity_give_nothing() {
    check_none(&[
        "{5}[Hardware Error]: Hardware error from APEI Generic Hardware Error Source: 5",
        "{5}[Hardware Error]:   section_type: memory error",
    ]);
    check_none(&["[Hardware Error]:   section_type: memory error"]);
}

/// A second `event severity:` line of the same `{n}` begins another error, so two errors are two
/// records, each with its own lines as raw text and its own first line's id.
#[test]
fn apei_second_severity_line_of_one_source_begins_another_record() {
    let texts = [
        "{6}[Hardware Error]: event severity: corrected",
        "{6}[Hardware Error]:   section_type: memory error",
        "{6}[Hardware Error]:   DIMM location: DIMM A1",
        "{6}[Hardware Error]: event severity: fatal",
        "{6}[Hardware Error]:   section_type: memory error",
        "{6}[Hardware Error]:   DIMM location: DIMM A2",
    ];
    let (lines, events) = read(&texts);
    assert_eq!(
        brief(&events),
        [
            (
                "DIMM A1",
                "memory_corrected",
                "Corrected memory error on DIMM A1 (APEI)"
            ),
            (
                "DIMM A2",
                "memory_uncorrectable",
                "Uncorrectable memory error on DIMM A2 (APEI)"
            ),
        ]
    );
    assert_eq!(events[0].raw, texts[..3].join(" | "));
    assert_eq!(events[1].raw, texts[3..].join(" | "));
    assert_eq!(events[0].event_id, line_id(&lines[0]));
    assert_eq!(events[1].event_id, line_id(&lines[3]));
}

/// A recoverable PCIe error is `pcie_uncorrected` and a fatal one `pcie_fatal`.
#[test]
fn apei_recoverable_pcie_is_uncorrected_and_fatal_pcie_is_fatal() {
    check_one(
        &[
            "{7}[Hardware Error]: event severity: recoverable",
            "{7}[Hardware Error]:   section_type: PCIe error",
            "{7}[Hardware Error]:   device_id: 0000:5e:00.0",
        ],
        Pcie,
        Critical,
        "0000:5e:00.0",
        "pcie_uncorrected",
        "Uncorrected PCIe error on 0000:5e:00.0 (APEI)",
    );
    check_one(
        &[
            "{8}[Hardware Error]: event severity: fatal",
            "{8}[Hardware Error]:   section_type: PCIe error",
            "{8}[Hardware Error]:   device_id: 0000:5e:00.1",
        ],
        Pcie,
        Critical,
        "0000:5e:00.1",
        "pcie_fatal",
        "Fatal PCIe error on 0000:5e:00.1 (APEI)",
    );
}

/// A `device_id:` that is no PCI address is passed over: the subject is `pcie`, and the line is not in `raw`.
#[test]
fn apei_pcie_device_that_is_no_address_is_about_pcie() {
    let texts = [
        "{9}[Hardware Error]: event severity: corrected",
        "{9}[Hardware Error]:   section_type: PCIe error",
        "{9}[Hardware Error]:   device_id: bogus",
    ];
    let events = read(&texts).1;
    assert_eq!(
        brief(&events),
        [(
            "pcie",
            "pcie_corrected",
            "Corrected PCIe error on pcie (APEI)"
        )]
    );
    assert_eq!(events[0].raw, texts[..2].join(" | "));
}

/// A BERT memory error with no DIMM location, severity and section in any case, is about
/// `memory` and of the boot before.
#[test]
fn bert_memory_error_without_a_location_is_about_memory_of_the_previous_boot() {
    let texts = [
        "[Hardware Error]: event severity: Corrected",
        "[Hardware Error]:   section_type: Memory Error",
    ];
    let (lines, events) = read(&texts);
    let expected = rec(
        &lines[0],
        &texts.join(" | "),
        Memory,
        Info,
        "memory",
        "memory_corrected",
        "Corrected memory error on memory (BERT)",
    );
    assert_eq!(events, [previous(expected)]);
}

/// A BERT error and an APEI error of the same DIMM are two records, since one is of the boot before.
#[test]
fn coalescing_keeps_apart_records_of_different_boots() {
    let texts = [
        "[Hardware Error]: event severity: corrected",
        "[Hardware Error]:   section_type: memory error",
        "[Hardware Error]:   DIMM location: DIMM A1",
        "{1}[Hardware Error]: event severity: corrected",
        "{1}[Hardware Error]:   section_type: memory error",
        "{1}[Hardware Error]:   DIMM location: DIMM A1",
    ];
    let events = read(&texts).1;
    let boots: Vec<bool> = events.iter().map(|e| e.of_previous_boot).collect();
    assert_eq!(boots, [true, false]);
    assert_eq!(
        events[0].message,
        "Corrected memory error on DIMM A1 (BERT)"
    );
    assert_eq!(
        events[1].message,
        "Corrected memory error on DIMM A1 (APEI)"
    );
}

/// Corrected errors of one DIMM fold into the first record even with another DIMM's between them;
/// each DIMM is its own record, and the folded one has the last line as raw text.
#[test]
fn coalescing_keeps_apart_records_of_different_subjects() {
    let texts = [
        "EDAC MC0: 1 CE memory read error on DIMM_A (x)",
        "EDAC MC0: 1 CE memory read error on DIMM_B (x)",
        "EDAC MC0: 1 CE memory read error on DIMM_A (y)",
    ];
    let events = read(&texts).1;
    assert_eq!(
        brief(&events),
        [
            (
                "DIMM_A",
                "memory_corrected",
                "Corrected memory error on DIMM_A (EDAC), 2 times"
            ),
            (
                "DIMM_B",
                "memory_corrected",
                "Corrected memory error on DIMM_B (EDAC)"
            ),
        ]
    );
    assert_eq!(events[0].raw, texts[2]);
}

/// A storm of I/O errors of one disk is one record: the first line's id, the last line's text,
/// and the number of errors.
#[test]
fn a_storm_of_io_errors_is_one_record_with_its_count() {
    let texts: Vec<String> = (0..100)
        .map(|n| format!("I/O error, dev sda, sector {n} op 0x0:(READ) flags 0x0"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let (lines, events) = read(&refs);
    assert_eq!(
        brief(&events),
        [("sda", "io_error", "I/O error on disk sda, 100 times")]
    );
    assert_eq!(events[0].raw, texts[99]);
    assert_eq!(events[0].event_id, line_id(&lines[0]));
}

/// I/O errors of virtual block devices, and a line with no device, give nothing.
#[test]
fn io_errors_of_virtual_devices_give_nothing() {
    check_none(&["I/O error, dev loop3, sector 0"]);
    check_none(&["I/O error, dev zram0, sector 0"]);
    check_none(&["I/O error, dev dm-1, sector 0"]);
    check_none(&["I/O error, dev , sector 0"]);
}
