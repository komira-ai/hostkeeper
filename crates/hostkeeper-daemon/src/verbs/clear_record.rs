//! `clear_record` (local only): not built yet; answers `not_implemented`. It will clear a step
//! record that cannot be read.

use hostkeeper_config::names::ServiceName;

use crate::executor::{Ctx, Reply};

pub fn run(_ctx: &Ctx, _service: ServiceName) -> Reply {
    super::not_implemented("clear_record")
}
