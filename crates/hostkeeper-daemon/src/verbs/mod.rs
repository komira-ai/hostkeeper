//! One module per verb, each a function taking the shared [`crate::executor::Ctx`] and the verb's
//! typed arguments and answering with a [`crate::executor::Reply`]. The executor only dispatches to them.
//!
//! Every verb of design section 4.1 is built: `update`, `force_rollback`, `force`, `install`,
//! `uninstall`, `abort`, `clear_halt` and `clear_record`.

pub mod abort;
pub mod clear_halt;
pub mod clear_record;
pub mod force;
pub mod force_rollback;
pub mod install;
pub mod uninstall;
pub mod update;
