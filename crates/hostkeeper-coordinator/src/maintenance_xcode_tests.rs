//! Whether `xcode` is due on a Mac, and what its `maintain` carries ([`crate::maintenance_xcode`]),
//! through the coordinator: `Hosts.Get` (the item in the shown report), `Alerts.List` (the alert
//! `updates_due`), `Maintenance.Schedule` (the Mac's kinds) and the heartbeat that starts the
//! Mac's turn (the order), over an Xcode store in a directory of the test's own; and [`plan`]
//! itself for what the order is made of.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::Heartbeat;

use super::plan;
use crate::decide_maintenance::tests::{beat, table, tell};
use crate::host_groups::GroupName;
use crate::host_groups_support::{SELF, World};
use crate::maintenance::text_of;
use crate::maintenance_calls_tests::{
    GROUP, clean_mac, edit_runs, hosts_request, member, schedule,
};
use crate::maintenance_policy::{Maintenance, XcodeDefault};
use crate::maintenance_runs::{Act, Kind, Step};
use crate::marks_support::{operator, start};
use crate::orders::{Order, XcodeOrder};
use crate::testutil::scratch;
use crate::xcode_feed::{XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Digesting, XcodeStore};

/// The Mac of the tests.
const MAC: &str = "m1";

/// A Linux host of the Mac's group.
const LINUX_HOST: &str = "l1";

/// The order id of the first run's `maintain`.
const ORDER: &str = "maintain-run-1900000000-1";

/// The time the archives of the tests were stored, in unix seconds.
const AT: u64 = 1_800_000_000;

/// The Xcode `version` of build `build`.
fn xcode_id(version: &str, build: &str) -> XcodeId {
    XcodeId {
        version: XcodeVersion::parse(version).expect("a version number"),
        build: build.to_owned(),
    }
}

/// An archive of `version` and `build` as the store would list it, its SHA-256 made of `seed`.
fn archive(version: &str, build: &str, seed: char) -> Archive {
    Archive {
        sha256: seed.to_string().repeat(64),
        id: xcode_id(version, build),
        size: 1000,
        by: "uploader".to_owned(),
        at_unix: AT,
    }
}

/// An Xcode a Mac reports installed: in its own directory, `Xcode-<version>.app`, or, when
/// `foreign`, in `Xcode.app`.
fn installed(version: &str, build: &str, selected: bool, foreign: bool) -> agent::InstalledXcode {
    agent::InstalledXcode {
        version: version.to_owned(),
        build: build.to_owned(),
        path: if foreign {
            "/Applications/Xcode.app".to_owned()
        } else {
            format!("/Applications/Xcode-{version}.app")
        },
        selected,
        foreign,
    }
}

/// The maintenance table of a group keeping `keep` Xcodes with the default `default`.
fn keeping(keep: u32, default: XcodeDefault) -> Maintenance {
    Maintenance {
        xcode_keep: keep,
        xcode_default: default,
        ..Maintenance::default()
    }
}

/// A heartbeat of a daemon on `platform` that announces `maintenance-v1` and reports the Xcodes
/// `xcodes` installed, the due items `due` and the Mac facts of a Mac that may be ordered `macos`.
fn beat_on(
    platform: Platform,
    xcodes: &[agent::InstalledXcode],
    due: &[agent::DueItem],
) -> Heartbeat {
    let mut heartbeat = beat(&[], None);
    heartbeat.platform = platform;
    let report = agent::MaintenanceReport {
        xcodes: xcodes.to_vec(),
        due: due.to_vec(),
        mac: (platform == Platform::DarwinArm64).then(clean_mac),
        ..agent::MaintenanceReport::default()
    };
    heartbeat.maintenance = Some(text_of(&report));
    heartbeat
}

/// [`beat_on`] of a Mac with nothing of its own due.
fn mac_beat(xcodes: &[agent::InstalledXcode]) -> Heartbeat {
    beat_on(Platform::DarwinArm64, xcodes, &[])
}

/// A world of groups whose coordinator keeps Xcode archives in a directory of its own.
struct Farm {
    world: World,
    store: Arc<XcodeStore>,
    dir: PathBuf,
}

impl Farm {
    /// A farm named `name` with the group [`GROUP`] of batch 1, `min_serving` 0 and the Xcode
    /// settings `keep` and `default` (none: `latest`), holding `hosts`, not heard.
    fn new(name: &str, keep: u32, default: Option<&str>, hosts: &[&str]) -> Self {
        let mut world = World::new(name);
        let dir = world.rig.dir.path().join("xcode");
        fs::create_dir(&dir).expect("the store's directory");
        let store =
            Arc::new(XcodeStore::open(&dir, 1 << 20, std::time::UNIX_EPOCH).expect("a store"));
        let spare = scratch(&format!("{name}-spare"));
        let placeholder = start(&spare, &world.rig.clock, "");
        let built = std::mem::replace(&mut world.rig.coordinator, placeholder);
        world.rig.coordinator = built.with_xcode_store(Arc::clone(&store));
        let farm = Self { world, store, dir };
        farm.settings(keep, default);
        for host in hosts {
            member(&farm.world, GROUP, host);
        }
        farm
    }

    /// Puts [`GROUP`] again with the Xcode settings `keep` and `default` (none: `latest`).
    fn settings(&self, keep: u32, default: Option<&str>) {
        let mut settings = table(1, 0);
        settings.xcode_keep = keep;
        settings.xcode_default = default.map(str::to_owned);
        let policy = proto::RolloutPolicy {
            maintenance: Some(settings),
            ..proto::RolloutPolicy::default()
        };
        let name = GROUP.parse::<GroupName>().expect("a group name");
        let generation = self
            .world
            .coordinator()
            .desired
            .host_group(&name)
            .map(|group| group.generation);
        self.world
            .put_with(GROUP, &[SELF], Some(policy), generation)
            .expect("a group put");
    }

    /// Stores an archive of `version` and `build` whose bytes name the build, as an operator's
    /// push does.
    fn add(&self, version: &str, build: &str) -> Archive {
        let bytes = format!("xip of {build}").into_bytes();
        let mut digest = Digesting::default();
        digest.feed(&bytes);
        let sha256 = digest.finish();
        let (path, mut file) = self.store.upload_file(&sha256).expect("an upload file");
        file.write_all(&bytes).expect("written");
        drop(file);
        let archive = Archive {
            sha256,
            id: xcode_id(version, build),
            size: u64::try_from(bytes.len()).expect("a size"),
            by: "uploader".to_owned(),
            at_unix: AT,
        };
        let _changing = self.store.changing();
        self.store.commit(&path, &archive).expect("committed");
        archive
    }

    /// The items of `kind` `xcode` `Hosts.Get` shows in the report of `host`.
    fn shown(&self, host: &str) -> Vec<agent::DueItem> {
        self.report(host)
            .due
            .into_iter()
            .filter(|item| item.kind == MaintenanceKind::Xcode as i32)
            .collect()
    }

    /// The report `Hosts.Get` shows of `host`.
    fn report(&self, host: &str) -> agent::MaintenanceReport {
        self.world
            .coordinator()
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: host.to_owned(),
                },
            )
            .expect("a host")
            .host
            .expect("a host")
            .maintenance
            .expect("a report")
    }

    /// The messages of the alerts `updates_due` `Alerts.List` answers.
    fn updates_due(&self) -> Vec<String> {
        self.world
            .coordinator()
            .list_alerts(&operator(), proto::ListAlertsRequest {})
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind() == proto::AlertKind::UpdatesDue)
            .map(|alert| alert.message)
            .collect()
    }

    /// A confirmed `Schedule` of [`MAC`] of the kinds `kinds` (none: every kind due), which may
    /// take the group's last host.
    fn schedule(
        &self,
        kinds: &[MaintenanceKind],
    ) -> Result<proto::ScheduleMaintenanceResponse, Refusal> {
        let mut asked = hosts_request(&[MAC]);
        asked.take_last_host = true;
        asked.kinds = kinds.iter().map(|kind| *kind as i32).collect();
        schedule(&self.world, asked)
    }

    /// The kinds of [`MAC`]'s turn in the first run the world stores, and its step.
    fn turn(&self) -> (Vec<Kind>, Step) {
        let host = self.world.ops().maintenance_runs[0]
            .hosts
            .iter()
            .find(|host| host.host_id == MAC)
            .expect("the Mac is in the run")
            .clone();
        (host.kinds, host.step)
    }

    /// The `maintain` the coordinator answers `beat` of [`MAC`] with, if any.
    fn maintain(&self, beat: Heartbeat) -> Option<(crate::orders::Maintain, agent::Order)> {
        let order = tell(&self.world, MAC, beat)?;
        let wire = order.to_proto();
        let Order::Maintain {
            maintain, order_id, ..
        } = order
        else {
            return None;
        };
        assert_eq!(order_id, ORDER);
        Some((maintain, wire))
    }
}

/// The due item a Mac whose `xcode-select` names `from` is shown when the kept release `to` of
/// build `build` is missing, or is the default it does not name.
fn due_item(from: &str, to: &str, build: &str) -> agent::DueItem {
    agent::DueItem {
        kind: MaintenanceKind::Xcode as i32,
        from: from.to_owned(),
        to: to.to_owned(),
        needs_reboot: false,
        label: build.to_owned(),
        security: false,
    }
}

/// `archive` as a `maintain` names it.
fn entry_of(archive: &Archive) -> agent::XcodeEntry {
    agent::XcodeEntry {
        version: archive.id.version.as_str().to_owned(),
        build: archive.id.build.clone(),
        size: archive.size,
        sha256: archive.sha256.clone(),
    }
}

/// Proves: a Mac of a group keeping one Xcode that runs 27.1, which the store holds, has nothing
/// of Xcode due (`Hosts.Get` shows no item, no `updates_due` stands, a `Schedule` of `xcode` is
/// refused `nothing_due`); once the store holds 27.2, the Mac is shown due from 27.1 to 27.2 (its
/// build the label), `updates_due` counts it in its group, and a `Schedule` of `xcode` takes it
/// with the kind `xcode`. A newer release the Mac lacks makes nothing due until the store holds it.
///
/// Catches: a coordinator that leaves `xcode` to the daemon's report, which never lists one, so
/// no Mac is ever offered a new Xcode; or one that reads the release list rather than the store.
#[test]
fn a_mac_lacking_a_release_is_due_only_once_the_store_holds_it() {
    let farm = Farm::new("mx-due", 1, None, &[MAC]);
    farm.add("27.1", "27A10");
    tell(
        &farm.world,
        MAC,
        mac_beat(&[installed("27.1", "27A10", true, false)]),
    );
    assert_eq!(farm.shown(MAC), Vec::new());
    assert_eq!(farm.updates_due(), Vec::<String>::new());
    assert_eq!(
        farm.schedule(&[MaintenanceKind::Xcode]).err(),
        Some(Refusal::NothingDue)
    );

    farm.add("27.2", "27B52");
    assert_eq!(farm.shown(MAC), vec![due_item("27.1", "27.2", "27B52")]);
    assert_eq!(
        farm.updates_due(),
        vec![format!("Updates due on 1 host of {GROUP}")]
    );
    farm.schedule(&[MaintenanceKind::Xcode]).expect("scheduled");
    assert_eq!(farm.turn(), (vec![Kind::Xcode], Step::Waiting));
}

/// Proves: in a group whose `xcode_keep` is 0, a Mac with no Xcode at all is never due one,
/// whatever the store holds: no item, no `updates_due`, and a `Schedule` of `xcode` is refused
/// `nothing_due`. A Linux host of a group that keeps Xcode, reporting none, is never due one
/// either.
///
/// Catches: a due check that reads the store without the group's `xcode_keep`, which would offer
/// Xcode to every Mac of a group that leaves Xcode alone, or one that does not ask the platform.
#[test]
fn a_group_keeping_no_xcode_and_a_linux_host_are_never_due_one() {
    let farm = Farm::new("mx-keep0", 0, None, &[MAC]);
    farm.add("27.2", "27B52");
    tell(&farm.world, MAC, mac_beat(&[]));
    assert_eq!(farm.shown(MAC), Vec::new());
    assert_eq!(farm.updates_due(), Vec::<String>::new());
    assert_eq!(
        farm.schedule(&[MaintenanceKind::Xcode]).err(),
        Some(Refusal::NothingDue)
    );

    let linux = Farm::new("mx-linux", 1, None, &[LINUX_HOST]);
    linux.add("27.2", "27B52");
    tell(
        &linux.world,
        LINUX_HOST,
        beat_on(Platform::LinuxX86_64, &[], &[]),
    );
    assert_eq!(linux.shown(LINUX_HOST), Vec::new());
    assert_eq!(linux.updates_due(), Vec::<String>::new());
}

/// Proves: a Mac that holds both kept releases and selects the newest is not due; the group's
/// `xcode_default` moved to 27.1 makes it due from 27.2 to 27.1 (and counted by `updates_due`);
/// the Mac reporting 27.1 selected is not due again; and `latest` once more makes it due back to
/// 27.2. A default no kept release has (27.0, with one release kept) leaves `xcode-select` alone:
/// nothing due.
///
/// Catches: a due check that looks only for missing releases, so a moved default is never
/// applied, or one that orders a default the store does not keep, which no run could satisfy.
#[test]
fn a_default_xcode_select_does_not_name_is_due() {
    let farm = Farm::new("mx-default", 2, None, &[MAC]);
    farm.add("27.1", "27A10");
    farm.add("27.2", "27B52");
    let on = |selected: &str| {
        mac_beat(&[
            installed("27.1", "27A10", selected == "27.1", false),
            installed("27.2", "27B52", selected == "27.2", false),
        ])
    };
    tell(&farm.world, MAC, on("27.2"));
    assert_eq!(farm.shown(MAC), Vec::new());

    farm.settings(2, Some("27.1"));
    assert_eq!(farm.shown(MAC), vec![due_item("27.2", "27.1", "27A10")]);
    assert_eq!(
        farm.updates_due(),
        vec![format!("Updates due on 1 host of {GROUP}")]
    );
    tell(&farm.world, MAC, on("27.1"));
    assert_eq!(farm.shown(MAC), Vec::new());

    farm.settings(2, None);
    assert_eq!(farm.shown(MAC), vec![due_item("27.1", "27.2", "27B52")]);

    farm.settings(1, Some("27.0"));
    tell(&farm.world, MAC, on("27.2"));
    assert_eq!(farm.shown(MAC), Vec::new());
}

/// Proves: with the store holding 26.4, 27.1 and 27.2 and the group keeping two, the `maintain`
/// of a Mac that has 26.3 and 27.1 (selected) each in its own directory, and a foreign 26.0
/// carries exactly the entries of 27.2 and 27.1, newest first, each with the store's version,
/// build, size and SHA-256; removes 26.3 alone (never the foreign 26.0, nor the kept 27.1); and
/// names 27.2 the default, on the wire as in the typed order.
///
/// Catches: an order that names every archive of the store, or none; a removal list that holds a
/// foreign directory, which a person or device management put there; or a default left empty.
#[test]
fn the_order_carries_the_kept_entries_the_removals_and_the_default() {
    let farm = Farm::new("mx-order", 2, None, &[MAC]);
    farm.add("26.4", "26E50");
    let older = farm.add("27.1", "27A10");
    let newest = farm.add("27.2", "27B52");
    let xcodes = [
        installed("26.3", "26D30", false, false),
        installed("27.1", "27A10", true, false),
        installed("26.0", "26A00", false, true),
    ];
    tell(&farm.world, MAC, mac_beat(&xcodes));
    farm.schedule(&[]).expect("scheduled");
    assert_eq!(farm.turn().0, vec![Kind::Xcode]);

    let (maintain, wire) = farm.maintain(mac_beat(&xcodes)).expect("ordered");
    assert_eq!(maintain.kinds, vec![MaintenanceKind::Xcode]);
    assert_eq!(
        maintain.xcode,
        Some(XcodeOrder {
            entries: vec![entry_of(&newest), entry_of(&older)],
            remove: vec!["26.3".to_owned()],
            default: Some("27.2".to_owned()),
        })
    );
    let Some(agent::order::Verb::Maintain(said)) = wire.verb else {
        panic!("a maintain: {wire:?}")
    };
    assert_eq!(said.xcodes, vec![entry_of(&newest), entry_of(&older)]);
    assert_eq!(said.xcode_remove, vec!["26.3".to_owned()]);
    assert_eq!(said.xcode_default, "27.2");
}

/// Proves: a Mac with a macOS update and the newest Xcode due is taken by a `Schedule` of every
/// kind with `macos` and `xcode`, and its `maintain` names `macos` before `xcode` and carries the
/// Xcode entry and the default; a turn stored with `xcode` before `macos` is still ordered
/// `macos` first.
///
/// Catches: a run that drops `xcode` from a Mac that also has macOS due, or an order whose kinds
/// follow the run's stored order, so the daemon would install an Xcode before the macOS it needs.
#[test]
fn a_run_of_macos_and_xcode_orders_macos_first_and_carries_both() {
    let farm = Farm::new("mx-both", 1, None, &[MAC]);
    let newest = farm.add("27.2", "27B52");
    let minor = agent::DueItem {
        kind: MaintenanceKind::Macos as i32,
        from: "26.1".to_owned(),
        to: "26.2".to_owned(),
        label: "macOS Tahoe 26.2-25C56".to_owned(),
        ..agent::DueItem::default()
    };
    let heartbeat = || beat_on(Platform::DarwinArm64, &[], std::slice::from_ref(&minor));
    tell(&farm.world, MAC, heartbeat());
    farm.schedule(&[]).expect("scheduled");
    assert_eq!(farm.turn().0, vec![Kind::Macos, Kind::Xcode]);
    edit_runs(&farm.world, |runs| {
        runs[0].hosts[0].kinds = vec![Kind::Xcode, Kind::Macos];
    });

    let (maintain, wire) = farm.maintain(heartbeat()).expect("ordered");
    assert_eq!(
        maintain.kinds,
        vec![MaintenanceKind::Macos, MaintenanceKind::Xcode]
    );
    assert_eq!(
        maintain.xcode,
        Some(XcodeOrder {
            entries: vec![entry_of(&newest)],
            remove: Vec::new(),
            default: Some("27.2".to_owned()),
        })
    );
    let Some(agent::order::Verb::Maintain(said)) = wire.verb else {
        panic!("a maintain: {wire:?}")
    };
    assert_eq!(
        said.kinds,
        vec![MaintenanceKind::Macos as i32, MaintenanceKind::Xcode as i32]
    );
}

/// Proves: a Mac's turn of `xcode` scheduled while its group kept one Xcode is ordered with no
/// Xcode entry, removal or default once the group's `xcode_keep` is 0.
///
/// Catches: an order that reads what the run stored rather than the group as it is now, which
/// would change the Xcodes of a group that has since chosen to leave them alone.
#[test]
fn a_group_that_stops_keeping_xcode_orders_none() {
    let farm = Farm::new("mx-stop", 1, None, &[MAC]);
    farm.add("27.2", "27B52");
    tell(&farm.world, MAC, mac_beat(&[]));
    farm.schedule(&[MaintenanceKind::Xcode]).expect("scheduled");

    farm.settings(0, None);
    let (maintain, wire) = farm.maintain(mac_beat(&[])).expect("ordered");
    assert_eq!(maintain.xcode, None);
    let Some(agent::order::Verb::Maintain(said)) = wire.verb else {
        panic!("a maintain: {wire:?}")
    };
    assert_eq!(said.xcodes, Vec::new());
    assert_eq!(said.xcode_remove, Vec::<String>::new());
    assert_eq!(said.xcode_default, "");
}

/// Proves: while the Xcode store's directory cannot be read, a Mac's waiting turn with `xcode` is
/// sent no order and stays waiting, and `Hosts.Get` shows no Xcode item; once the directory is
/// back, the next heartbeat starts the turn with the store's entry.
///
/// Catches: an order sent with no entry because the store could not be listed, which would start
/// a turn that installs nothing and drains the Mac for it.
#[test]
fn a_turn_with_xcode_waits_while_the_store_cannot_be_read() {
    let farm = Farm::new("mx-unread", 1, None, &[MAC]);
    let newest = farm.add("27.2", "27B52");
    tell(&farm.world, MAC, mac_beat(&[]));
    farm.schedule(&[MaintenanceKind::Xcode]).expect("scheduled");

    let away = farm.dir.with_file_name("xcode-away");
    fs::rename(&farm.dir, &away).expect("moved away");
    assert_eq!(farm.maintain(mac_beat(&[])), None);
    assert_eq!(farm.turn(), (vec![Kind::Xcode], Step::Waiting));
    assert_eq!(farm.shown(MAC), Vec::new());
    assert_eq!(farm.updates_due(), Vec::<String>::new());

    fs::rename(&away, &farm.dir).expect("moved back");
    let (maintain, _) = farm.maintain(mac_beat(&[])).expect("ordered");
    assert_eq!(
        maintain.xcode.map(|xcode| xcode.entries),
        Some(vec![entry_of(&newest)])
    );
    assert_eq!(farm.turn(), (vec![Kind::Xcode], Step::Staging));
}

/// Proves: a Mac whose report already lists the 12 items a report holds is shown its Xcode item
/// as one more left out (`due_left_out`), the 12 listed unchanged.
///
/// Catches: a shown report that breaks the bound of 12 listed items its readers rely on.
#[test]
fn an_xcode_item_past_the_listed_bound_is_counted_left_out() {
    let farm = Farm::new("mx-bound", 1, None, &[MAC]);
    farm.add("27.2", "27B52");
    let due: Vec<agent::DueItem> = (0..12)
        .map(|n| agent::DueItem {
            kind: MaintenanceKind::Macos as i32,
            label: format!("item-{n}"),
            ..agent::DueItem::default()
        })
        .collect();
    tell(&farm.world, MAC, beat_on(Platform::DarwinArm64, &[], &due));
    let report = farm.report(MAC);
    assert_eq!(report.due, due);
    assert_eq!(report.due_left_out, 1);
}

/// Proves: of five releases held, a group keeping two keeps the two newest (whatever order the
/// store lists them in); a Mac with the newest installed but the second missing is due to the
/// second; a kept release installed only in a foreign directory, or in its own directory with
/// another build, is not installed (so due), and neither is removed; installed versions older
/// than every kept release go, oldest first, each once.
///
/// Catches: a keep that takes the oldest archives or the store's listing order; a foreign
/// Xcode counted as the release (the daemon installs its own beside it); a removal of a kept
/// version because its build differs.
#[test]
fn the_plan_keeps_the_newest_and_removes_only_older_own_directories() {
    let archives = [
        archive("26.4", "26E50", 'a'),
        archive("27.2", "27B52", 'b'),
        archive("25.0", "25A00", 'c'),
        archive("27.1", "27A10", 'd'),
        archive("26.0", "26A00", 'e'),
    ];
    let table = keeping(2, XcodeDefault::Latest);

    let missing_second = plan(
        &table,
        &archives,
        &[installed("27.2", "27B52", true, false)],
    );
    assert_eq!(
        missing_second.kept,
        vec![archives[1].clone(), archives[3].clone()]
    );
    assert_eq!(missing_second.default, Some(archives[1].clone()));
    assert_eq!(missing_second.due, Some(due_item("27.2", "27.1", "27A10")));

    let foreign = plan(
        &table,
        &archives,
        &[
            installed("27.2", "27B52", true, true),
            installed("27.1", "27A11", false, false),
            installed("26.4", "26E50", false, false),
            installed("25.0", "25A00", false, false),
            installed("26.4", "26E50", false, false),
            installed("26.0", "26A00", false, true),
        ],
    );
    assert_eq!(foreign.due, Some(due_item("27.2", "27.2", "27B52")));
    assert_eq!(foreign.remove, vec!["25.0".to_owned(), "26.4".to_owned()]);
}

/// Proves: a group keeping more releases than the store holds keeps every one held; a group
/// keeping none, or a store holding none, plans nothing (no entry, no removal, no default, nothing
/// due), so its order carries no Xcode.
///
/// Catches: a plan of a group that leaves Xcode alone that still removes or switches an Xcode.
#[test]
fn a_plan_of_no_kept_release_is_empty() {
    let archives = [archive("27.1", "27A10", 'a'), archive("27.2", "27B52", 'b')];
    let old = [installed("20.0", "20A00", true, false)];
    let all = plan(&keeping(4, XcodeDefault::Latest), &archives, &old);
    assert_eq!(all.kept, vec![archives[1].clone(), archives[0].clone()]);
    assert_eq!(all.remove, vec!["20.0".to_owned()]);

    let none = super::Plan::default();
    assert_eq!(
        plan(&keeping(0, XcodeDefault::Latest), &archives, &old),
        none
    );
    assert_eq!(plan(&keeping(2, XcodeDefault::Latest), &[], &old), none);
    assert_eq!(none.order(), None);
}

/// Proves: a Mac's turn with `xcode` whose order it has not reported yet is sent the same order
/// again at each heartbeat; while the store cannot be read it is sent nothing (the turn stays
/// staging), and once the store is back it is sent the order again with the store's entry.
///
/// Catches: a resend made while the store cannot be listed, which would name no Xcode under the
/// order id of one that did.
#[test]
fn a_started_turn_is_not_sent_its_order_again_while_the_store_cannot_be_read() {
    let farm = Farm::new("mx-resend", 1, None, &[MAC]);
    let newest = farm.add("27.2", "27B52");
    tell(&farm.world, MAC, mac_beat(&[]));
    farm.schedule(&[MaintenanceKind::Xcode]).expect("scheduled");
    farm.maintain(mac_beat(&[])).expect("started");

    let away = farm.dir.with_file_name("xcode-away");
    fs::rename(&farm.dir, &away).expect("moved away");
    assert_eq!(farm.maintain(mac_beat(&[])), None);
    assert_eq!(farm.turn(), (vec![Kind::Xcode], Step::Staging));

    fs::rename(&away, &farm.dir).expect("moved back");
    let (maintain, _) = farm.maintain(mac_beat(&[])).expect("sent again");
    assert_eq!(
        maintain.xcode.map(|xcode| xcode.entries),
        Some(vec![entry_of(&newest)])
    );
}

/// Proves: an operator's Put back queued on a Mac's halted turn with `xcode` is sent while the
/// store cannot be read: action `PUT_BACK`, the try it was queued under, and no Xcode.
///
/// Catches: a store fault that keeps a drained, halted Mac out of service, since a Put back needs
/// nothing of the store.
#[test]
fn a_put_back_of_a_halted_mac_is_sent_while_the_store_cannot_be_read() {
    let farm = Farm::new("mx-putback", 1, None, &[MAC]);
    farm.add("27.2", "27B52");
    tell(&farm.world, MAC, mac_beat(&[]));
    farm.schedule(&[MaintenanceKind::Xcode]).expect("scheduled");
    edit_runs(&farm.world, |runs| {
        let host = &mut runs[0].hosts[0];
        host.step = Step::Halted;
        host.queued = Some(Act::PutBack);
        host.order_try = 2;
    });

    fs::rename(&farm.dir, farm.dir.with_file_name("xcode-away")).expect("moved away");
    let (maintain, wire) = farm.maintain(mac_beat(&[])).expect("sent");
    assert_eq!(maintain.action, agent::MaintainAction::PutBack);
    assert_eq!(maintain.xcode, None);
    assert_eq!(wire.r#try, 2);
}

/// Proves: a turn that holds `xcode` on a host that is not a Mac (here, stored by hand on a Linux
/// host of a group that keeps Xcode, the store holding a release) is ordered with no Xcode.
///
/// Catches: an order that would send Xcode entries to a Linux host, which never reports any.
#[test]
fn a_turn_with_xcode_on_a_linux_host_orders_no_xcode() {
    let farm = Farm::new("mx-linux-turn", 1, None, &[LINUX_HOST]);
    farm.add("27.2", "27B52");
    let linux = agent::DueItem {
        kind: MaintenanceKind::Linux as i32,
        label: "openssl".to_owned(),
        security: true,
        ..agent::DueItem::default()
    };
    let heartbeat = || beat_on(Platform::LinuxX86_64, &[], std::slice::from_ref(&linux));
    tell(&farm.world, LINUX_HOST, heartbeat());
    let mut asked = hosts_request(&[LINUX_HOST]);
    asked.take_last_host = true;
    schedule(&farm.world, asked).expect("scheduled");
    edit_runs(&farm.world, |runs| {
        runs[0].hosts[0].kinds = vec![Kind::Xcode, Kind::Linux];
    });

    let Some(Order::Maintain { maintain, .. }) = tell(&farm.world, LINUX_HOST, heartbeat()) else {
        panic!("a maintain")
    };
    assert_eq!(
        maintain.kinds,
        vec![MaintenanceKind::Xcode, MaintenanceKind::Linux]
    );
    assert_eq!(maintain.xcode, None);
}

/// Proves: the store keeps a release a Mac reports installed, however old. A group keeps one
/// Xcode, the store holds 27.1, 27.2 and 27.3, and the Mac reports 27.1 installed in its own
/// directory: the coordinator's installed set is exactly 27.1 (27A10), and a prune leaves exactly
/// 27.3 and 27.1 in the store and audits the removal of 27.2 alone. A Mac that reports 27.2 in
/// `Xcode.app` (foreign) keeps it too, as the design keeps any release a Mac reports.
///
/// Catches: a coordinator whose installed set stays empty though its Macs report their Xcodes, so
/// the prune removes from the store the release a Mac is running once it falls out of the newest
/// `xcode_keep`.
#[test]
fn the_store_keeps_each_release_a_mac_reports_installed() {
    let farm = Farm::new("mx-installed", 1, None, &[MAC]);
    let oldest = farm.add("27.1", "27A10");
    let middle = farm.add("27.2", "27B52");
    let newest = farm.add("27.3", "27C30");
    tell(
        &farm.world,
        MAC,
        mac_beat(&[installed("27.1", "27A10", true, false)]),
    );
    let coordinator = farm.world.coordinator();
    assert_eq!(
        coordinator.xcodes_installed(),
        std::collections::BTreeSet::from([xcode_id("27.1", "27A10")])
    );
    coordinator.prune_xcode_store();
    let held = |farm: &Farm| {
        let mut held: Vec<Archive> = farm.store.archives().expect("listed");
        held.sort_by(|a, b| b.id.cmp(&a.id));
        held
    };
    assert_eq!(held(&farm), vec![newest.clone(), oldest.clone()]);
    let removed: Vec<Option<String>> = farm
        .world
        .audit()
        .into_iter()
        .filter(|entry| entry.action == crate::audit_actions::XCODE_REMOVE)
        .map(|entry| entry.reason)
        .collect();
    assert_eq!(
        removed,
        vec![Some(format!(
            "pruned: Xcode 27.2 (27B52), sha256 {}",
            middle.sha256
        ))]
    );

    let again = farm.add("27.2", "27B52");
    tell(
        &farm.world,
        MAC,
        mac_beat(&[
            installed("27.1", "27A10", false, false),
            installed("27.2", "27B52", true, true),
        ]),
    );
    farm.world.coordinator().prune_xcode_store();
    assert_eq!(held(&farm), vec![newest, again, oldest]);
}
