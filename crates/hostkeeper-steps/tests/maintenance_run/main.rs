//! The maintenance run record and its state machine (docs/design/maintenance-updates.md,
//! sections 4.2 and 5.2 to 5.7): the rows of the step table and of the table of how a turn ends
//! (`machine`), an operator's Retry and Put back of a halted turn (`operator`), the record's file
//! (`record`), and a crash at every write of a turn resumed from the record on disk (`replay`).

mod machine;
mod operator;
mod record;
mod replay;
mod support;
