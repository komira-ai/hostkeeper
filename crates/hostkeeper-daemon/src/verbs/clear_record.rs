//! `clear_record` (design sections 4.1 and 7; two-programs.md section 3.2): set aside a step
//! record that cannot be read, so that orders run for the service again: a coordinator's order,
//! the `clear_record` case of `hostkeeper.agent.v1`'s `Order` ([`run`]). The executor dispatches
//! the order before anything reads the record, so a record that does not read never refuses it.
//! On a host the coordinator cannot reach, root stops the daemon and renames the record by hand
//! (docs/admin.md): that rename is the whole of what this verb does.
//!
//! Under the service's lock, never waited for (`busy` when held). Only a record that does not
//! read ([`RecordError::Corrupt`]) is cleared. An absent record, one that reads in any state, and
//! one of a newer schema are refused `record_reads` and not touched: a record that reads is the
//! runner's or `abort`'s to finish, and a newer one belongs to a newer hostkeeper.
//!
//! Clearing is one rename of `step-record.json` to [`CORRUPT_FILE`] (an earlier one is replaced)
//! and a directory fsync, so that the operator, or a bug report, can still read what broke.
//! Nothing else is touched: not the params file, not the held halt, not the package directory,
//! not another service. The next order of the service starts at generation 1. A unit the lost
//! record had started is not stopped: the record that named it cannot be read.

use std::fs::{self, File};
use std::io;

use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::Verdict;
use hostkeeper_steps::record::{Loaded, RECORD_FILE, RecordError};

use crate::executor::{Ctx, Refused, Reply};
use crate::locks::Holder;
use crate::refusal::Refusal;

/// The name a cleared record is kept under, beside where it was.
pub const CORRUPT_FILE: &str = "step-record.json.corrupt";

/// The reason of the verdict of a `clear_record` order that set the record aside.
pub const REASON_RECORD_CLEARED: &str = "record_cleared";

/// A record that was cleared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cleared {
    /// Why it did not read.
    pub why: String,
}

/// The coordinator's `clear_record` order: `Finished{Succeeded, record_cleared}` once the record
/// is set aside, the refusal otherwise (`busy`, `record_reads`, `store_failed`). Its verdict is
/// reported at the version the service runs; why the record did not read is not in it, and
/// [`CORRUPT_FILE`] keeps the bytes.
pub fn run(ctx: &Ctx, service: &ServiceName) -> Reply {
    let cleared = |_| Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: Some(REASON_RECORD_CLEARED.to_owned()),
        resent: false,
    };
    clear_record(ctx, service).map_or_else(Reply::from, cleared)
}

/// Clears the corrupt step record of `service`.
pub fn clear_record(ctx: &Ctx, service: &ServiceName) -> Result<Cleared, Refused> {
    let _guard = ctx
        .locks
        .take_for(service, Holder::ClearRecord)
        .map_err(|held| {
            Refused::new(
                Refusal::Busy,
                format!("{service} is locked by {}", held.holder),
            )
        })?;
    let reads = |what: String| {
        Refused::new(
            Refusal::RecordReads,
            format!("the step record of {service} {what}; only a corrupt one is cleared"),
        )
    };
    let why = match ctx.store(service).load() {
        Err(RecordError::Corrupt(why)) => why,
        Ok(Loaded::Absent) => return Err(reads("is absent".to_owned())),
        Ok(Loaded::Current(_)) => return Err(reads("reads".to_owned())),
        Ok(Loaded::Newer { schema }) => {
            return Err(reads(format!("has schema {schema}, of a newer hostkeeper")));
        }
        Err(e) => return Err(Refused::new(Refusal::StoreFailed, e)),
    };
    set_aside(ctx, service).map_err(|e| Refused::new(Refusal::StoreFailed, e))?;
    Ok(Cleared { why })
}

/// Renames the record to [`CORRUPT_FILE`], durably.
fn set_aside(ctx: &Ctx, service: &ServiceName) -> io::Result<()> {
    let dir = ctx.service_dir(service);
    fs::rename(dir.join(RECORD_FILE), dir.join(CORRUPT_FILE))?;
    File::open(&dir)?.sync_all()
}
