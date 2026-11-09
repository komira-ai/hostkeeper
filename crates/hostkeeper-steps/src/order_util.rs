//! Small helpers of the runner that read an order or a record and hold no state.

use std::path::Path;

use hostkeeper_hook::Step;

use crate::artifact::Staged;
use crate::record::Record;
use crate::runner::{MAX_RETRY_SECS, RunError, UpdateOrder};
use crate::systemd::unit_name;

/// How long to wait: what the hook asked for, or `default`, never more than [`MAX_RETRY_SECS`].
pub(crate) fn wait_secs(asked: Option<u64>, default: u64) -> u64 {
    asked.unwrap_or(default).min(MAX_RETRY_SECS)
}

/// Whether `rec` is the update `order` asks for.
pub(crate) fn same_update(rec: &Record, order: &UpdateOrder) -> bool {
    rec.service == order.service
        && rec.target_version == order.target_version
        && rec.artifact_digest == order.staged.digest()
        && rec.tarball_digest == order.artifact_digest
        && rec.previous_digest.as_deref() == order.previous.as_ref().map(Staged::digest)
}

pub(crate) fn path_str(path: &Path) -> Result<&str, RunError> {
    path.to_str()
        .ok_or(RunError::BadOrder("a release path that is not UTF-8"))
}

/// The bytes of the longest unit a step of `service` can run in: the longest step name, the
/// largest generation and the `.service` systemd adds.
pub(crate) fn longest_step_unit(service: &str) -> usize {
    Step::ALL
        .iter()
        .map(|step| unit_name(service, *step, u64::MAX).len())
        .max()
        .unwrap_or_default()
        + ".service".len()
}
