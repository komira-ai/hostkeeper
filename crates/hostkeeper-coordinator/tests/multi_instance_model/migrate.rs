//! The model `migrate`: instances that start at the same moment and run the migrations (design
//! section 4, "Migrations (serialized, and short)", and its Built paragraph).
//!
//! Rows: the migrations table (whether `m1` and `m2` are each recorded), the schema objects (how
//! many times each migration's DDL was applied), and the migration lock (`MIGRATE_KEY`: the
//! session holding it, if any). Waiters are starters whose lock step is not yet enabled.
//!
//! Each starter takes the lock (it waits while another holds it), then for each migration in
//! order reads whether it is recorded (one statement) and, when it is not, applies it and records
//! it in one transaction (one step: the DDL and the `schema_version` row commit together, or
//! neither does), then releases the lock. A starter that finished goes on to load and serve, which
//! the model does not follow. The runner runs on a session that already holds the shared fence
//! lock, which excludes no one and so is not a row of the model.
//!
//! A starter may die once in the whole run (not once per starter), at any step before it
//! finished: Postgres releases its session lock and rolls back its open transaction (a migration
//! transaction has no step of its own in between, so its death leaves either the whole migration
//! or none of it), and the dead starter is restarted and runs again from the start. One death
//! suffices: the lock is released on every death, and a second death only returns a starter to
//! the start again. A wait for the lock that runs past `lock_timeout` ends the start, which is
//! restarted the same way; the model leaves that as waiting, since the restarted starter is in
//! the same state as the one that waited.
//!
//! A DDL applied a second time fails on a duplicate object in Postgres; the model counts the
//! second application and calls it the defect, whatever the server then does with it.

use crate::explore::{Check, Invariant, Model, Outcome, explore};
use crate::harness::{ModelDef, VariantCase};

pub const MODEL: ModelDef = ModelDef {
    name: "migrate",
    bound: "N starters running 2 migrations, all starting at once; one starter death in the whole \
            run, at any step, after which it is restarted",
    invariants: &[
        "two_starters_apply_each_migration_once",
        "every_migration_is_applied_once_a_starter_survives",
    ],
    cases: &[
        VariantCase {
            variant: "migration_runner_without_the_mutex",
            breaks: "two_starters_apply_each_migration_once",
            fails_from: Some(2),
        },
        VariantCase {
            variant: "migration_lock_kept_in_a_row",
            breaks: "every_migration_is_applied_once_a_starter_survives",
            fails_from: Some(1),
        },
    ],
    run,
};

fn run(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome {
    explore(&Migrate::new(n, variant), only)
}

/// The number of migrations the runner has to apply.
const MIGRATIONS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    Locked,
    /// The runner takes no lock: starters read the migrations table and apply with nothing
    /// between them.
    MigrationRunnerWithoutTheMutex,
    /// The lock is a row the starter sets and clears itself: Postgres does not release it when
    /// the starter dies.
    MigrationLockKeptInARow,
}

impl Variant {
    fn of(name: Option<&str>) -> Variant {
        match name {
            None => Variant::Locked,
            Some("migration_runner_without_the_mutex") => Variant::MigrationRunnerWithoutTheMutex,
            Some("migration_lock_kept_in_a_row") => Variant::MigrationLockKeptInARow,
            Some(other) => panic!("the model migrate has no variant {other}"),
        }
    }
}

/// Where a starter is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Pc {
    /// Started; has not begun the runner (without the mutex there is no lock to take).
    Start,
    /// Is about to read whether migration `.0` is recorded.
    Read(u8),
    /// Has read migration `.0`; `.1` is whether it was recorded, and the starter acts on it next.
    Decide(u8, bool),
    /// Has applied or skipped every migration and has not yet released the lock.
    Finishing,
    /// Released the lock and went on to load and serve.
    Done,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// Whether each migration is recorded in the migrations table.
    recorded: [bool; MIGRATIONS],
    /// How many times each migration's DDL was applied (at most 2 is kept).
    applied: [u8; MIGRATIONS],
    /// The starter holding the migration lock.
    lock: Option<u8>,
    starters: Vec<Pc>,
    death_left: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read by the trace of a broken invariant, through Debug"
)]
pub enum Step {
    TakeLock(usize),
    Begin(usize),
    Read(usize),
    ApplyAndRecord(usize),
    SkipRecorded(usize),
    ReleaseLock(usize),
    Die(usize),
}

pub struct Migrate {
    n: usize,
    variant: Variant,
}

impl Migrate {
    fn new(n: usize, variant: Option<&str>) -> Migrate {
        Migrate {
            n,
            variant: Variant::of(variant),
        }
    }
}

impl Model for Migrate {
    type State = State;
    type Step = Step;

    fn init(&self) -> Vec<State> {
        vec![State {
            recorded: [false; MIGRATIONS],
            applied: [0; MIGRATIONS],
            lock: None,
            starters: vec![Pc::Start; self.n],
            death_left: true,
        }]
    }

    fn next(&self, s: &State, out: &mut Vec<(Step, State)>) {
        for (i, pc) in s.starters.iter().enumerate() {
            let me = Some(i as u8);
            match *pc {
                Pc::Start => {
                    if self.variant == Variant::MigrationRunnerWithoutTheMutex {
                        let mut t = s.clone();
                        t.starters[i] = Pc::Read(0);
                        out.push((Step::Begin(i), t));
                    } else if s.lock.is_none() {
                        let mut t = s.clone();
                        t.lock = me;
                        t.starters[i] = Pc::Read(0);
                        out.push((Step::TakeLock(i), t));
                    }
                }
                Pc::Read(k) => {
                    let mut t = s.clone();
                    t.starters[i] = Pc::Decide(k, s.recorded[usize::from(k)]);
                    out.push((Step::Read(i), t));
                }
                Pc::Decide(k, recorded) => {
                    let mut t = s.clone();
                    let m = usize::from(k);
                    if recorded {
                        out.push((Step::SkipRecorded(i), {
                            t.starters[i] = Self::after(k);
                            t
                        }));
                    } else {
                        t.applied[m] = t.applied[m].saturating_add(1).min(2);
                        t.recorded[m] = true;
                        t.starters[i] = Self::after(k);
                        out.push((Step::ApplyAndRecord(i), t));
                    }
                }
                Pc::Finishing => {
                    let mut t = s.clone();
                    if s.lock == me {
                        t.lock = None;
                    }
                    t.starters[i] = Pc::Done;
                    out.push((Step::ReleaseLock(i), t));
                }
                Pc::Done => {}
            }
            if s.death_left && *pc != Pc::Done {
                let mut t = s.clone();
                t.death_left = false;
                if self.variant != Variant::MigrationLockKeptInARow && s.lock == me {
                    t.lock = None;
                }
                t.starters[i] = Pc::Start;
                out.push((Step::Die(i), t));
            }
        }
    }

    fn invariants(&self) -> Vec<Invariant<Self>> {
        vec![
            Invariant {
                name: "two_starters_apply_each_migration_once",
                check: Check::Always(|_, s: &State| {
                    s.applied
                        .iter()
                        .position(|a| *a > 1)
                        .map(|m| format!("migration m{} was applied twice", m + 1))
                }),
            },
            Invariant {
                name: "every_migration_is_applied_once_a_starter_survives",
                check: Check::Reachable(|_, s: &State| s.recorded.iter().all(|r| *r)),
            },
        ]
    }
}

impl Migrate {
    /// The place after migration `k`: the next one, or the end of the runner.
    fn after(k: u8) -> Pc {
        if usize::from(k) + 1 < MIGRATIONS {
            Pc::Read(k + 1)
        } else {
            Pc::Finishing
        }
    }
}
