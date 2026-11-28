//! The tests of `maintenance_softwareupdate`: `parse_softwareupdate_list` over the listings of
//! `softwareupdate --list` and `--list-full-installers` of each macOS release the farm runs (26
//! and 27), each read whole and compared by equality, and which of them `macos_due` makes due.

use super::*;

const MACOS26_RESTART: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_restart.txt");
const MACOS26_NO_RESTART: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_no_restart.txt");
const MACOS26_NONE: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_none.txt");
const MACOS26_MAJOR: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_list_major.txt");
const MACOS26_FULL: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos26_full_installers.txt");
const MACOS27_RESTART: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos27_list_restart.txt");
const MACOS27_NONE: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos27_list_none.txt");
const MACOS27_FULL: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos27_full_installers.txt");
const MACOS27_UNKNOWN: &str =
    include_str!("../tests/fixtures/maintenance/softwareupdate/macos27_list_unknown_line.txt");

fn update(label: &str, title: &str, version: &str, restart: bool) -> ListedUpdate {
    ListedUpdate {
        label: label.to_owned(),
        title: title.to_owned(),
        version: version.to_owned(),
        restart,
        size: None,
    }
}

fn installer(title: &str, version: &str, build: &str) -> FullInstaller {
    FullInstaller {
        title: title.to_owned(),
        version: version.to_owned(),
        build: build.to_owned(),
        size: None,
    }
}

/// `update` of a listing line whose `Size` is `kib` KiB.
fn sized(update: ListedUpdate, kib: u64) -> ListedUpdate {
    ListedUpdate {
        size: Some(kib * 1024),
        ..update
    }
}

/// `installer` of a listing line whose `Size` is `kib` KiB.
fn sized_installer(installer: FullInstaller, kib: u64) -> FullInstaller {
    FullInstaller {
        size: Some(kib * 1024),
        ..installer
    }
}

fn updates(list: Vec<ListedUpdate>, unparsed: u32) -> SoftwareUpdateListing {
    SoftwareUpdateListing {
        updates: list,
        installers: Vec::new(),
        unparsed,
    }
}

fn installers(list: Vec<FullInstaller>) -> SoftwareUpdateListing {
    SoftwareUpdateListing {
        updates: Vec::new(),
        installers: list,
        unparsed: 0,
    }
}

fn macos(from: &str, to: &str, needs_reboot: bool, label: &str) -> pb::DueItem {
    pb::DueItem {
        kind: pb::MaintenanceKind::Macos.into(),
        from: from.to_owned(),
        to: to.to_owned(),
        needs_reboot,
        label: label.to_owned(),
        security: false,
    }
}

/// Proves a macOS 26 listing of a minor update that says `Action: restart` is read with its label,
/// title, version, restart and size, the Command Line Tools beside it with no restart, and no line
/// unknown; the minor update is due with a reboot, from the running version, and the tools are not.
#[test]
fn a_minor_update_with_a_restart_is_due_with_a_reboot() {
    let listing = parse_softwareupdate_list(MACOS26_RESTART);
    assert_eq!(
        listing,
        updates(
            vec![
                sized(
                    update(
                        "Command Line Tools for Xcode 26.4-26.4",
                        "Command Line Tools for Xcode 26.4",
                        "26.4",
                        false,
                    ),
                    912_346,
                ),
                sized(
                    update(
                        "macOS Tahoe 26.6.1-25G110",
                        "macOS Tahoe 26.6.1",
                        "26.6.1",
                        true,
                    ),
                    3_215_472,
                ),
            ],
            0,
        )
    );
    let due = vec![macos("26.6", "26.6.1", true, "macOS Tahoe 26.6.1-25G110")];
    assert_eq!(macos_due("26.6", &listing), due);
}

/// Proves a macOS 27 listing of a minor update with a restart is read and made due the same way.
#[test]
fn a_macos_27_minor_update_is_due() {
    let listing = parse_softwareupdate_list(MACOS27_RESTART);
    assert_eq!(
        listing,
        updates(
            vec![sized(
                update("macOS 27.2-26C45", "macOS 27.2", "27.2", true),
                3_670_016
            )],
            0
        )
    );
    assert_eq!(
        macos_due("27.1", &listing),
        vec![macos("27.1", "27.2", true, "macOS 27.2-26C45")]
    );
}

/// Proves a minor update whose details name no `Action` is due with no reboot.
#[test]
fn a_minor_update_without_a_restart_is_due_with_no_reboot() {
    let listing = parse_softwareupdate_list(MACOS26_NO_RESTART);
    assert_eq!(
        listing,
        updates(
            vec![sized(
                update(
                    "macOS Tahoe 26.6.1 (a)-25G110a",
                    "macOS Tahoe 26.6.1 (a)",
                    "26.6.1 (a)",
                    false,
                ),
                98_304,
            )],
            0,
        )
    );
    assert_eq!(
        macos_due("26.6.1", &listing),
        vec![macos(
            "26.6.1",
            "26.6.1 (a)",
            false,
            "macOS Tahoe 26.6.1 (a)-25G110a"
        )]
    );
}

/// Proves a listing with no update, on macOS 26 and on macOS 27, reads as nothing, no line
/// unknown, and nothing is due.
#[test]
fn a_listing_with_no_update_has_nothing_due() {
    for (text, running) in [(MACOS26_NONE, "26.7"), (MACOS27_NONE, "27.2")] {
        let listing = parse_softwareupdate_list(text);
        assert_eq!(listing, SoftwareUpdateListing::default());
        assert_eq!(macos_due(running, &listing), Vec::new());
    }
    assert_eq!(
        parse_softwareupdate_list(""),
        SoftwareUpdateListing::default()
    );
}

/// Proves a major upgrade `--list` names on macOS 26 is read, and is due after the minor update
/// beside it, with a reboot: the daemon reports it whatever the group allows, which the
/// coordinator judges.
///
/// Catches: a major upgrade the Mac is offered left out of its report, so a group that allows one
/// never has it due.
#[test]
fn a_listed_major_upgrade_is_due_after_the_minor_update() {
    let listing = parse_softwareupdate_list(MACOS26_MAJOR);
    assert_eq!(
        listing,
        updates(
            vec![
                sized(
                    update("macOS Tahoe 26.7-25H62", "macOS Tahoe 26.7", "26.7", true),
                    4_718_592,
                ),
                sized(
                    update("macOS 27.1-26B70", "macOS 27.1", "27.1", true),
                    15_728_640,
                ),
            ],
            0,
        )
    );
    assert_eq!(
        macos_due("26.6.1", &listing),
        vec![
            macos("26.6.1", "26.7", true, "macOS Tahoe 26.7-25H62"),
            macos("26.6.1", "27.1", true, "macOS 27.1-26B70"),
        ]
    );
}

/// Proves the full installers of macOS 26 and 27 are read with title, version, build and size; on a
/// Mac of macOS 26 the installer of 27 is a major upgrade, due and named by its title, and the
/// installers of its own or a lower major version are never due.
#[test]
fn a_full_installer_of_a_higher_major_version_is_due() {
    let on_26 = parse_softwareupdate_list(MACOS26_FULL);
    assert_eq!(
        on_26,
        installers(vec![
            sized_installer(installer("macOS 27.1", "27.1", "26B70"), 16_252_928),
            sized_installer(installer("macOS Tahoe", "26.7", "25H62"), 15_204_352),
            sized_installer(installer("macOS Sequoia", "15.7.4", "24G517"), 14_680_064),
        ])
    );
    assert_eq!(
        macos_due("26.6.1", &on_26),
        vec![macos("26.6.1", "27.1", true, "macOS 27.1")]
    );
    let on_27 = parse_softwareupdate_list(MACOS27_FULL);
    assert_eq!(
        on_27,
        installers(vec![
            sized_installer(installer("macOS 27.2", "27.2", "26C45"), 16_515_072),
            sized_installer(installer("macOS Tahoe", "26.7", "25H62"), 15_204_352),
        ])
    );
    assert_eq!(macos_due("27.1", &on_27), Vec::new());
}

/// Proves that, of the major upgrades, only the newest is due: a full installer newer than
/// the listed upgrade wins, and of equal versions the listed update (with its label) wins.
#[test]
fn only_the_newest_major_upgrade_is_due() {
    let mut listing = parse_softwareupdate_list(MACOS26_MAJOR);
    listing.installers = parse_softwareupdate_list(MACOS26_FULL).installers;
    assert_eq!(
        macos_due("26.6.1", &listing)[1],
        macos("26.6.1", "27.1", true, "macOS 27.1-26B70")
    );
    listing
        .installers
        .push(installer("macOS 28", "28.0", "27A100"));
    listing
        .installers
        .push(installer("macOS 28 again", "28.0", "27A101"));
    assert_eq!(
        macos_due("26.6.1", &listing)[1],
        macos("26.6.1", "28.0", true, "macOS 28")
    );
}

/// Proves a line the parser does not know is counted and never guessed at: a details line with a
/// field it does not know still gives its update (and is counted once), a `* Label:` line followed
/// by an unknown line, the unknown line, and the details line after it with no label are each
/// counted, and nothing of them is read as an update.
#[test]
fn an_unknown_line_is_counted() {
    let listing = parse_softwareupdate_list(MACOS27_UNKNOWN);
    assert_eq!(
        listing,
        updates(
            vec![sized(
                update("macOS 27.2-26C45", "macOS 27.2", "27.2", true),
                3_670_016
            )],
            4
        )
    );
    assert_eq!(
        macos_due("27.1", &listing),
        vec![macos("27.1", "27.2", true, "macOS 27.2-26C45")]
    );
}

/// Proves each malformed shape of an update is counted: two labels in a row (the first counted),
/// a label at the end with no details, a label that is empty, details with no title or no version
/// (counted, no update), and details with a field twice, an `Action` or a `Recommended` it does not
/// know, or a piece that is no field (counted, the update kept, an unknown action no restart).
#[test]
fn malformed_updates_are_counted() {
    let text = "* Label: first\n\
                * Label: second\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Action: restart,\n\
                * Label: \n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7,\n\
                * Label: no-title\n\
                \tTitle: , Version: 26.7,\n\
                * Label: no-version\n\
                \tTitle: macOS Tahoe 26.7,\n\
                * Label: twice\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Version: 26.8,\n\
                * Label: action\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Action: log out,\n\
                * Label: shut-down\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Action: shut down,\n\
                * Label: recommended\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Recommended: MAYBE,\n\
                * Label: comma\n\
                \tTitle: Tools, for Xcode, Version: 26.4,\n\
                * Label: last\n";
    let listing = parse_softwareupdate_list(text);
    assert_eq!(
        listing,
        updates(
            vec![
                update("second", "macOS Tahoe 26.7", "26.7", true),
                update("twice", "macOS Tahoe 26.7", "26.7", false),
                update("action", "macOS Tahoe 26.7", "26.7", false),
                update("shut-down", "macOS Tahoe 26.7", "26.7", true),
                update("recommended", "macOS Tahoe 26.7", "26.7", false),
                update("comma", "Tools", "26.4", false),
            ],
            9,
        )
    );
}

/// Proves a full installer's line with no build, no version or no title is counted and gives no
/// installer, and one with an `Action` or a `Deferred` it does not know is counted and kept.
#[test]
fn malformed_installers_are_counted() {
    let text = "* Title: macOS 27.1, Version: 27.1, Deferred: NO\n\
                * Title: macOS 27.1, Build: 26B70\n\
                * Title: , Version: 27.1, Build: 26B70\n\
                * Title: macOS 27.1, Version: 27.1, Build: 26B70, Action: restart\n\
                * Title: macOS 27.1, Version: 27.1, Build: 26B70, Deferred: LATER\n";
    assert_eq!(
        parse_softwareupdate_list(text),
        SoftwareUpdateListing {
            updates: Vec::new(),
            installers: vec![
                installer("macOS 27.1", "27.1", "26B70"),
                installer("macOS 27.1", "27.1", "26B70"),
            ],
            unparsed: 5,
        }
    );
}

/// Proves an installer's line after a label that had no details counts the label, and the
/// installer is still read.
#[test]
fn an_installer_after_a_bare_label_counts_the_label() {
    let text = "* Label: bare\n* Title: macOS 27.1, Version: 27.1, Build: 26B70\n";
    assert_eq!(
        parse_softwareupdate_list(text),
        SoftwareUpdateListing {
            updates: Vec::new(),
            installers: vec![installer("macOS 27.1", "27.1", "26B70")],
            unparsed: 1,
        }
    );
}

/// Proves what is never due: an update that is not macOS, a macOS of a lower major version, and a
/// macOS whose version has no major number; and nothing is due when the running version has none.
#[test]
fn what_is_never_due() {
    let listing = updates(
        vec![
            update("Safari", "Safari 27.2", "27.2", false),
            update("old", "macOS Sequoia 15.7.4", "15.7.4", true),
            update("odd", "macOS Tahoe", "next", true),
            update("Tahoe", "macOS Tahoe 26.7", "26.7", true),
        ],
        0,
    );
    let mut with_installers = listing.clone();
    with_installers.installers = vec![
        installer("Safari", "28.0", "1"),
        installer("macOS odd", "x.1", "1"),
    ];
    assert_eq!(
        macos_due("26.6", &with_installers),
        vec![macos("26.6", "26.7", true, "Tahoe")]
    );
    assert_eq!(macos_due("", &listing), Vec::new());
    assert_eq!(macos_due("unknown", &listing), Vec::new());
}

/// Proves a due item's texts are cut to the wire's bounds: the label to 64 bytes, the versions to
/// 32.
#[test]
fn a_due_item_is_cut_to_its_bounds() {
    let label = "L".repeat(MAX_DUE_LABEL + 10);
    let version = format!("26.{}", "9".repeat(MAX_DUE_VERSION));
    let listing = updates(vec![update(&label, "macOS Tahoe", &version, true)], 0);
    let due = macos_due("26.1", &listing);
    assert_eq!(
        due,
        vec![macos(
            "26.1",
            &version[..MAX_DUE_VERSION],
            true,
            &label[..MAX_DUE_LABEL]
        )]
    );
}

/// Proves the major version is the number before the first `.`, and none when that is no number.
#[test]
fn the_major_version_is_the_first_number() {
    assert_eq!(major_of("26.2"), Some(26));
    assert_eq!(major_of("27"), Some(27));
    assert_eq!(major_of("26.6.1 (a)"), Some(26));
    assert_eq!(major_of("x.1"), None);
    assert_eq!(major_of(""), None);
}

/// Proves a `Size` is kept in bytes only when it is a whole number of KiB, on an update and on a
/// full installer, and that any other size is kept unknown without counting the line: another
/// unit, a fraction, no digits, a sign, and more bytes than `u64` holds.
///
/// Catches: a size in another unit read as KiB, or one too large wrapped to a small number, which
/// would let the stage's free-space check pass on a guess.
#[test]
fn a_size_is_kept_only_in_whole_kib() {
    let text = "* Label: kib\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Size: 12KiB,\n\
                * Label: mib\n\
                \tTitle: macOS Tahoe 26.7, Version: 26.7, Size: 12MiB,\n\
                * Title: macOS 27.1, Version: 27.1, Size: 3KiB, Build: 26B70\n\
                * Title: macOS 27.1, Version: 27.1, Size: 1.5KiB, Build: 26B70\n";
    assert_eq!(
        parse_softwareupdate_list(text),
        SoftwareUpdateListing {
            updates: vec![
                sized(update("kib", "macOS Tahoe 26.7", "26.7", false), 12),
                update("mib", "macOS Tahoe 26.7", "26.7", false),
            ],
            installers: vec![
                sized_installer(installer("macOS 27.1", "27.1", "26B70"), 3),
                installer("macOS 27.1", "27.1", "26B70"),
            ],
            unparsed: 0,
        }
    );
    assert_eq!(kib_bytes("0KiB"), Some(0));
    assert_eq!(kib_bytes("4718592KiB"), Some(4_831_838_208));
    let largest = u64::MAX / 1024;
    assert_eq!(kib_bytes(&format!("{largest}KiB")), Some(largest * 1024));
    for odd in [
        "KiB",
        "12",
        "12 KiB",
        "+12KiB",
        "-12KiB",
        "12kib",
        &format!("{}KiB", largest + 1),
        "99999999999999999999999KiB",
    ] {
        assert_eq!(kib_bytes(odd), None, "{odd}");
    }
}
