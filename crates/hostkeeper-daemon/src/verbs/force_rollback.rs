//! `force_rollback`: a hooked update to any version, lower ones included (design section 4.1).
//! It is `update` without the refusal below the running version. The service `hostkeeper` is
//! refused (`self_service`): it updates itself by its own path (design sections 5.1 and 6.6).

use crate::checks;
use crate::executor::{Ctx, Reply};
use crate::verb::{Hooked, HookedVerb};

pub fn run(ctx: &Ctx, order: Hooked) -> Reply {
    if let Err(refusal) = checks::self_service(HookedVerb::ForceRollback, &order.service) {
        return Reply::refused(
            refusal,
            format!(
                "{} updates itself by its own path, never by a force_rollback",
                order.service
            ),
        );
    }
    super::update::hooked(ctx, HookedVerb::ForceRollback, order)
}
