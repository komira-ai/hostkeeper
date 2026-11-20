//! The maintenance run record and its state machine (docs/design/maintenance-updates.md,
//! sections 5.2 to 5.7): the rows of the step table and of the table of how a turn ends
//! (`machine`), the record's file (`record`), and a crash at every write of a turn resumed from
//! the record on disk (`replay`).

mod machine;
mod record;
mod replay;
mod support;
