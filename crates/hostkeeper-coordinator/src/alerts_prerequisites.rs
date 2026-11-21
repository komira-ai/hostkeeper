//! The `prerequisite_failed` alerts ([`crate::prerequisites`]): one for each host whose last
//! prerequisites report holds a failed state.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1::{PrerequisiteStatus, PrerequisitesReport};
use hostkeeper_wire::HostId;

use super::{Alert, Kind, Said, Subject};
use crate::fleet::Fleet;

/// `prerequisite_failed`: each host of the fleet whose last heartbeat announced
/// `prerequisites-v1` and whose last prerequisites report holds a failed state, with each failed
/// prerequisite, its failures in a row, the Unix time of its next try and its reason. A report
/// kept for a host that left the fleet, or whose daemon no longer announces the feature, raises
/// none.
pub(super) fn prerequisite_failures(
    fleet: &Fleet,
    reports: &BTreeMap<HostId, PrerequisitesReport>,
) -> Vec<Alert> {
    reports
        .iter()
        .filter(|(id, _)| fleet.announced(id, crate::prerequisites::FEATURE))
        .filter_map(|(id, report)| {
            let failed: Vec<String> = report
                .states
                .iter()
                .filter(|state| state.status == PrerequisiteStatus::Failed as i32)
                .map(|state| {
                    format!(
                        "{} failed {} in a row, next try at unix {}: {}",
                        state.name, state.failures, state.next_try_unix, state.reason
                    )
                })
                .collect();
            (!failed.is_empty()).then(|| Alert {
                subject: Subject {
                    host: id.to_string(),
                    ..Subject::of(Kind::PrerequisiteFailed)
                },
                message: format!(
                    "{id} cannot make sure of its host prerequisites ({}); its daemon tries \
                     again by itself, and until it succeeds the host lacks what its host group \
                     needs",
                    failed.join("; ")
                ),
                said: Said::default(),
            })
        })
        .collect()
}
