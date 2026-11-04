//! `uninstall`: not built yet; answers `not_implemented`. It will stop the service's units, run
//! its `uninstall` hook and remove its package.

use hostkeeper_config::names::ServiceName;

use crate::executor::{Ctx, Reply};

pub fn run(_ctx: &Ctx, _service: ServiceName) -> Reply {
    super::not_implemented("uninstall")
}
