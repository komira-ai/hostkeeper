//! `force_rollback`: a hooked update to any version, lower ones included (design section 4.1).
//! It is `update` without the refusal below the running version.

use crate::executor::{Ctx, Reply};
use crate::verb::{Hooked, HookedVerb};

pub fn run(ctx: &Ctx, order: Hooked) -> Reply {
    super::update::hooked(ctx, HookedVerb::ForceRollback, order)
}
