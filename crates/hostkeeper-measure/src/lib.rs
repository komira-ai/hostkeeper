//! `hostkeeper-measure`: the measurements of the technical report (docs/report/hostkeeper.html).
//!
//! # The pieces
//!
//! - The plans, `plans/<id>.toml` ([`plan`]): each says what is measured, on which hosts, how many
//!   repetitions, what is recorded, the steps of a repetition, the measures computed from the
//!   records and what the report states from them. [`plan::Plan::parse`] checks a plan whole.
//! - The runner ([`runner`]): carries a plan out against a coordinator through `hkctl` alone
//!   ([`coordinator::Hkctl`]: the coordinator's endpoint and CA as flags, the operator's token
//!   from a file) and records one event per line ([`record`]). Its dry run prints what it would do
//!   and calls nothing.
//! - The analysis ([`analysis`]) and its output ([`render`]): pure functions from a run's records
//!   to the numbers of the plan's measures, a section of the results page and a result file of
//!   `docs/report/results`.
//!
//! # Commands
//!
//! - `hostkeeper-measure check --plan FILE...` reads and checks plans.
//! - `hostkeeper-measure run --plan FILE --run-id ID [--param NAME[@REP]=VALUE]... --dry-run`
//!   prints the steps of every repetition with the parameters filled in. It calls no `hkctl`, writes
//!   nothing and asks the operator nothing; it reads the parameters' files to check them.
//! - `hostkeeper-measure run --plan FILE --run-id ID [--param ...]... --run-dir DIR --hkctl PATH
//!   --endpoint URL --ca-file PATH --token-file PATH` runs the plan. It writes `run.json`,
//!   `records.jsonl` (one event per line, appended and flushed as each is seen), the request files
//!   it sent under `requests/`, and `hosts.tsv` (each node id and the `host-<n>` the records call
//!   it). `hosts.tsv` and `requests/` hold the lab's names and are never committed.
//! - `hostkeeper-measure analyze --plan FILE --run-dir DIR --out-dir docs/report/results` writes
//!   `<plan>-<run>.md`, the run's section of the results page, and `<plan>-<run>.tsv`, its result
//!   file.
//!
//! The exit code is 0 when the command is done, 1 when it failed and 2 for a usage error. A run
//! with a repetition that ended early (a failed call, a watch that timed out) is done: the
//! records say where each one ended, and the analysis counts what it misses.
//!
//! docs/report/results/README.md describes the plans, the columns of the records and how a
//! result is produced.

pub mod analysis;
pub mod cli;
pub mod clock;
pub mod coordinator;
pub mod dry;
pub mod fill;
pub mod fleet;
pub mod params;
pub mod plan;
pub mod record;
pub mod render;
pub mod runner;
pub mod template;

pub use cli::main;
