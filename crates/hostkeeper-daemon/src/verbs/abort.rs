//! `abort`: not built yet; answers `not_implemented`. It will stop the service's running step and
//! end its order in the halt phase.

use hostkeeper_config::names::ServiceName;

use crate::executor::{Ctx, Reply};

pub fn run(_ctx: &Ctx, _service: ServiceName) -> Reply {
    super::not_implemented("abort")
}
