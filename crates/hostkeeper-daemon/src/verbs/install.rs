//! `install`: not built yet; answers `not_implemented`. It will add a service the coordinator
//! assigned, by the update path with no previous version.

use crate::executor::{Ctx, Reply};
use crate::verb::Hooked;

pub fn run(_ctx: &Ctx, _order: Hooked) -> Reply {
    super::not_implemented("install")
}
