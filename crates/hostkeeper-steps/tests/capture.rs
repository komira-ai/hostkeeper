//! What is kept of a hook's output: the last bytes of each stream, and the last line of standard
//! output read as `Report::read` reads it, within its bound.

use std::io::Write;

use hostkeeper_hook::Report;
use hostkeeper_steps::{LAST_LINE_MAX, RING_BYTES, Ring};

fn ring_of(bytes: &[u8]) -> Ring {
    let mut ring = Ring::new();
    ring.push(bytes);
    ring
}

/// Catches a change of the documented limits.
#[test]
fn the_limits_are_one_mib_kept_and_a_64_kib_line() {
    assert_eq!(RING_BYTES, 1_048_576);
    assert_eq!(LAST_LINE_MAX, 65_536);
}

/// Catches a ring that keeps the first bytes instead of the last, loses bytes at the boundary, or
/// says it dropped nothing when it did: writes of odd sizes cross the capacity several times, and
/// after each the ring holds exactly the tail of everything written; then the same at the real
/// capacity, with the last line found in what is kept.
#[test]
fn the_last_line_is_found_across_ring_wraps() {
    let mut ring = Ring::with_capacity(10);
    let mut all = Vec::new();
    for (n, size) in [3, 7, 5, 11, 2, 9, 1, 13, 4].into_iter().enumerate() {
        let chunk: Vec<u8> = (0..size).map(|i| b'a' + ((n * 7 + i) % 26) as u8).collect();
        ring.push(&chunk);
        all.extend_from_slice(&chunk);
        let tail = &all[all.len().saturating_sub(10)..];
        assert_eq!(ring.bytes(), tail, "after {} bytes", all.len());
        assert_eq!(ring.cut(), all.len() > 10, "after {} bytes", all.len());
    }

    let mut ring = Ring::new();
    let mut all = Vec::new();
    let mut n = 0;
    while all.len() < 3 * RING_BYTES + 12_345 {
        let mut chunk = Vec::new();
        // Chunks of an odd size, so no boundary falls on a line.
        while chunk.len() < 65_537 {
            chunk.extend_from_slice(format!("line {n}\n").as_bytes());
            n += 1;
        }
        chunk.truncate(65_537);
        ring.push(&chunk);
        all.extend_from_slice(&chunk);
    }
    all.extend_from_slice(b"{\"reason\":\"done\"}\n");
    ring.push(b"{\"reason\":\"done\"}\n");
    assert_eq!(ring.bytes(), &all[all.len() - RING_BYTES..]);
    assert!(ring.cut());
    assert_eq!(ring.last_line().as_deref(), Some("{\"reason\":\"done\"}"));
}

/// Catches an off-by-one at the bound of the last line: a line of exactly 64 KiB is read, one
/// byte more is not (and its report is `{}`); the line feed and a carriage return before it are
/// not counted.
#[test]
fn a_last_line_of_exactly_64_kib_is_read_and_one_byte_more_is_not() {
    let line = |len: usize| {
        let mut text = b"{\"pad\":\"".to_vec();
        text.resize(len - 2, b'x');
        text.extend_from_slice(b"\"}");
        text
    };
    for (len, end) in [(LAST_LINE_MAX, &b"\n"[..]), (LAST_LINE_MAX, b"\r\n")] {
        let mut out = b"log\n".to_vec();
        out.extend_from_slice(&line(len));
        out.extend_from_slice(end);
        let got = ring_of(&out).last_line().expect("a line of 64 KiB is read");
        assert_eq!(got.len(), LAST_LINE_MAX);
        assert_eq!(got.as_bytes(), line(len));
    }
    let mut out = b"log\n".to_vec();
    out.extend_from_slice(&line(LAST_LINE_MAX + 1));
    out.push(b'\n');
    let ring = ring_of(&out);
    assert_eq!(ring.last_line(), None);
    assert_eq!(
        Report::read(ring.last_line().as_deref().unwrap_or("")),
        Report::default()
    );
}

/// Catches a last line read differently from how `Report::read` reads the whole output (blank
/// lines after it, a carriage return, bytes that are not UTF-8, no line at all): for each sample
/// the report of the kept line is the report of the whole text.
#[test]
fn the_last_line_is_read_as_report_read_reads_the_output() {
    let samples: [(&[u8], Option<&str>); 10] = [
        (b"", None),
        (b"\n\n  \n\t\n", None),
        (
            b"starting\n{\"reason\":\"ok_now\"}\n",
            Some("{\"reason\":\"ok_now\"}"),
        ),
        (b"{\"reason\":\"a\"}\n\n   \n", Some("{\"reason\":\"a\"}")),
        (b"{\"reason\":\"b\"}", Some("{\"reason\":\"b\"}")),
        (b"x\n{\"reason\":\"c\"}\r\n", Some("{\"reason\":\"c\"}")),
        // A carriage return with no line feed after it stays, as `str::lines` leaves it.
        (b"{\"reason\":\"d\"}\r", Some("{\"reason\":\"d\"}\r")),
        (
            b"log\n\xff{\"reason\":\"e\"}\n",
            Some("\u{fffd}{\"reason\":\"e\"}"),
        ),
        (b"one line, no report", Some("one line, no report")),
        (
            b"{\"retry_after_secs\":5}\n\r\n",
            Some("{\"retry_after_secs\":5}"),
        ),
    ];
    for (bytes, want) in samples {
        let ring = ring_of(bytes);
        assert!(!ring.cut());
        let got = ring.last_line();
        assert_eq!(got.as_deref(), want, "{:?}", String::from_utf8_lossy(bytes));
        assert_eq!(
            Report::read(got.as_deref().unwrap_or("")),
            Report::read(&String::from_utf8_lossy(bytes)),
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
}

/// Catches a line whose beginning was dropped being read as the report: once the ring has dropped
/// bytes, its first kept line is not read, and a whole line after it is.
#[test]
fn a_last_line_cut_at_the_start_of_the_ring_is_not_read() {
    let mut ring = Ring::with_capacity(16);
    ring.push(b"{\"reason\":\"long_lost_reason\"}");
    assert!(ring.cut());
    assert_eq!(ring.last_line(), None);
    ring.push(b"\n{\"a\":1}\n\n");
    assert_eq!(ring.bytes(), b"ason\"}\n{\"a\":1}\n\n");
    assert_eq!(ring.last_line().as_deref(), Some("{\"a\":1}"));
    // A cut ring whose only line is blank has no line.
    let mut ring = Ring::with_capacity(4);
    ring.push(b"xxxxxx    ");
    assert_eq!(ring.last_line(), None);
}

/// Catches a ring of no capacity keeping anything, and the writer side of a ring (what
/// `io::copy` uses) not keeping what it was given.
#[test]
fn a_ring_is_a_writer_and_keeps_at_most_its_capacity() {
    let mut ring = Ring::with_capacity(0);
    ring.push(b"gone");
    assert_eq!((ring.bytes(), ring.cut()), (Vec::new(), true));

    let mut ring = Ring::default();
    assert_eq!(ring, Ring::new());
    assert_eq!(ring.write(b"abc").unwrap(), 3);
    ring.flush().unwrap();
    assert_eq!((ring.bytes(), ring.cut()), (b"abc".to_vec(), false));
}
