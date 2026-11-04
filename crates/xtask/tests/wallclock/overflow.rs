//! Rename sources too many to follow: the scan refuses the rename by name instead of judging
//! a shortened list of its sources, and only a rename that may name a wall clock is followed.

use crate::{DECIDE, lint, renaming, unresolved};

/// `count` glob imports of unrelated crates, one per line.
fn unrelated_globs(count: usize) -> String {
    (1..=count).map(|n| format!("use a{n}::*;\n")).collect()
}

/// A file with `count` unrelated globs, then `use jiff::*;`, then a rename of jiff's
/// `Timestamp` and a read through it.
fn jiff_after(count: usize) -> String {
    format!(
        "{}use jiff::*;\ntype T = Timestamp;\nfn f() {{ T::now(); }}\n",
        unrelated_globs(count)
    )
}

/// Catches: a rename judged on the first 64 of its sources, so that `use jiff::*` after enough
/// unrelated globs falls outside them and the rename of jiff's `Timestamp` (and the read through
/// it) passes; the refusal names the file, the line and the new name.
#[test]
fn a_rename_with_too_many_sources_is_refused() {
    let text = jiff_after(70);
    assert_eq!(
        lint(&[(DECIDE, text.as_str())], ""),
        Err(vec![unresolved(DECIDE, 72, "T")])
    );
    assert_eq!(
        unresolved(DECIDE, 72, "T"),
        format!(
            "{DECIDE}:72: cannot resolve `T`: more than 64 import sources; split the glob \
             imports or allow-list the path"
        )
    );
}

/// Catches: a bound off by one either way. With 62 unrelated globs the rename's sources are
/// itself and 63 globs, 64 in all: they fit, and the jiff source is refused as a wall-clock
/// rename. With 63 unrelated globs they are 65 and the rename is refused as unresolved.
#[test]
fn the_source_bound_is_exactly_64() {
    let fits = jiff_after(62);
    assert_eq!(
        lint(&[(DECIDE, fits.as_str())], ""),
        Err(vec![renaming(DECIDE, 64, "type T = Timestamp")])
    );
    let over = jiff_after(63);
    assert_eq!(
        lint(&[(DECIDE, over.as_str())], ""),
        Err(vec![unresolved(DECIDE, 65, "T")])
    );
    let few = jiff_after(3);
    assert_eq!(
        lint(&[(DECIDE, few.as_str())], ""),
        Err(vec![renaming(DECIDE, 5, "type T = Timestamp")])
    );
}

/// Catches: a refusal too eager, which would refuse any file with many globs: with 100
/// unrelated globs and renames of names that are not wall clocks (a type alias, a `use .. as`,
/// an alias of one), nothing is followed far enough to overflow, and the file passes.
#[test]
fn many_globs_without_a_wall_clock_rename_pass() {
    let text = format!(
        "{}type Target = Path;\nuse foo::Bar as Baz;\ntype Q = Baz;\n\
         fn f() {{ let a = Instant::now(); let b = Baz::new(); }}\n",
        unrelated_globs(100)
    );
    assert_eq!(lint(&[(DECIDE, text.as_str())], ""), Ok(()));
}
