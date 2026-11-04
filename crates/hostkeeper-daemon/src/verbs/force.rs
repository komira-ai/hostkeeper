//! `force`: not built yet; answers `not_implemented`. It will be the hookless switch of design
//! section 5.6.

use crate::executor::{Ctx, Reply};
use crate::verb::Forced;

pub fn run(_ctx: &Ctx, _order: Forced) -> Reply {
    super::not_implemented("force")
}
