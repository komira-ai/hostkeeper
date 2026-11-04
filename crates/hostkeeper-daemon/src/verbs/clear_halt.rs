//! `clear_halt`: not built yet; answers `not_implemented`. It will lift the service's halt when
//! the clear generation is above the held one.

use hostkeeper_config::names::ServiceName;

use crate::executor::{Ctx, Reply};

pub fn run(_ctx: &Ctx, _service: ServiceName, _halt_generation: u64) -> Reply {
    super::not_implemented("clear_halt")
}
