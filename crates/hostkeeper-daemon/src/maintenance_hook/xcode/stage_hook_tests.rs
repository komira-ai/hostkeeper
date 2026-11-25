//! The tests of the `xcode` kind's stage as the maintenance hook's `prepare` runs it: the
//! order's JSON on standard input, the kind registered as `xcode`, the outcome and the report line
//! the hook writes, each asserted by equality of the whole; and the stage's resume over an archive
//! an interrupted stage left. They use the fake Mac of the stage's own tests.

use hostkeeper_hook::Outcome;
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};

use super::*;
use crate::maintenance_hook::xcode::apply::Apply;
use crate::maintenance_hook::{
    HookArgs, HookPorts, HookStep, Kind, Registry, STDIN_PARAMS, Unwired, XcodeKind, main_with,
};

/// A boot id the hook's `prepare` has no need of.
struct Boot;

impl BootIdSource for Boot {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test", "boot-1")
    }
}

/// `maintenance-hook prepare --params -` over `mac`'s stage as the `xcode` kind, with the
/// parameters `json` on standard input: its outcome and its standard output.
fn hook(mac: &Mac, json: &str) -> (Outcome, String) {
    let kinds = Registry::new().with(
        Kind::Xcode,
        Box::new(XcodeKind::new(mac.stage(), Apply::at_host())),
    );
    let ports = HookPorts {
        kinds: &kinds,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &Boot,
    };
    let args = HookArgs {
        step: HookStep::Prepare,
        params: PathBuf::from(STDIN_PARAMS),
        package_lock_wait_secs: crate::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    };
    let mut stdout = Vec::new();
    let outcome = main_with(&args, &ports, &mut json.as_bytes(), &mut stdout);
    (outcome, String::from_utf8(stdout).expect("UTF-8"))
}

/// The order's JSON for a run of the `xcode` kind keeping `entry`.
fn order_json(entry: &XcodeEntry) -> String {
    format!(
        r#"{{"runId":"run-1","action":"MAINTAIN_ACTION_RUN","kinds":["MAINTENANCE_KIND_XCODE"],"xcodes":[{{"version":"{}","build":"{}","size":"{}","sha256":"{}"}}]}}"#,
        entry.version, entry.build, entry.size, entry.sha256
    )
}

/// Proves: `maintenance-hook prepare` with an `xcode` order, on a disk one byte short of what the
/// archive and its expansion need, ends deferred with the report of `free_space_short`, class
/// `environment`, and the whole detail, as the one line the hook writes; the store is not asked
/// and no program runs.
///
/// Catches: the xcode stage's deferral reported as a failure through the hook (which would halt
/// the run rather than leave the host in service), or an archive fetched on a disk too small.
#[test]
fn maintenance_hook_prepare_defers_an_xcode_order_on_a_full_disk() {
    let mut mac = Mac::new("xcs-hook-short");
    mac.space.free = Ok(needed() - 1);
    let json = order_json(&entry_of(ARCHIVE_BYTES));
    let detail = format!(
        "{} bytes are free on the volume of /Applications; staging needs {}",
        needed() - 1,
        needed()
    );
    assert_eq!(
        hook(&mac, &json),
        (
            Outcome::Deferred,
            format!(
                "{{\"class\":\"environment\",\"detail\":\"{detail}\",\"promotion_pending\":false,\"reason\":\"free_space_short\"}}\n"
            )
        )
    );
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

/// Proves: a copy an interrupted stage left in the entry's directory is resumed from its length,
/// and a file there longer than the archive is not taken for a part of it: it is cut and the
/// archive read from byte 0.
#[test]
fn a_copy_left_by_an_interrupted_stage_resumes_from_its_length() {
    let entry = entry_of(ARCHIVE_BYTES);
    let archive = format!("{STAGING}/{}/{ARCHIVE}", entry.sha256);
    let mut longer = ARCHIVE_BYTES.to_vec();
    longer.extend_from_slice(b"more");
    for (left, first) in [(ARCHIVE_BYTES[..4].to_vec(), 4), (longer, 0)] {
        let mac = Mac::new("xcs-left");
        mac.trusted_staging();
        mac.file(&archive, &left);
        assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()), "{first}");
        assert_eq!(
            mac.store.asked(),
            [(format!("/v1/xcode/{}", entry.sha256), first)]
        );
        assert_eq!(*lock(&mac.runner.archives), [ARCHIVE_BYTES.to_vec()]);
    }
}

/// Proves: a complete archive an interrupted stage left in the entry's directory, of the order's
/// size but other bytes, is not taken for the archive: the stage defers `xcode_archive_mismatch`
/// with the whole detail (its size and SHA-256 and the order's), the store is not asked, `xip`
/// never runs and the staging directory is removed.
///
/// Catches: a leftover of the right length trusted as the archive and expanded, or fetched over
/// from the store's bytes without being checked.
#[test]
fn a_complete_leftover_of_the_orders_size_and_other_bytes_is_refused_mismatched() {
    let entry = entry_of(ARCHIVE_BYTES);
    let archive = format!("{STAGING}/{}/{ARCHIVE}", entry.sha256);
    let mut other = ARCHIVE_BYTES.to_vec();
    other[5] = b'X';
    let mac = Mac::new("xcs-leftover-other");
    mac.trusted_staging();
    mac.file(&archive, &other);
    assert_eq!(
        mac.run(std::slice::from_ref(&entry)),
        deferral(
            REASON_ARCHIVE_MISMATCH,
            &format!(
                "the archive is {} bytes with SHA-256 {}; the order names {} bytes with SHA-256 {}",
                ARCHIVE_BYTES.len(),
                sha256(&other),
                entry.size,
                entry.sha256
            )
        )
    );
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    assert!(lock(&mac.runner.archives).is_empty());
    mac.assert_clean();
}
