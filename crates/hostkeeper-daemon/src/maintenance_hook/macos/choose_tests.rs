//! The tests of what the `macos` kind's `cutover` chooses, and refuses, over the fake Mac of
//! `apply_tests.rs`: which of several updates, which build, and the Mac states it does not install
//! on. Each outcome is asserted by equality of the whole result, each choice by the plan's bytes.

use super::*;

/// `fdesetup status` while FileVault is being turned on, and off.
const FILEVAULT_ENCRYPTING: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_encrypting.txt");
const FILEVAULT_DECRYPTING: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/fdesetup_decrypting.txt");

/// `softwareupdate --list` offering macOS Tahoe 26.6.1 and 26.7, in the order `first`, `second`.
fn two_minors(first: &str, second: &str) -> String {
    let entry = |label: &str, version: &str| {
        format!(
            "* Label: {label}\n\tTitle: macOS Tahoe {version}, Version: {version}, Size: \
             1024KiB, Recommended: YES, Action: restart, \n"
        )
    };
    let version = |label: &str| {
        label
            .trim_start_matches("macOS Tahoe ")
            .split('-')
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    format!(
        "{}{}",
        entry(first, &version(first)),
        entry(second, &version(second))
    )
}

/// The plan of a minor update by `label` to `build`, as the file holds it.
fn minor_plan(build: &str, label: &str) -> String {
    format!(r#"{{"build":"{build}","label":"{label}","runId":"{RUN}"}}"#)
}

/// Proves: of two macOS updates of the running major, the newest is installed, whichever order
/// `--list` gives them in, with the build its label ends with.
///
/// Catches: the first or the last listed update installed, so the Mac lands on an older build
/// than the one it is offered.
#[test]
fn the_newest_of_several_minor_updates_is_chosen_in_either_order() {
    let older = "macOS Tahoe 26.6.1-25G110";
    let lists = [two_minors(older, TAHOE_26_7), two_minors(TAHOE_26_7, older)];
    for (index, list) in lists.iter().enumerate() {
        let files = Files::new(&format!("macos-choose-newest-{index}"));
        let mac = Mac::on_26(list);
        assert_eq!(
            mac.apply(&files).cutover(&params(false)),
            Ok(Cutover::NeedsReboot)
        );
        assert_eq!(files.plan(), minor_plan("25H62", TAHOE_26_7));
    }
}

/// Proves: with a major upgrade allowed but no full installer of a higher major offered, the
/// minor update is chosen, after both listings.
///
/// Catches: an allowed but unoffered upgrade blocking the minor update, or a full installer of
/// the running major taken for an upgrade.
#[test]
fn an_allowed_upgrade_that_is_not_offered_leaves_the_minor_update() {
    let files = Files::new("macos-choose-no-upgrade");
    let mac = Mac::on_26(LIST_MAJOR);
    let only_running = "* Title: macOS Tahoe, Version: 26.7, Size: 15204352KiB, Build: 25H62, \
                        Deferred: NO\n";
    mac.look(
        SOFTWAREUPDATE,
        &["--list-full-installers"],
        ok(only_running),
    );
    assert_eq!(
        mac.apply(&files).cutover(&params(true)),
        Ok(Cutover::NeedsReboot)
    );
    assert_eq!(files.plan(), minor_plan("25H62", TAHOE_26_7));
    assert_eq!(
        mac.effects(),
        [
            looks_of_minor(),
            strings(&["run /usr/sbin/softwareupdate --list-full-installers"])
        ]
        .concat()
    );
}

/// Proves: a label whose last `-` is followed by nothing, or by more than letters and digits,
/// names no build, and the `cutover` fails `macos_target_unknown` keeping no plan.
///
/// Catches: a target build guessed from a label Apple wrote in another shape, which `verify`
/// could then never match.
#[test]
fn a_label_whose_build_is_not_letters_and_digits_names_no_build() {
    let files = Files::new("macos-choose-label");
    for label in ["macOS Tahoe 26.7-", "macOS Tahoe 26.7-25H62 beta"] {
        let mac = Mac::on_26(&format!(
            "* Label: {label}\n\tTitle: macOS Tahoe 26.7, Version: 26.7, Size: 1024KiB, \n"
        ));
        assert_eq!(
            mac.apply(&files).cutover(&params(false)).map(|_| ()),
            failure(
                REASON_TARGET_UNKNOWN,
                &format!("the label \"{label}\" names no build after its last '-'")
            )
        );
    }
    assert!(!files.run_dir.join(PLAN_FILE).exists());
}

/// Proves: a `sw_vers` version with no major number fails `macos_look_failed`, naming it, before
/// anything is listed.
///
/// Catches: a Mac whose version cannot be read offered every update of every major.
#[test]
fn a_version_with_no_major_number_fails_the_look() {
    let files = Files::new("macos-choose-version");
    let mac = Mac::on_26(LIST_MAJOR);
    mac.look(
        SW_VERS,
        &[],
        ok("ProductVersion:\tTahoe\nBuildVersion:\t25G62\n"),
    );
    assert_eq!(
        mac.apply(&files).cutover(&params(false)).map(|_| ()),
        failure(
            REASON_LOOK_FAILED,
            "sw_vers named the version \"Tahoe\", which has no major number"
        )
    );
    assert_eq!(
        mac.effects(),
        ["run /usr/bin/fdesetup status", "run /usr/bin/sw_vers"]
    );
}

/// Proves: FileVault being turned on or off is not off: each fails `filevault_not_off`, naming
/// the state.
///
/// Catches: a Mac mid-conversion restarted into a pre-boot login.
#[test]
fn filevault_encrypting_or_decrypting_is_not_off() {
    let files = Files::new("macos-choose-filevault");
    for (text, state) in [
        (FILEVAULT_ENCRYPTING, "encrypting"),
        (FILEVAULT_DECRYPTING, "decrypting"),
    ] {
        let mac = Mac::on_26(LIST_MAJOR);
        mac.look(FDESETUP, &["status"], ok(text));
        assert_eq!(
            mac.apply(&files).cutover(&params(false)).map(|_| ()),
            failure(
                REASON_FILEVAULT_NOT_OFF,
                &format!(
                    "FileVault is {state} (fdesetup status): its authenticated restart is not \
                     built, so the update is not installed"
                )
            )
        );
    }
}

/// Proves: a Mac with no `/Applications` holds no installer app: a major upgrade fails
/// `macos_installer_not_found` with none counted.
///
/// Catches: a missing directory read as an error that hides the reason, or as an app.
#[test]
fn a_mac_with_no_applications_directory_has_no_installer() {
    let files = Files::new("macos-choose-no-applications");
    fs::remove_dir(files.root.join("Applications")).expect("removed");
    let mac = Mac::on_26(LIST_MAJOR);
    assert_eq!(
        mac.apply(&files).cutover(&params(true)).map(|_| ()),
        failure(
            REASON_INSTALLER_NOT_FOUND,
            "/Applications holds 0 apps named Install macOS *.app (), not one"
        )
    );
}
