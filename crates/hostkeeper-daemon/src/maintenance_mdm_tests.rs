//! The tests of `maintenance_mdm`: the collector over a scratch directory standing for the
//! host's files, filled with fixtures at their host paths; the sections and the errors are
//! asserted by equality.

use std::collections::BTreeSet;
use std::fs;

use hostkeeper_proto::maintenance::MAX_MDM_CONFLICTS;
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_collect::LinuxScope;

/// The bytes of the fixture `path` under `tests/fixtures/maintenance/mdm`.
macro_rules! fixture {
    ($path:literal) => {
        &include_bytes!(concat!("../tests/fixtures/maintenance/mdm/", $path))[..]
    };
}

const RESTRICTIONS_PATH: &str = "/Library/Managed Preferences/com.apple.applicationaccess.plist";
const SOFTWARE_UPDATE_PATH: &str = "/Library/Managed Preferences/com.apple.SoftwareUpdate.plist";
const EMPTY: &[u8] = b"";
const RECEIPT: &str = "/Applications/Xcode.app/Contents/_MASReceipt/receipt";

/// Each of the 16 names, the host path of the file that sets it and the fixture holding that
/// one key on.
fn cases() -> Vec<(&'static str, &'static str, &'static [u8])> {
    vec![
        (
            ENFORCED,
            DDM_STATE,
            fixture!("ddm/enforcement-specific.plist"),
        ),
        (DEFERRALS, DDM_STATE, fixture!("ddm/deferrals.plist")),
        (
            AUTOMATIC_ACTIONS,
            DDM_STATE,
            fixture!("ddm/automatic-actions.plist"),
        ),
        (BETA, DDM_STATE, fixture!("ddm/beta.plist")),
        (
            "forceDelayedSoftwareUpdates",
            RESTRICTIONS_PATH,
            fixture!("restrictions/forceDelayedSoftwareUpdates.plist"),
        ),
        (
            "forceDelayedMajorSoftwareUpdates",
            RESTRICTIONS_PATH,
            fixture!("restrictions/forceDelayedMajorSoftwareUpdates.plist"),
        ),
        (
            "forceDelayedAppSoftwareUpdates",
            RESTRICTIONS_PATH,
            fixture!("restrictions/forceDelayedAppSoftwareUpdates.plist"),
        ),
        (
            "enforcedSoftwareUpdateDelay",
            RESTRICTIONS_PATH,
            fixture!("restrictions/enforcedSoftwareUpdateDelay.plist"),
        ),
        (
            "enforcedSoftwareUpdateMajorOSDeferredInstallDelay",
            RESTRICTIONS_PATH,
            fixture!("restrictions/enforcedSoftwareUpdateMajorOSDeferredInstallDelay.plist"),
        ),
        (
            "enforcedSoftwareUpdateMinorOSDeferredInstallDelay",
            RESTRICTIONS_PATH,
            fixture!("restrictions/enforcedSoftwareUpdateMinorOSDeferredInstallDelay.plist"),
        ),
        (
            "enforcedSoftwareUpdateNonOSDeferredInstallDelay",
            RESTRICTIONS_PATH,
            fixture!("restrictions/enforcedSoftwareUpdateNonOSDeferredInstallDelay.plist"),
        ),
        (
            "CatalogURL",
            SOFTWARE_UPDATE_PATH,
            fixture!("softwareupdate/CatalogURL.plist"),
        ),
        (
            "AllowPreReleaseInstallation",
            SOFTWARE_UPDATE_PATH,
            fixture!("softwareupdate/AllowPreReleaseInstallation.plist"),
        ),
        (
            "AutomaticallyInstallMacOSUpdates",
            SOFTWARE_UPDATE_PATH,
            fixture!("softwareupdate/AutomaticallyInstallMacOSUpdates.plist"),
        ),
        (
            "restrict-software-update-require-admin-to-install",
            SOFTWARE_UPDATE_PATH,
            fixture!("softwareupdate/restrict-software-update-require-admin-to-install.plist"),
        ),
    ]
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn put(scratch: &Scratch, path: &str, bytes: &[u8]) {
    let file = scratch.path().join(path.trim_start_matches('/'));
    fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    fs::write(file, bytes).expect("the file");
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// A Mac whose files are those put in its scratch directory.
struct Mac {
    root: Scratch,
}

impl Mac {
    fn new(name: &str, files: &[(&str, &[u8])]) -> Mac {
        let root = scratch(name);
        for (path, bytes) in files {
            put(&root, path, bytes);
        }
        Mac { root }
    }

    fn collect(&self) -> Result<Section, String> {
        let own = BTreeSet::new();
        let look = Look {
            scope: LinuxScope::Security,
            own_packages: &own,
        };
        MdmConflicts::new(
            Box::new(HostPlists::new(HostRoot::at(self.root.path()))),
            HostRoot::at(self.root.path()),
        )
        .collect(&look)
    }

    fn conflicts(&self) -> Vec<String> {
        self.collect()
            .expect("a section")
            .mac
            .expect("the Mac's facts")
            .mdm_conflicts
    }
}

fn section(conflicts: &[&str]) -> Section {
    Section {
        mac: Some(pb::MacUpdateFacts {
            mdm_conflicts: names(conflicts),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Proves the collector's name, which a failed look's reason starts with, and that the one over
/// the host's own files is built.
#[test]
fn the_collector_is_named_mdm_and_is_built_over_the_host() {
    assert_eq!(MdmConflicts::at_host().name(), "mdm");
}

/// Proves a Mac with none of the files is clean: the Mac's facts are there with no conflict.
#[test]
fn a_mac_with_no_files_is_clean_with_its_facts_present() {
    let mac = Mac::new("mdm-clean", &[]);
    assert_eq!(mac.collect(), Ok(section(&[])));
}

/// Proves each of the 16 names is reported alone by the one key that sets it on.
#[test]
fn each_key_set_on_reports_exactly_its_name() {
    let cases = cases();
    assert_eq!(cases.len(), MAX_MDM_CONFLICTS - 1);
    for (name, path, bytes) in cases {
        let mac = Mac::new("mdm-each", &[(path, bytes)]);
        assert_eq!(mac.collect(), Ok(section(&[name])), "{name}");
    }
}

/// Proves the sixteenth name, the App Store's Xcode, is reported for an Xcode holding a receipt.
#[test]
fn an_xcode_with_an_app_store_receipt_reports_xcode_app_store() {
    let mac = Mac::new("mdm-receipt", &[(RECEIPT, EMPTY)]);
    assert_eq!(mac.collect(), Ok(section(&[APP_STORE_XCODE])));
}

/// Proves a Mac with every key set reports all 16 names, in the order of the design.
#[test]
fn a_mac_with_every_key_set_reports_all_sixteen_in_order() {
    let mac = Mac::new(
        "mdm-all",
        &[
            (DDM_STATE, fixture!("ddm/all.plist")),
            (RESTRICTIONS_PATH, fixture!("restrictions/all.plist")),
            (SOFTWARE_UPDATE_PATH, fixture!("softwareupdate/all.plist")),
            (RECEIPT, EMPTY),
        ],
    );
    let expected = [
        ENFORCED,
        DEFERRALS,
        AUTOMATIC_ACTIONS,
        BETA,
        "forceDelayedSoftwareUpdates",
        "forceDelayedMajorSoftwareUpdates",
        "forceDelayedAppSoftwareUpdates",
        "enforcedSoftwareUpdateDelay",
        "enforcedSoftwareUpdateMajorOSDeferredInstallDelay",
        "enforcedSoftwareUpdateMinorOSDeferredInstallDelay",
        "enforcedSoftwareUpdateNonOSDeferredInstallDelay",
        "CatalogURL",
        "AllowPreReleaseInstallation",
        "AutomaticallyInstallMacOSUpdates",
        "restrict-software-update-require-admin-to-install",
        APP_STORE_XCODE,
    ];
    assert_eq!(expected.len(), MAX_MDM_CONFLICTS);
    assert_eq!(mac.collect(), Ok(section(&expected)));
}

/// Proves [`reported`] is every name the collector reports, in its order, each once: a Mac with
/// every key set reports exactly that list, and each key set alone reports one name of it.
#[test]
fn reported_is_what_a_mac_with_every_key_set_reports() {
    let mac = Mac::new(
        "mdm-reported",
        &[
            (DDM_STATE, fixture!("ddm/all.plist")),
            (RESTRICTIONS_PATH, fixture!("restrictions/all.plist")),
            (SOFTWARE_UPDATE_PATH, fixture!("softwareupdate/all.plist")),
            (RECEIPT, EMPTY),
        ],
    );
    let reported = reported();
    assert_eq!(mac.collect(), Ok(section(&reported)));
    assert_eq!(reported.len(), MAX_MDM_CONFLICTS);
    assert_eq!(
        reported.iter().collect::<BTreeSet<_>>().len(),
        reported.len()
    );
    let alone: Vec<&str> = cases().iter().map(|(name, _, _)| *name).collect();
    assert_eq!(alone[..], reported[..reported.len() - 1]);
    assert_eq!(reported.last(), Some(&APP_STORE_XCODE));
}

/// Proves keys set off (false, 0) and keys that name nothing of software updates report none.
#[test]
fn keys_set_off_and_unrelated_keys_report_none() {
    let mac = Mac::new(
        "mdm-off",
        &[
            (DDM_STATE, fixture!("ddm/off.plist")),
            (RESTRICTIONS_PATH, fixture!("restrictions/off.plist")),
            (SOFTWARE_UPDATE_PATH, fixture!("softwareupdate/off.plist")),
        ],
    );
    assert_eq!(mac.collect(), Ok(section(&[])));
    let unrelated = Mac::new(
        "mdm-unrelated",
        &[(RESTRICTIONS_PATH, fixture!("restrictions/unrelated.plist"))],
    );
    assert_eq!(unrelated.collect(), Ok(section(&[])));
}

/// Proves a target build version enforces an update like a target OS version, and that the
/// second automatic key and a state without the settings declaration are read.
#[test]
fn the_declarations_are_read_by_every_shape_they_take() {
    let build = Mac::new(
        "mdm-build",
        &[(DDM_STATE, fixture!("ddm/enforcement-build.plist"))],
    );
    assert_eq!(build.conflicts(), names(&[ENFORCED]));
    let security = Mac::new(
        "mdm-security",
        &[(DDM_STATE, fixture!("ddm/automatic-actions-security.plist"))],
    );
    assert_eq!(security.conflicts(), names(&[AUTOMATIC_ACTIONS]));
    let bare = Mac::new("mdm-bare", &[(DDM_STATE, fixture!("ddm/bare.plist"))]);
    assert_eq!(bare.conflicts(), names(&[]));
    let none = Mac::new(
        "mdm-no-settings",
        &[(DDM_STATE, fixture!("ddm/no-settings.plist"))],
    );
    assert_eq!(none.conflicts(), names(&[]));
}

/// Proves binary property lists, as a Mac writes the files of Managed Preferences, are read the
/// same as XML ones: the fixtures of the restrictions and the enforcement are binary.
#[test]
fn a_binary_property_list_is_read_like_an_xml_one() {
    assert!(fixture!("restrictions/all.plist").starts_with(b"bplist00"));
    assert!(fixture!("ddm/enforcement-specific.plist").starts_with(b"bplist00"));
    assert!(fixture!("ddm/deferrals.plist").starts_with(b"<?xml"));
    let mac = Mac::new(
        "mdm-binary",
        &[
            (DDM_STATE, fixture!("ddm/deferrals.plist")),
            (
                RESTRICTIONS_PATH,
                fixture!("restrictions/enforcedSoftwareUpdateDelay.plist"),
            ),
        ],
    );
    assert_eq!(
        mac.conflicts(),
        names(&[DEFERRALS, "enforcedSoftwareUpdateDelay"])
    );
}

/// Proves a file that is there and is not a property list is reported unreadable, in each of the
/// three sources, and never as a clean Mac.
#[test]
fn an_unreadable_property_list_is_an_error_in_each_source() {
    for path in [DDM_STATE, RESTRICTIONS_PATH, SOFTWARE_UPDATE_PATH] {
        let mac = Mac::new("mdm-garbage", &[(path, fixture!("garbage.bin"))]);
        assert_eq!(mac.collect(), Err(format!("{path} is not a property list")));
    }
}

/// Proves a property list whose top is an array is reported as not a dictionary.
#[test]
fn a_property_list_with_an_array_on_top_is_an_error() {
    let mac = Mac::new(
        "mdm-array",
        &[(SOFTWARE_UPDATE_PATH, fixture!("softwareupdate/array.plist"))],
    );
    assert_eq!(
        mac.collect(),
        Err(format!("{SOFTWARE_UPDATE_PATH} is not a dictionary"))
    );
}

/// Proves an Xcode without a receipt, and an app other than an Xcode with one, add nothing.
#[test]
fn an_xcode_without_a_receipt_adds_nothing() {
    let mac = Mac::new(
        "mdm-no-receipt",
        &[
            ("/Applications/Xcode.app/Contents/Info.plist", EMPTY),
            (
                "/Applications/Other.app/Contents/_MASReceipt/receipt",
                EMPTY,
            ),
        ],
    );
    assert_eq!(mac.collect(), Ok(section(&[])));
}

/// Proves only the device's files at the top of Managed Preferences are read: a user's
/// subdirectory holding a software-update file with a catalog sets nothing.
#[test]
fn a_per_user_managed_preferences_file_is_not_read() {
    let mac = Mac::new(
        "mdm-user",
        &[(
            "/Library/Managed Preferences/someone/com.apple.SoftwareUpdate.plist",
            fixture!("softwareupdate/CatalogURL.plist"),
        )],
    );
    assert_eq!(mac.collect(), Ok(section(&[])));
}

/// Proves an Applications that cannot be listed fails the collector with the reason of the
/// listing.
#[test]
fn an_applications_that_is_not_a_directory_is_an_error() {
    let mac = Mac::new("mdm-applications-file", &[("/Applications", EMPTY)]);
    let error = mac.collect().expect_err("a failure");
    assert!(
        error.starts_with("/Applications could not be read: "),
        "{error}"
    );
}
