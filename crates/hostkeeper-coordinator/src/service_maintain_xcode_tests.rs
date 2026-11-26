//! A Mac's turn of a run that was scheduled before a newer Xcode archive was stored
//! (docs/design/xcode-one-action.md, section 2.5, step 2): the kinds of a run for a Mac are fixed
//! when it is scheduled, and the coordinator reads its Xcode store when the turn starts. So a turn
//! whose kinds for the Mac include `xcode` orders the archive stored since the run was scheduled,
//! and a turn whose kinds do not include `xcode` does not, whatever the store holds then. The
//! builders are those of `service_maintain_tests.rs`.

use std::io::Write;
use std::sync::Arc;

use super::*;
use crate::xcode_feed::{XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, XcodeStore};

/// The bytes of the archive the tests store.
const XIP: &[u8] = b"xip of 27B52";

/// A world of the group [`GROUP`] (batch 1, keeping one Xcode) of the Macs [`MACS`] over an Xcode
/// store that holds 27.1 (27A10), each Mac heard once with a macOS update and that Xcode due, and
/// the run of `kinds` scheduled for both.
fn macs_scheduled(name: &str, kinds: &[MaintenanceKind]) -> (World, Arc<XcodeStore>) {
    let mut world = crate::host_groups_support::World::new(name);
    let dir = world.rig.dir.path().join("xcode");
    std::fs::create_dir(&dir).expect("the store's directory");
    let store = Arc::new(XcodeStore::open(&dir, 1 << 20, std::time::UNIX_EPOCH).expect("a store"));
    let spare = crate::testutil::scratch(&format!("{name}-spare"));
    let placeholder = crate::marks_support::start(&spare, &world.rig.clock, "");
    let built = std::mem::replace(&mut world.rig.coordinator, placeholder);
    world.rig.coordinator = built.with_xcode_store(Arc::clone(&store));
    let mut settings = table(1, 0);
    settings.xcode_keep = 1;
    crate::decide_maintenance::tests::group_with(&world, GROUP, settings, &MACS);
    store_release(&store, "27.1", "27A10", b"xip of 27A10");
    for host in MACS {
        tell(&world, host, mac_report(clean_mac_facts(), None));
    }
    let mut asked = crate::maintenance_calls_tests::hosts_request(&MACS);
    asked.take_last_host = true;
    asked.kinds = kinds.iter().map(|kind| *kind as i32).collect();
    crate::maintenance_calls_tests::schedule(&world, asked).expect("scheduled");
    (world, store)
}

/// Stores `bytes` in `store` as the archive of Xcode `version` of `build`, as an upload does, and
/// returns its SHA-256.
fn store_release(store: &XcodeStore, version: &str, build: &str, bytes: &[u8]) -> String {
    let mut digest = Digesting::default();
    digest.feed(bytes);
    let sha256 = digest.finish();
    let (path, mut file) = store.upload_file(&sha256).expect("an upload file");
    file.write_all(bytes).expect("written");
    drop(file);
    let archive = Archive {
        sha256: sha256.clone(),
        id: XcodeId {
            version: XcodeVersion::parse(version).expect("a version"),
            build: build.to_owned(),
        },
        size: u64::try_from(bytes.len()).expect("a size"),
        by: "uploader".to_owned(),
        at_unix: 1_800_000_000,
    };
    let _changing = store.changing();
    store.commit(&path, &archive).expect("committed");
    sha256
}

/// Proves: a run scheduled with the kinds `macos` and `xcode` while the store held 27.1 alone
/// orders the Mac's turn, started after 27.2 (27B52) was stored, with the kinds `macos` then
/// `xcode` and one Xcode entry, 27.2 (27B52), of the new archive's SHA-256 and size.
///
/// Catches: a turn that fixes the Xcode order when the run is scheduled, so that an archive
/// added to the store after it is left out of every Mac's turn.
#[test]
fn a_turn_whose_kinds_include_xcode_orders_an_archive_stored_after_the_run() {
    let (world, store) = macs_scheduled(
        "sm-xcode-after",
        &[MaintenanceKind::Macos, MaintenanceKind::Xcode],
    );
    let sha256 = store_release(&store, "27.2", "27B52", XIP);

    let order = tell(&world, "m1", mac_report(clean_mac_facts(), None)).expect("an order");

    let Order::Maintain { maintain, .. } = &order else {
        panic!("a maintain: {order:?}");
    };
    assert_eq!(
        maintain.kinds,
        vec![MaintenanceKind::Macos, MaintenanceKind::Xcode]
    );
    let xcode = maintain.xcode.as_ref().expect("an Xcode order");
    let entries: Vec<(&str, &str, &str, u64)> = xcode
        .entries
        .iter()
        .map(|entry| {
            (
                entry.version.as_str(),
                entry.build.as_str(),
                entry.sha256.as_str(),
                entry.size,
            )
        })
        .collect();
    assert_eq!(
        entries,
        vec![(
            "27.2",
            "27B52",
            sha256.as_str(),
            u64::try_from(XIP.len()).expect("a size")
        )]
    );
}

/// Proves: a run scheduled with the kind `macos` alone, while Xcode 27.1 was due on the Mac,
/// orders the Mac's turn with the kind `macos` alone and no Xcode order, though the store holds
/// 27.2 (27B52) when the turn starts.
///
/// Catches: a turn that takes Xcode because the store holds a release, when the run's kinds for
/// the Mac, fixed when it was scheduled, do not name it (the install of an Xcode nobody asked
/// to install in that run).
#[test]
fn a_turn_whose_kinds_do_not_include_xcode_orders_none_though_the_store_holds_one() {
    let (world, store) = macs_scheduled("sm-xcode-not-asked", &[MaintenanceKind::Macos]);
    store_release(&store, "27.2", "27B52", XIP);

    let order = tell(&world, "m1", mac_report(clean_mac_facts(), None)).expect("an order");

    let Order::Maintain { maintain, .. } = &order else {
        panic!("a maintain: {order:?}");
    };
    assert_eq!(maintain.kinds, vec![MaintenanceKind::Macos]);
    assert_eq!(maintain.xcode, None);
}
