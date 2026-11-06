//! Reading a page of the audit file without reading the rest: the newest entries from its end,
//! the oldest after a `since_seq` by bisection, each through a reader that counts the bytes it
//! hands out, and every boundary of the chunked reads (a line across chunks, a line past the
//! cap, a torn or unparseable line, empty lines). Nothing here times anything.

use std::fs::File;
use std::path::Path;

use std::num::{NonZeroU64, NonZeroUsize};

use super::{
    AuditLine, AuditLog, CHUNK, Logged, MAX_LINE, Query, Reader, Rotation, backward, forward,
    line_from, page_in, segment_path, segments, start_after,
};
use crate::ops::AuditEntry;
use crate::testutil::scratch;

/// The line of entry `seq` by `by`, as the store writes it, at `1_800_000_000 + seq`.
fn line(seq: u64, by: &str) -> String {
    format!(
        "{{\"seq\":{seq},\"at_unix\":{},\"by\":\"{by}\",\"action\":\"exclude\",\"detail\":{{\"service\":\"web\",\"node_id\":\"n{seq}\",\"reason\":\"why {seq}\"}}}}\n",
        1_800_000_000 + seq
    )
}

/// A file holding `text`, open for counted reads, with its length.
fn counted(path: &Path, text: &str) -> (Reader, u64) {
    std::fs::write(path, text).expect("written");
    let file = Reader::new(File::open(path).expect("opened")).expect("its length");
    (file, text.len() as u64)
}

/// A log of entries 1 to `n`, every third by `bob` and the rest by `alice`.
fn log_of(n: u64) -> String {
    (1..=n)
        .map(|seq| line(seq, if seq % 3 == 0 { "bob" } else { "alice" }))
        .collect()
}

fn all(_: &AuditEntry) -> bool {
    true
}

fn query(since_seq: u64, limit: usize, newest: bool) -> Query<'static> {
    Query {
        since_seq,
        limit,
        newest,
        keep: &all,
    }
}

fn seqs(page: &[Logged]) -> Vec<u64> {
    page.iter().map(|l| l.entry.seq).collect()
}

/// Proves: the newest 5 entries are read from the end in about one chunk, the same number of
/// bytes for a log ten times as long, and come back oldest first with their times; a
/// `since_seq` page in the middle returns exactly the entries after it and reads a small part of
/// a large log.
///
/// Catches: a read of the whole file for a page, a tail returned newest first, an entry lost or
/// repeated at the `since_seq` boundary.
#[test]
fn audit_tail_read_is_bounded() {
    let dir = scratch("audit-tail-bounded");
    let mut tail_bytes = Vec::new();
    for n in [2_000_u64, 20_000] {
        let (file, len) = counted(&dir.path().join(format!("audit-{n}")), &log_of(n));
        assert!(len > 10 * CHUNK as u64, "the log spans many chunks");

        let page = page_in(&file, &query(0, 5, true)).expect("read");
        assert_eq!(seqs(&page), ((n - 4)..=n).collect::<Vec<u64>>());
        assert_eq!(page[4].at_unix, Some(1_800_000_000 + n));
        let tail = file.bytes_read();
        assert!(tail <= 2 * CHUNK as u64, "{tail}");
        tail_bytes.push(tail);

        let since = n / 2;
        let page = page_in(&file, &query(since, 7, false)).expect("read");
        assert_eq!(
            seqs(&page),
            ((since + 1)..=(since + 7)).collect::<Vec<u64>>()
        );
        let middle = file.bytes_read() - tail;
        assert!(middle < len / 2, "{middle} of {len}");
    }
    assert_eq!(tail_bytes[0], tail_bytes[1]);
}

/// Proves: `since_seq` at every position of a log of a few chunks (0, inside, at the last
/// entry, past it) returns exactly the entries after it, oldest and newest, through filters.
///
/// Catches: a bisection that skips the first entry after `since_seq` or starts before it, and
/// a newest page that reads past `since_seq`.
#[test]
fn every_since_seq_pages_exactly_the_entries_after_it() {
    let dir = scratch("audit-every-since");
    let n = 600;
    let (file, _) = counted(&dir.path().join("audit"), &log_of(n));
    let bob = |e: &AuditEntry| e.by == "bob";
    for since in [0, 1, 2, 99, 300, 301, 598, 599, 600, 601] {
        let oldest = page_in(&file, &query(since, 3, false)).expect("read");
        let want: Vec<u64> = ((since + 1)..=n).take(3).collect();
        assert_eq!(seqs(&oldest), want, "since {since}");
        let newest = page_in(&file, &query(since, 3, true)).expect("read");
        let after: Vec<u64> = ((since + 1)..=n).collect();
        assert_eq!(
            seqs(&newest),
            after[after.len().saturating_sub(3)..],
            "since {since}"
        );
        let bobs = page_in(
            &file,
            &Query {
                since_seq: since,
                limit: 2,
                newest: true,
                keep: &bob,
            },
        )
        .expect("read");
        let want: Vec<u64> = after.iter().copied().filter(|s| s % 3 == 0).collect();
        assert_eq!(
            seqs(&bobs),
            want[want.len().saturating_sub(2)..],
            "since {since}"
        );
    }
}

/// Proves: an empty file and a missing one list nothing; a page whose filter keeps nothing reads
/// to the end and lists nothing.
///
/// Catches: an empty or missing file that fails, or a filter that stops the read early.
#[test]
fn an_empty_log_and_a_filter_that_keeps_nothing_list_none() {
    let dir = scratch("audit-empty");
    let (file, _) = counted(&dir.path().join("audit"), "");
    for newest in [false, true] {
        assert!(
            page_in(&file, &query(0, 5, newest))
                .expect("read")
                .is_empty()
        );
    }
    let missing = super::page(&dir.path().join("none"), &query(0, 5, false)).expect("none");
    assert!(missing.is_empty());

    let (file, _) = counted(&dir.path().join("full"), &log_of(50));
    let none = |_: &AuditEntry| false;
    for newest in [false, true] {
        let q = Query {
            since_seq: 0,
            limit: 5,
            newest,
            keep: &none,
        };
        assert!(page_in(&file, &q).expect("read").is_empty());
    }
}

/// Proves: lines that do not parse (garbage, a torn last line, a run past [`MAX_LINE`]), and
/// empty lines, are passed over in both directions and by the bisection; a line that crosses
/// chunk boundaries is read whole both ways.
///
/// Catches: a torn line that ends the read, a long run held in memory or taken for a line, a
/// line split at a chunk boundary, or an empty line counted as one that does not parse.
#[test]
fn bad_long_torn_and_split_lines_are_handled_in_both_directions() {
    let dir = scratch("audit-bad-lines");
    // Past the cap by more than two chunks: it keeps growing after it is passed over.
    let long = "x".repeat(MAX_LINE + 2 * CHUNK + 10);
    let mut text = String::new();
    text.push_str(&line(1, "alice"));
    text.push_str("not json\n\n");
    text.push_str(&long);
    text.push('\n');
    // A line whose reason spans more than one chunk.
    let wide = line(2, "alice").replace("why 2", &"w".repeat(CHUNK + 100));
    text.push_str(&wide);
    text.push_str(&log_of(400)[line(1, "alice").len() + line(2, "alice").len()..]);
    text.push_str("{\"seq\":401,\"at_un");
    let (file, _) = counted(&dir.path().join("audit"), &text);

    let oldest = page_in(&file, &query(0, 3, false)).expect("read");
    assert_eq!(seqs(&oldest), vec![1, 2, 3]);
    assert_eq!(
        oldest[1].entry.reason.as_deref().map(str::len),
        Some(CHUNK + 100)
    );
    let newest = page_in(&file, &query(0, 400, true)).expect("read");
    assert_eq!(seqs(&newest), (1..=400).collect::<Vec<u64>>());
    let middle = page_in(&file, &query(200, 2, false)).expect("read");
    assert_eq!(seqs(&middle), vec![201, 202]);

    let path = dir.path().join("audit");
    let mut found = Vec::new();
    let skipped = super::each_in(&path, &mut |l| found.push(l.seq)).expect("read");
    assert_eq!(found, (1..=400).collect::<Vec<u64>>());
    assert_eq!(skipped, 3, "garbage, the long run and the torn line");

    // Backward over a run past the cap at the very start of the file.
    let (file, _) = counted(
        &dir.path().join("long-first"),
        &format!("{long}\n{}", line(1, "a")),
    );
    let mut seen = Vec::new();
    backward(&file, &mut |raw| {
        seen.push(raw.map(<[u8]>::len));
        std::ops::ControlFlow::Continue(())
    })
    .expect("read");
    assert_eq!(seen, vec![Some(line(1, "a").len() - 1), None]);
}

/// Proves: a line found from a byte that is a line's start is that line, not the next one; one
/// found from inside a line is the next; none is found past the bound; and a bisection over a
/// line wider than a chunk, whose probes land inside it, still pages it.
///
/// Catches: an off-by-one at the probe that skips the line starting at it.
#[test]
fn a_probe_at_a_line_start_finds_that_line() {
    let dir = scratch("audit-probe");
    let text = log_of(3);
    let (file, len) = counted(&dir.path().join("audit"), &text);
    let second = line(1, "alice").len() as u64;
    let third = second + line(2, "alice").len() as u64;
    assert_eq!(
        line_from(&file, second, len).expect("read"),
        Some((2, third))
    );
    assert_eq!(
        line_from(&file, second + 1, len).expect("read"),
        Some((3, len))
    );
    assert_eq!(line_from(&file, second + 1, third).expect("read"), None);
    assert_eq!(line_from(&file, len, len + 1).expect("read"), None);

    // A probe whose next line does not parse finds the line after it.
    let text = format!("{}not json\n{}", line(1, "a"), line(2, "a"));
    let (file, len) = counted(&dir.path().join("garbage"), &text);
    assert_eq!(line_from(&file, 1, len).expect("read"), Some((2, len)));

    let wide = line(1, "a").replace("why 1", &"w".repeat(3 * CHUNK));
    let (file, _) = counted(&dir.path().join("wide"), &format!("junk\n{wide}"));
    assert_eq!(start_after(&file, 1).expect("read"), 0);
    assert_eq!(
        seqs(&page_in(&file, &query(0, 5, false)).expect("read")),
        vec![1]
    );
    assert!(
        page_in(&file, &query(1, 5, false))
            .expect("read")
            .is_empty()
    );
}

/// Proves: a read that fails is an error in every direction, not an empty page.
///
/// Catches: an I/O error answered as "no entries".
#[test]
fn a_failed_read_is_an_error() {
    let dir = scratch("audit-read-fails");
    // A directory opens as a file, has a length, and fails every read.
    let broken = Reader::new(File::open(dir.path()).expect("opened")).expect("its length");
    assert!(broken.len > 0);
    assert!(page_in(&broken, &query(0, 5, true)).is_err());
    assert!(page_in(&broken, &query(0, 5, false)).is_err());
    assert!(forward(&broken, 0, &mut |_, _| std::ops::ControlFlow::Continue(())).is_err());
    assert_eq!(broken.bytes_read(), 0);

    // A path under a file cannot be opened, and that is no missing file.
    let file = dir.path().join("plain");
    std::fs::write(&file, "x").expect("written");
    let under = file.join("audit");
    assert!(super::page(&under, &query(0, 5, false)).is_err());
    assert!(super::each(&under, &mut |_| {}).is_err());
}

/// Proves: a page of a log rotated into many segments is read across them in both directions:
/// the oldest from the oldest segment on and the newest from the active file back, each
/// crossing segment boundaries, through a filter, stopping once the page is full; `each` visits
/// every line in order; and with the active file gone (a rotation sealed it and no line came
/// after) the segments still page.
///
/// Catches: a page cut at a segment boundary, a newest page put together in the wrong order,
/// an entry repeated or lost between segments, and a missing active file taken as an error.
#[test]
fn a_page_spans_segments_in_both_directions() {
    let dir = scratch("audit-segments-page");
    let active = dir.path().join("ops.json.audit");
    let one = line(10, "alice").len() as u64;
    let rotation = Rotation {
        max_bytes: NonZeroU64::new(5 * one).expect("not zero"),
        retained: NonZeroUsize::new(100).expect("not zero"),
    };
    let log = AuditLog::open(active.clone(), rotation).expect("opened");
    for seq in 1..=50 {
        let by = if seq % 3 == 0 { "bob" } else { "alice" };
        let parsed: AuditLine = serde_json::from_str(line(seq, by).trim_end()).expect("a line");
        log.append(&parsed).expect("appended");
    }
    assert!(
        segments(&active).expect("listed").len() > 5,
        "many segments"
    );
    let page =
        |since_seq, limit, newest| seqs(&log.page(&query(since_seq, limit, newest)).expect("read"));

    assert_eq!(page(0, 25, false), (1..=25).collect::<Vec<u64>>());
    assert_eq!(page(7, 30, false), (8..=37).collect::<Vec<u64>>());
    assert!(page(50, 5, false).is_empty());
    assert_eq!(page(0, 25, true), (26..=50).collect::<Vec<u64>>());
    assert_eq!(page(0, 3, true), vec![48, 49, 50]);
    assert_eq!(page(44, 10, true), (45..=50).collect::<Vec<u64>>());
    assert!(page(50, 5, true).is_empty());
    let bob = |e: &AuditEntry| e.by == "bob";
    let bobs = log
        .page(&Query {
            since_seq: 0,
            limit: 6,
            newest: true,
            keep: &bob,
        })
        .expect("read");
    assert_eq!(seqs(&bobs), vec![33, 36, 39, 42, 45, 48]);
    let mut all = Vec::new();
    log.each(&mut |line| all.push(line.seq)).expect("read");
    assert_eq!(all, (1..=50).collect::<Vec<u64>>());

    let newest = segments(&active)
        .expect("listed")
        .last()
        .map_or(0, |(n, _)| *n);
    std::fs::rename(&active, segment_path(&active, newest + 1)).expect("sealed by hand");
    assert_eq!(page(0, 4, true), vec![47, 48, 49, 50]);
    assert_eq!(page(45, 50, false), (46..=50).collect::<Vec<u64>>());
}
