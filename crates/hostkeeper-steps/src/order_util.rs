//! Small helpers of the runner that read an order or a record and hold no state.

use std::path::{Path, PathBuf};

use hostkeeper_hook::Step;

use crate::artifact::Staged;
use crate::record::{HookSel, Record};
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

/// The hook of the release being replaced, as its own manifest names it: what the `rollback`
/// fallback to that release runs ([`UpdateOrder::previous_hook`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviousHook {
    /// The program, a path in the previous release's tree.
    pub binary: PathBuf,
    /// The arguments given before the step.
    pub args: Vec<String>,
}

/// The program and the leading arguments a step run from the release `sel` names: the order's
/// own, or for the previous release its [`PreviousHook`] when the order has one.
pub(crate) fn hook_of(order: &UpdateOrder, sel: HookSel) -> (&Path, &[String]) {
    match (sel, &order.previous_hook) {
        (HookSel::Previous, Some(previous)) => (&previous.binary, &previous.args),
        _ => (&order.hook_binary, &order.hook_args),
    }
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
