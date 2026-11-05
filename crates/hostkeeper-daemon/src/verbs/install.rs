//! `install`: add a service the coordinator assigned, by the update path with no previous
//! version (design sections 4.1 and 5.7, step 6). It is refused for a service that has a release
//! installed (`already_installed`) and while the service's uninstall is unfinished
//! (`uninstalling`). The runner's switch places the package and points `current` at it
//! immediately before `cutover`, and takes both off the host immediately before `rollback`
//! ([`crate::place`]); a first install whose `rollback` declines with `no_previous_release` ends
//! `Failed` with that reason, never in a halt, and the service is absent again, so that a later
//! order is accepted.

use crate::executor::{Ctx, Reply};
use crate::verb::{Hooked, HookedVerb};

pub fn run(ctx: &Ctx, order: Hooked) -> Reply {
    super::update::hooked(ctx, HookedVerb::Install, order)
}
