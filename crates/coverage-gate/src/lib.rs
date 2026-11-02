//! Per-crate line and branch coverage for the workspace, and the gate that keeps it up.
//!
//! CI runs the tests under `cargo llvm-cov --branch`, which writes an lcov tracefile. This crate
//! reads it (`lcov`), sums its per-file counts into one [`CrateCoverage`] per directory under
//! `crates/` (`gate::by_crate`) and fails any crate whose line or branch coverage is under
//! [`Percent::TARGET`], 99.00%. Every crate is judged, the repository's own tooling included:
//! there is no exclusion. [`main`] is the program CI runs and the crate's one export; `main.rs`
//! only forwards to it.
//!
//! Percentages are floored to hundredths ([`Percent`]), so one uncovered line in a large crate
//! shows as 99.99%, never as a rounded-up 100.00%. The gate compares in integers
//! (`hit * 10000 >= percent * found`, see [`Counts::meets`]), never floats. Sums are checked: a
//! sum that does not fit is an error, never a saturated or wrapped count.

mod cli;
mod gate;
mod lcov;
#[cfg(test)]
mod testdir;

pub use cli::main;

use std::fmt;

/// How many items (lines or branches) a source holds and how many of them ran.
/// `hit <= found` always holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    found: u64,
    hit: u64,
}

impl Counts {
    /// `None` when `hit > found`, which no coverage tool can report.
    fn new(found: u64, hit: u64) -> Option<Self> {
        (hit <= found).then_some(Self { found, hit })
    }

    fn found(self) -> u64 {
        self.found
    }

    fn hit(self) -> u64 {
        self.hit
    }

    /// The sum of two counts, or `None` when it does not fit in 64 bits. Both fields are
    /// checked: saturating them one by one could turn a failing sum into 100%.
    fn plus(self, other: Self) -> Option<Self> {
        Some(Self {
            found: self.found.checked_add(other.found)?,
            hit: self.hit.checked_add(other.hit)?,
        })
    }

    /// Whether the share that ran is at least `p`; true when there is nothing to
    /// measure. Exact: no division, so `99.00` of 10000 items passes at 9900 hits and
    /// fails at 9899.
    fn meets(self, p: Percent) -> bool {
        u128::from(self.hit) * 10_000 >= u128::from(p.0) * u128::from(self.found)
    }

    /// The share that ran, or `None` when there is nothing to measure.
    fn percent(self) -> Option<Percent> {
        if self.found == 0 {
            return None;
        }
        let hundredths = u128::from(self.hit) * 10_000 / u128::from(self.found);
        // `hit <= found`, so `hundredths <= 10_000` and the cast cannot truncate.
        Some(Percent(hundredths as u16))
    }
}

/// Line and branch counts for one crate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CrateCoverage {
    lines: Counts,
    branches: Counts,
}

impl CrateCoverage {
    /// The sum, or `None` when a count does not fit in 64 bits.
    fn plus(self, other: Self) -> Option<Self> {
        Some(Self {
            lines: self.lines.plus(other.lines)?,
            branches: self.branches.plus(other.branches)?,
        })
    }
}

/// A percentage in hundredths, from 0.00 to 100.00, floored (never rounded up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Percent(u16);

impl Percent {
    /// What every measured crate must reach: 99.00%.
    const TARGET: Percent = Percent(9_900);
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

/// Formats an optional value as the table writes it: `-` when there is nothing to measure.
fn show<T: fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Percent = Percent(10_000);

    fn counts(found: u64, hit: u64) -> Counts {
        Counts::new(found, hit).expect("valid")
    }

    fn pct(c: Counts) -> String {
        show(c.percent())
    }

    /// Catches: a percentage rounded up, which would show one uncovered line in a large
    /// crate as 100.00% with no gap.
    #[test]
    fn percent_floors_so_a_miss_is_never_hidden() {
        let c = counts(100_000, 99_999);
        assert_eq!(pct(c), "99.99");
        assert_eq!(pct(counts(3, 2)), "66.66");
        assert_eq!(pct(counts(20, 1)), "5.00");
        assert_eq!(counts(4, 4).percent(), Some(FULL));
        assert_eq!(pct(counts(4, 0)), "0.00");
        assert_eq!(Percent::TARGET.to_string(), "99.00");
    }

    /// Catches: a crate with nothing to measure reported as 0%, a value that could not be
    /// told from a real measurement.
    #[test]
    fn nothing_found_has_no_percentage() {
        assert_eq!(Counts::default().percent(), None);
        assert_eq!(show::<Percent>(None), "-");
        assert_eq!(show(Some(7)), "7");
    }

    /// Catches: counts that claim more hits than items, which would yield over 100%.
    #[test]
    fn hits_above_found_are_refused() {
        assert_eq!(Counts::new(1, 2), None);
        let c = counts(2, 1);
        assert_eq!((c.found(), c.hit()), (2, 1));
    }

    /// Catches: a sum that adds the wrong fields.
    #[test]
    fn counts_add_field_by_field() {
        let a = CrateCoverage {
            lines: counts(10, 4),
            branches: counts(2, 1),
        };
        let b = CrateCoverage {
            lines: counts(5, 5),
            branches: counts(3, 0),
        };
        let sum = a.plus(b).expect("fits");
        assert_eq!(sum.lines, counts(15, 9));
        assert_eq!(sum.branches, counts(5, 1));
    }

    /// Catches: a sum that saturates or wraps. Two halves-covered maximum counts would saturate
    /// to a fully covered one (100%) though together they are about 50% covered; a sum that does
    /// not fit is `None`, on either field, for lines and for branches.
    #[test]
    fn a_sum_that_does_not_fit_is_none_never_saturated() {
        let half = counts(u64::MAX, 1 << 63);
        assert_eq!(half.plus(half), None);
        assert_eq!(counts(u64::MAX, 0).plus(counts(1, 0)), None);
        assert_eq!(counts(u64::MAX, u64::MAX).plus(counts(1, 1)), None);
        let fits = CrateCoverage {
            lines: counts(1, 1),
            branches: counts(1, 1),
        };
        let big_lines = CrateCoverage {
            lines: half,
            branches: counts(0, 0),
        };
        let big_branches = CrateCoverage {
            lines: counts(0, 0),
            branches: half,
        };
        assert_eq!(big_lines.plus(big_lines), None);
        assert_eq!(big_branches.plus(big_branches), None);
        assert_eq!(
            fits.plus(fits),
            Some(CrateCoverage {
                lines: counts(2, 2),
                branches: counts(2, 2)
            })
        );
    }

    /// Catches: a gate that compares floats or rounds, or `>` for `>=`: exactly the
    /// threshold passes, one hit fewer fails, and an empty source always passes.
    #[test]
    fn meets_is_exact_at_the_threshold() {
        let t = Percent::TARGET;
        assert!(counts(10_000, 9_900).meets(t));
        assert!(!counts(10_000, 9_899).meets(t));
        assert!(counts(100, 99).meets(t));
        assert!(!counts(101, 99).meets(t), "98.01% is under 99.00%");
        assert!(counts(200, 198).meets(t));
        assert!(Counts::default().meets(FULL));
        assert!(counts(u64::MAX, u64::MAX).meets(FULL));
        assert!(!counts(u64::MAX, u64::MAX - 1).meets(FULL));
        // Whatever `percent` reports is met by the counts it came from.
        for (f, h) in [(3, 2), (7, 5), (9_103, 9_031), (1_000_003, 999_999)] {
            let c = counts(f, h);
            assert!(c.meets(c.percent().expect("found")), "{h}/{f}");
        }
    }
}
