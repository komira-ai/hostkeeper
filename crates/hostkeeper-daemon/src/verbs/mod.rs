//! One module per verb, each a function taking the shared [`crate::executor::Ctx`] and the verb's
//! typed arguments and answering with a [`Reply`]. The executor only dispatches to them.
//!
//! `update`, `force_rollback`, `force`, `install` and `uninstall` are built. The others answer
//! `not_implemented` and change nothing: `abort`, `clear_halt` and `clear_record` each fill their
//! own file.

pub mod abort;
pub mod clear_halt;
pub mod clear_record;
pub mod force;
pub mod force_rollback;
pub mod install;
pub mod uninstall;
pub mod update;

use crate::executor::Reply;
use crate::refusal::Refusal;

/// The answer of a verb that is not built yet: `not_implemented`, and nothing done.
pub fn not_implemented(verb: &str) -> Reply {
    Reply::refused(
        Refusal::NotImplemented,
        format!("`{verb}` is not implemented in this daemon yet (a later task)"),
    )
}
