//! The forms: each row of the table refuses its read however the path is spelled, a rename of a
//! wall-clock name is refused, and the monotonic clocks pass.

use std::collections::BTreeSet;

use crate::scan::FORMS;
use crate::{DECIDE, lint, refusal, renaming, st};

/// Each case: the form it reads, the source, and the line of the read.
fn form_cases() -> Vec<(&'static str, String, usize)> {
    let st = st();
    vec![
        (
            "SystemTime::now",
            format!("let t = std::time::{st}::now();"),
            1,
        ),
        (
            "SystemTime::now",
            format!("let t = ::std::time::{st}::now();"),
            1,
        ),
        (
            "SystemTime::now",
            format!("use std::time::{{Instant, {st}}};\nlet t =\n  {st}::now();"),
            3,
        ),
        (
            "SystemTime::now",
            format!("use std::{{time::{{self}}}};\ntime::{st}::now();"),
            2,
        ),
        (
            "SystemTime::now",
            format!("let t = <std::time::{st}>::now();"),
            1,
        ),
        (
            "SystemTime::now",
            format!("let t = x.unwrap_or_else({st}::now);"),
            1,
        ),
        (
            "SystemTime::now",
            format!("fn f() -> impl Sized + use<> {{\n    {st}::now()\n}}"),
            2,
        ),
        ("SystemTime::now", format!("x.r#use({st}::now());"), 1),
        (
            "SystemTime::now",
            format!("fn horloge_é() {{ {st}::now(); }}"),
            1,
        ),
        (
            "SystemTime::elapsed",
            format!("let d = {st}::elapsed(&t);"),
            1,
        ),
        (
            "UNIX_EPOCH.elapsed",
            "let a = std::time::UNIX_EPOCH.elapsed();".to_owned(),
            1,
        ),
        (
            "UNIX_EPOCH.elapsed",
            "use std::time::UNIX_EPOCH;\nlet a = UNIX_EPOCH\n    .elapsed();".to_owned(),
            2,
        ),
        ("Utc::now", "chrono::Utc::now()".to_owned(), 1),
        (
            "Local::now",
            "use chrono::Local;\nLocal::now()".to_owned(),
            2,
        ),
        (
            "OffsetDateTime::now_utc",
            "time::OffsetDateTime::now_utc()".to_owned(),
            1,
        ),
        (
            "OffsetDateTime::now_local",
            "use time::OffsetDateTime;\nOffsetDateTime::now_local()".to_owned(),
            2,
        ),
        ("Timestamp::now", "jiff::Timestamp::now()".to_owned(), 1),
        ("Zoned::now", "jiff::Zoned::now()".to_owned(), 1),
        (
            "libc::time",
            "libc::time(std::ptr::null_mut())".to_owned(),
            1,
        ),
        (
            "libc::time",
            "use libc::time;\nlet t = time(p);".to_owned(),
            2,
        ),
        ("libc::time", "use libc::*;\nlet t = time(p);".to_owned(), 2),
        (
            "gettimeofday",
            "use libc::gettimeofday;\ngettimeofday(&mut tv, n)".to_owned(),
            2,
        ),
        (
            "CLOCK_REALTIME",
            "libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts)".to_owned(),
            1,
        ),
        (
            "CLOCK_REALTIME_COARSE",
            "clock_gettime(CLOCK_REALTIME_COARSE, &mut ts)".to_owned(),
            1,
        ),
        (
            "CLOCK_REALTIME_ALARM",
            "timerfd_create(libc::CLOCK_REALTIME_ALARM, 0)".to_owned(),
            1,
        ),
        (
            "CLOCK_TAI",
            "clock_gettime(libc::CLOCK_TAI, &mut ts)".to_owned(),
            1,
        ),
        (
            "ClockId::Realtime",
            "use rustix::time::{clock_gettime, ClockId};\nclock_gettime(ClockId::Realtime)"
                .to_owned(),
            2,
        ),
        (
            "ClockId::Realtime",
            "let id = ClockId::Realtime;\nlet t = rustix::time::clock_gettime(id);".to_owned(),
            1,
        ),
        (
            "ClockId::Realtime",
            "let f = rustix::time::clock_gettime;\nf(rustix::time::ClockId::Realtime)".to_owned(),
            2,
        ),
        (
            "ClockId::Realtime",
            "clock_gettime({ let _ = ClockId::Realtime; ClockId::Monotonic })".to_owned(),
            1,
        ),
        (
            "ClockId::Realtime",
            "clock_gettime_dynamic(DynamicClockId::Known(ClockId::Realtime))".to_owned(),
            1,
        ),
        (
            "ClockId::Realtime",
            "use rustix::time::ClockId::*;\nfn f() { rustix::time::clock_gettime(Realtime); }"
                .to_owned(),
            2,
        ),
        (
            "ClockId::Realtime",
            "use rustix::time::ClockId::*;\nfn f() {\n    let id = Realtime;\n    \
             rustix::time::clock_gettime(id);\n}"
                .to_owned(),
            3,
        ),
        (
            "ClockId::RealtimeCoarse",
            "rustix::time::clock_gettime(ClockId::RealtimeCoarse)".to_owned(),
            1,
        ),
        (
            "ClockId::RealtimeCoarse",
            "use ClockId::*;\nlet id = RealtimeCoarse;".to_owned(),
            2,
        ),
        (
            "DynamicClockId::RealtimeAlarm",
            "clock_gettime_dynamic(DynamicClockId::RealtimeAlarm)".to_owned(),
            1,
        ),
        (
            "DynamicClockId::Tai",
            "clock_gettime_dynamic(DynamicClockId::Tai)".to_owned(),
            1,
        ),
    ]
}

/// Catches: a row of the form table that refuses nothing (or that is removed); a form missed
/// when it is spelled through a `use`, a group, `self`, a leading `::`, `<T>::`, a libc glob,
/// a reference passed on to be called, a clock id held in a binding or brought in by a glob
/// (passed directly or held in a binding), a raw or Unicode
/// identifier nearby, or inside a function with a `use<..>` capture bound; and a row added to
/// the table without a case here.
#[test]
fn each_wall_clock_form_is_refused() {
    let cases = form_cases();
    for &(form, ref text, line) in &cases {
        assert_eq!(
            lint(&[(DECIDE, text.as_str())], ""),
            Err(vec![refusal(DECIDE, line, form)]),
            "in {text:?}"
        );
    }
    let tested: BTreeSet<&str> = cases.iter().map(|&(form, _, _)| form).collect();
    let table: BTreeSet<&str> = FORMS.iter().map(|f| f.name).collect();
    assert_eq!(tested, table);
    assert_eq!(table.len(), FORMS.len(), "two rows share a name");
}

/// Catches: a wall-clock name renamed by `use .. as ..` or `type .. = ..;` hiding the reads made
/// through the new name (in the same scope, in another module, through a chain of renames or a
/// re-export, under a raw or Unicode alias, in either order of the scopes; a source reached
/// through an import, a glob, a crate alias, `crate::` or a module of the file, also when another
/// scope binds the same name elsewhere, in either order), and a rename of a name that is not a
/// wall clock, or of another crate's `Timestamp` or `time` (or `as _`), refused.
#[test]
fn a_renamed_wall_clock_name_is_refused() {
    let st = st();
    let cases = [
        (
            format!("use std::time::{st} as T;\nfn f() {{ T::now(); }}"),
            1,
            format!("std::time::{st} as T"),
        ),
        (
            format!("use std::time::{{Instant, {st} as Wall}};"),
            1,
            format!("std::time::{st} as Wall"),
        ),
        (
            format!("fn f() {{\n    W::now();\n}}\nmod m {{\n    use std::time::{st} as W;\n}}"),
            5,
            format!("std::time::{st} as W"),
        ),
        (
            format!(
                "use std::time::Instant as W;\nfn f() {{ W::now(); }}\n\
                 mod m {{ use std::time::{st} as W; }}"
            ),
            3,
            format!("std::time::{st} as W"),
        ),
        (
            format!("use std::time::{st} as Wall;\nuse self::Wall as W;\nfn f() {{ W::now(); }}"),
            1,
            format!("std::time::{st} as Wall"),
        ),
        (
            format!("mod m {{ pub use std::time::{st}; }}\nuse m::{st} as W;"),
            2,
            format!("m::{st} as W"),
        ),
        (
            format!("use std::time::{st} as r#Wall;"),
            1,
            format!("std::time::{st} as r#Wall"),
        ),
        (
            format!("use std::time::{st} as HorlogeÉ;"),
            1,
            format!("std::time::{st} as HorlogeÉ"),
        ),
        (
            format!("type W = std::time::{st};\nfn f() {{ W::now(); }}"),
            1,
            format!("type W = std::time::{st}"),
        ),
        (
            "pub use std::time::{UNIX_EPOCH as E};".to_owned(),
            1,
            "std::time::UNIX_EPOCH as E".to_owned(),
        ),
        ("use libc as c;".to_owned(), 1, "libc as c".to_owned()),
        (
            "use libc::time as t;".to_owned(),
            1,
            "libc::time as t".to_owned(),
        ),
        (
            "use rustix::time::{ClockId as Id};".to_owned(),
            1,
            "rustix::time::ClockId as Id".to_owned(),
        ),
        (
            "use chrono::Local as L;".to_owned(),
            1,
            "chrono::Local as L".to_owned(),
        ),
        (
            "use time::OffsetDateTime as O;".to_owned(),
            1,
            "time::OffsetDateTime as O".to_owned(),
        ),
        (
            "use jiff::Timestamp as Ts;".to_owned(),
            1,
            "jiff::Timestamp as Ts".to_owned(),
        ),
        (
            "use chrono::Utc;\ntype W = Utc;\nfn f() { W::now(); }".to_owned(),
            2,
            "type W = Utc".to_owned(),
        ),
        (
            "mod m { pub use chrono::Utc; }\nuse m::Utc as W;".to_owned(),
            2,
            "m::Utc as W".to_owned(),
        ),
        (
            "use jiff::Timestamp;\ntype T = Timestamp;".to_owned(),
            2,
            "type T = Timestamp".to_owned(),
        ),
        (
            "mod m { pub use jiff::Timestamp; }\nuse m::Timestamp as T;".to_owned(),
            2,
            "m::Timestamp as T".to_owned(),
        ),
        (
            "use crate::clock::Timestamp as T;".to_owned(),
            1,
            "crate::clock::Timestamp as T".to_owned(),
        ),
        (
            "use jiff as j;\nuse j::Timestamp as T;".to_owned(),
            2,
            "j::Timestamp as T".to_owned(),
        ),
        (
            "use crate::time as t2;".to_owned(),
            1,
            "crate::time as t2".to_owned(),
        ),
        (
            "use jiff::*;\ntype T = Timestamp;\nfn f() { T::now(); }".to_owned(),
            2,
            "type T = Timestamp".to_owned(),
        ),
        (
            "use jiff::Timestamp;\ntype T = Timestamp;\nfn f() { T::now(); }\n\
             mod other { use prost_types::Timestamp; }"
                .to_owned(),
            2,
            "type T = Timestamp".to_owned(),
        ),
        (
            "mod other { use prost_types::Timestamp; }\nuse jiff::Timestamp;\n\
             type T = Timestamp;\nfn f() { T::now(); }"
                .to_owned(),
            3,
            "type T = Timestamp".to_owned(),
        ),
    ];
    for (text, line, spelling) in &cases {
        assert_eq!(
            lint(&[(DECIDE, text.as_str())], ""),
            Err(vec![renaming(DECIDE, *line, spelling)]),
            "in {text:?}"
        );
    }
    let passing = format!(
        "use std::time::Instant as I;\nuse std::time as t;\nuse prost_types::Timestamp as Pb;\n\
         use chrono::Duration as D;\nuse std::time::{st} as _;\nuse other::Timestamp as Pb2;\n\
         type R = Result<{st}, E>;\ntype J = std::time::Instant;\nfn f() {{ I::now(); }}\n"
    );
    assert_eq!(lint(&[(DECIDE, passing.as_str())], ""), Ok(()));
}

/// Catches: a table that over-matches (any `::now`, any `clock_gettime`, any `.elapsed()`,
/// any mention of the epoch), and a `use` of a wall-clock name under its own name taken for a
/// read.
#[test]
fn monotonic_reads_pass() {
    let st = st();
    let text = format!(
        "use std::time::{{Instant, {st}, UNIX_EPOCH}};\nuse libc::time;\n\
         use libc::{{clock_gettime as cg}};\nuse rustix::time::*;\nuse rustix::time::ClockId::*;\n\
         fn f(clock: &dyn Clock, id: ClockId, t: {st}) -> impl Sized + use<> {{\n\
             let a = Instant::now();\n    let b = std::time::Instant::now().elapsed();\n\
             let c = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);\n\
             let d = clock_gettime(ClockId::Boottime);\n\
             let e = clock_gettime_dynamic(DynamicClockId::Known(ClockId::Monotonic));\n\
             let g = libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);\n\
             let h = cg(id);\n    let i = Stamp::now(clock);\n\
             let j = {st}::UNIX_EPOCH + d;\n    let k = x.time();\n    let l = clock_gettime;\n\
             let m = t.duration_since(UNIX_EPOCH);\n    let n = start.elapsed();\n\
             let o = clock_gettime(Monotonic);\n    let p = Boottime;\n\
             Instant::now()\n}}\n"
    );
    assert_eq!(lint(&[(DECIDE, text.as_str())], ""), Ok(()));
}
