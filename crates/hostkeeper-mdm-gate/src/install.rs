//! The gate's install of the hostkeeper pkg (docs/design/hostkeeper-mdm.md sections 7.1 and
//! 5.4), one step of the enrollment side's look ([`crate::enroll`]).
//!
//! `InstallEnterpriseApplication` reports only that the Mac accepted the command, never the
//! install's end, so the gate learns of the install from the daemon's request. The gate sends
//! the pkg to a device the enrollment side acts on (enrolled, with a serial no other live
//! enrollment has, on the allowlist, and not approved in this enrollment) when all of these
//! hold:
//!
//! - the gate has the pkg install ([`crate::Gate::with_install`]);
//! - the device acknowledged the config profile, which the pkg's postinstall reads;
//! - no request is a candidate for the device (design 5.1, condition 3): the daemon has not
//!   posted, so sending stops at the first candidate;
//! - no `InstallEnterpriseApplication` of this enrollment is unanswered, and the last one was
//!   never queued or was answered (`Acknowledged` or an error) at least
//!   [`INSTALL_RESEND_SECS`] ago. A Mac that is off piles nothing up in NanoHUB's queue;
//! - the newest pkg on the allowlist has a file whose bytes are its listed digest.
//!
//! The device need not be supervised: Apple's schema marks the command `supervised: false`,
//! and an unsupervised Mac is approved by an operator once its daemon posts. Each send NanoHUB
//! queued is counted in the record and audited `mdm_install_sent` with the count. A new
//! enrollment forgets the sends, so its count starts again.

use crate::command::Command;
use crate::enroll::{EnrollSide, candidates};
use crate::gate::Gate;
use crate::pkg::manifest;
use crate::record::{DeviceRecord, InstallRecord, ProfileCommand};

/// How long after the device's last answer to the pkg's install it is sent again, in seconds.
/// A constant, not a flag (design 5.4).
pub const INSTALL_RESEND_SECS: u64 = 3600;

/// The audit action of a pkg install NanoHUB queued.
pub const ACTION_INSTALL_SENT: &str = "mdm_install_sent";

/// Whether the pkg's install is due again for `record` at `now`: none sent in this
/// enrollment, or the last neither unanswered nor answered less than
/// [`INSTALL_RESEND_SECS`] ago.
fn due(record: &DeviceRecord, now: u64) -> bool {
    record.install.as_ref().is_none_or(|install| {
        !record.is_unanswered(&install.last.uuid)
            && install
                .last
                .answer
                .is_none_or(|answer| now.saturating_sub(answer.at_unix) >= INSTALL_RESEND_SECS)
    })
}

impl Gate {
    /// The pkg, when it is due for the device of `record` (whose serial is `serial` and whose
    /// enrollment began at `since`); whether the look is done: it sent the pkg, or changed the
    /// record, or the enrollment did not answer.
    pub(crate) async fn send_pkg(
        &self,
        side: &EnrollSide,
        mut record: DeviceRecord,
        serial: &str,
        since: u64,
        now: u64,
    ) -> bool {
        let Some(install) = self.install() else {
            return false;
        };
        let configured = record
            .config_profile
            .as_ref()
            .and_then(ProfileCommand::acknowledged_unix)
            .is_some();
        if !configured || !due(&record, now) {
            return false;
        }
        let waiting = match side.enrollments.waiting(serial) {
            Ok(waiting) => waiting,
            Err(why) => {
                tracing::warn!(enrollment_id = %record.enrollment_id, %why, "the enrollment did not list the Mac's requests");
                return true;
            }
        };
        if !candidates(&record, &waiting, since).is_empty() {
            return false;
        }
        let entry = match install.checked() {
            Ok(entry) => entry.clone(),
            Err(why) => {
                tracing::warn!(enrollment_id = %record.enrollment_id, %why, "the pkg is not sent");
                let error = Some(why.to_string());
                if record.last_error != error {
                    record.last_error = error;
                    self.kept(&record);
                }
                return true;
            }
        };
        let Some([uuid]) = self.uuids(&mut record) else {
            return true;
        };
        let mut sent = InstallRecord {
            sha256: entry.sha256.clone(),
            sends: record.install.as_ref().map_or(0, |install| install.sends),
            last: ProfileCommand::new(uuid.clone(), now),
        };
        record.install = Some(sent.clone());
        let command = Command::install_application(uuid, manifest(&entry, &install.host));
        if !self.dispatch(&mut record, &command, now).await {
            return true;
        }
        sent.sends = sent.sends.saturating_add(1);
        let sends = sent.sends;
        record.install = Some(sent);
        let reason = format!(
            "serial {serial}; sha256 {}; version {}; send {sends}",
            entry.sha256,
            entry.version.as_str()
        );
        tracing::info!(
            enrollment_id = %record.enrollment_id,
            serial,
            sha256 = %entry.sha256,
            send = sends,
            "sent the pkg"
        );
        side.enrollments.audit(ACTION_INSTALL_SENT, &reason);
        self.kept(&record);
        true
    }
}
