//! The bounds of a maintenance report (`hostkeeper_proto::maintenance`, the numbers
//! `hostkeeper/agent/v1/maintenance.proto` gives each list and text): a report at every bound is
//! accepted and fits its budget, and one a single entry or byte over any bound is refused, naming
//! it.

use prost::Message as _;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::maintenance::{
    MAX_APPLIED, MAX_DUE, MAX_DUE_LABEL, MAX_DUE_VERSION, MAX_HALTED_REASON, MAX_KERNEL_TEXT,
    MAX_MAC_TEXT, MAX_MDM_CONFLICTS, MAX_OS_TEXT, MAX_REPORT_BYTES, MAX_RUN_ID, MAX_XCODE_PATH,
    MAX_XCODE_VERSION, MAX_XCODES, MaintenanceReportError, check_report,
};

#[path = "support/maintenance_reports.rs"]
mod maintenance_reports;
use maintenance_reports::{at_every_bound, linux_report, text};

/// Catches: a bound's constant changed from the number the schema gives, or a report at every
/// bound refused or over its budget (and so over its share of the heartbeat's
/// 16 KiB): every list and text at its bound, and every number at its largest, passes the check
/// and encodes to at most 5120 bytes; so does a Linux host's report with no Mac facts and no run,
/// and the empty report.
#[test]
fn a_report_at_every_bound_is_accepted_within_its_budget() {
    assert_eq!(
        [
            MAX_XCODES,
            MAX_DUE,
            MAX_MDM_CONFLICTS,
            MAX_APPLIED,
            MAX_OS_TEXT,
            MAX_KERNEL_TEXT,
            MAX_XCODE_VERSION,
            MAX_XCODE_PATH,
            MAX_MAC_TEXT,
            MAX_DUE_VERSION,
            MAX_DUE_LABEL,
            MAX_RUN_ID,
            MAX_HALTED_REASON,
            MAX_REPORT_BYTES,
        ],
        [6, 12, 16, 4, 32, 64, 16, 128, 64, 32, 64, 64, 256, 5120]
    );
    let full = at_every_bound();
    assert_eq!(check_report(&full), Ok(()));
    assert!(
        full.encoded_len() <= MAX_REPORT_BYTES,
        "{} bytes",
        full.encoded_len()
    );
    assert_eq!(check_report(&linux_report()), Ok(()));
    assert_eq!(check_report(&agent::MaintenanceReport::default()), Ok(()));
    // A bound is in bytes: 32 two-byte characters are a label's 64 bytes, and pass.
    let mut wide = linux_report();
    wide.due[0].label = "é".repeat(MAX_DUE_LABEL / 2);
    assert_eq!(check_report(&wide), Ok(()));
}

/// One way to break a bound of [`at_every_bound`], and the refusal it is to meet.
type Breach = (fn(&mut agent::MaintenanceReport), MaintenanceReportError);

/// A list one entry over its bound.
fn too_many(field: &'static str, max: usize) -> MaintenanceReportError {
    MaintenanceReportError::TooMany {
        field,
        count: max + 1,
        max,
    }
}

/// A text one byte over its bound.
fn too_long(field: &'static str, max: usize) -> MaintenanceReportError {
    MaintenanceReportError::TooLong {
        field,
        len: max + 1,
        max,
    }
}

/// The host facts of a report.
fn host(report: &mut agent::MaintenanceReport) -> &mut agent::MaintenanceHost {
    report.host.as_mut().expect("the host")
}

/// The Mac facts of a report.
fn mac(report: &mut agent::MaintenanceReport) -> &mut agent::MacUpdateFacts {
    report.mac.as_mut().expect("the Mac facts")
}

/// The run of a report.
fn run(report: &mut agent::MaintenanceReport) -> &mut agent::MaintenanceRunState {
    report.run.as_mut().expect("the run")
}

/// Catches: a bound not checked, checked one off, or checked on another field than its own: a
/// report at every bound but one list one entry longer, or one text one byte longer (the last of
/// a list's entries, or a two-byte character over a bound in bytes), is refused naming that list
/// or text, its size and its bound. The refusal's words name them too.
#[test]
fn a_report_one_over_any_bound_is_refused_naming_it() {
    let breaches: [Breach; 22] = [
        (
            |r| r.xcodes.push(r.xcodes[0].clone()),
            too_many("xcodes", MAX_XCODES),
        ),
        (
            |r| mac(r).mdm_conflicts.push(String::new()),
            too_many("mac.mdm_conflicts", MAX_MDM_CONFLICTS),
        ),
        (|r| r.due.push(r.due[0].clone()), too_many("due", MAX_DUE)),
        (
            |r| run(r).applied.push(0),
            too_many("run.applied", MAX_APPLIED),
        ),
        (
            |r| host(r).os_name.push('a'),
            too_long("host.os_name", MAX_OS_TEXT),
        ),
        (
            |r| host(r).os_version.push('a'),
            too_long("host.os_version", MAX_OS_TEXT),
        ),
        (
            |r| host(r).os_build.push('a'),
            too_long("host.os_build", MAX_OS_TEXT),
        ),
        (
            |r| host(r).hardware_serial.push('a'),
            too_long("host.hardware_serial", MAX_OS_TEXT),
        ),
        (
            |r| host(r).running_kernel.push('a'),
            too_long("host.running_kernel", MAX_KERNEL_TEXT),
        ),
        (
            |r| host(r).newest_kernel.push('a'),
            too_long("host.newest_kernel", MAX_KERNEL_TEXT),
        ),
        (
            |r| host(r).boot_id.push('a'),
            too_long("host.boot_id", MAX_KERNEL_TEXT),
        ),
        (
            |r| r.xcodes[MAX_XCODES - 1].version.push('a'),
            too_long("xcodes.version", MAX_XCODE_VERSION),
        ),
        (
            |r| r.xcodes[MAX_XCODES - 1].build.push('a'),
            too_long("xcodes.build", MAX_XCODE_VERSION),
        ),
        (
            |r| r.xcodes[MAX_XCODES - 1].path.push('a'),
            too_long("xcodes.path", MAX_XCODE_PATH),
        ),
        (
            |r| {
                mac(r)
                    .command_line_tools
                    .as_mut()
                    .expect("the tools")
                    .version
                    .push('a');
            },
            too_long("mac.command_line_tools.version", MAX_MAC_TEXT),
        ),
        (
            |r| mac(r).mdm_conflicts[MAX_MDM_CONFLICTS - 1].push('a'),
            too_long("mac.mdm_conflicts", MAX_MAC_TEXT),
        ),
        (
            |r| r.due[MAX_DUE - 1].from.push('a'),
            too_long("due.from", MAX_DUE_VERSION),
        ),
        (
            |r| r.due[MAX_DUE - 1].to.push('a'),
            too_long("due.to", MAX_DUE_VERSION),
        ),
        (
            |r| r.due[MAX_DUE - 1].label.push('a'),
            too_long("due.label", MAX_DUE_LABEL),
        ),
        (
            |r| run(r).run_id.push('a'),
            too_long("run.run_id", MAX_RUN_ID),
        ),
        (
            |r| run(r).order_id.push('a'),
            too_long("run.order_id", MAX_RUN_ID),
        ),
        (
            |r| run(r).halted_reason.push('a'),
            too_long("run.halted_reason", MAX_HALTED_REASON),
        ),
    ];
    for (breach, want) in breaches {
        let mut report = at_every_bound();
        breach(&mut report);
        assert_eq!(check_report(&report), Err(want));
    }

    let mut wide = linux_report();
    wide.due[1].label = format!("{}é", text(MAX_DUE_LABEL - 1));
    assert_eq!(
        check_report(&wide),
        Err(MaintenanceReportError::TooLong {
            field: "due.label",
            len: MAX_DUE_LABEL + 1,
            max: MAX_DUE_LABEL,
        })
    );
    assert_eq!(
        too_many("due", MAX_DUE).to_string(),
        "due holds 13 entries, more than 12"
    );
    assert_eq!(
        too_long("run.halted_reason", MAX_HALTED_REASON).to_string(),
        "run.halted_reason is 257 bytes, more than 256"
    );
}

/// Catches: a list over its bound reported as one of its texts instead: the counts are checked
/// before the texts, so a report with both too many due items and a host text too long is
/// refused for the items.
#[test]
fn the_lists_are_checked_before_the_texts() {
    let mut report = at_every_bound();
    host(&mut report).os_name.push('a');
    report.due.push(agent::DueItem::default());
    assert_eq!(check_report(&report), Err(too_many("due", MAX_DUE)));
}
