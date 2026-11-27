//! What a host of a group is ordered to install and to uninstall ([`crate::install_order`]): the
//! installs of the group's packages in the order of its list, a later install waiting while an
//! earlier package is not installed and can be, and the uninstalls of the packages a `Put` took
//! out, one slot of the group's `max_updating` at a time, a config package before its service.
//! The tests drive heartbeats of enrolled hosts placed in groups, over transports that carry
//! typed orders.

use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{DeferredReport, Health, Heartbeat, ServiceBeat, ServiceReply, Token};
use serde_json::json;

use crate::decide::{Gate, Skip, Why};
use crate::group_decision_support::{Farm, policy, stages};
use crate::index_set_support::{index_text, sha};
use crate::marks_support::{hold, runs, scope_host, update};
use crate::ops::{AuditEntry, Exclusion, Halt, HaltOrigin, Write};
use crate::orders::Order;
use crate::releases_tests::targets;
use crate::testutil::version;

const NOTHING: ServiceReply = ServiceReply::Nothing;

/// The wide policy: everything may go at once, in one stage.
fn wide() -> hostkeeper_proto::hostkeeper::operator::v1::RolloutPolicy {
    policy(|p| {
        p.max_updating = Some(5);
        p.live_rollouts = Some(4);
        p.stages = stages(&[100]);
    })
}

/// What an order is: its verb and the service.
fn verb(order: Option<Order>) -> Option<(&'static str, String)> {
    match order? {
        Order::Install { service, .. } => Some(("install", service)),
        Order::Uninstall { service, .. } => Some(("uninstall", service)),
        _ => Some(("other", String::new())),
    }
}

fn install(service: &str) -> Option<(&'static str, String)> {
    Some(("install", service.to_owned()))
}

fn uninstall(service: &str) -> Option<(&'static str, String)> {
    Some(("uninstall", service.to_owned()))
}

const INSTALL_ORDER: Option<Why> = Some(Why::Gate(Gate::InstallOrder));

/// Proves: a package a group lists that a host does not run is installed in the order of the
/// group's list: the first listed is ordered first and the next only once it is installed, and a
/// group that lists the same packages the other way round orders them the other way round.
///
/// Catches: installs ordered by package name or all at once, or an order of the rollout file.
#[tokio::test]
async fn a_package_added_to_a_group_is_installed_on_each_host_in_list_order() {
    let farm = Farm::new("io-list-order");
    farm.flag(4);
    farm.group("alpha", &["base", "app"], Some(wide()));
    farm.group("beta", &["app", "base"], Some(wide()));
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.host("b1", "beta");
    farm.deploy("", "base", "2.0", 1).await;
    farm.deploy("", "app", "2.0", 2).await;

    assert_eq!(verb(farm.order("a1", &[])), install("base"));
    assert_eq!(verb(farm.order("a1", &[])), install("base"));
    assert_eq!(verb(farm.order("a2", &[])), install("base"));
    assert_eq!(verb(farm.order("b1", &[])), install("app"));

    assert_eq!(verb(farm.order("a1", &[("base", "2.0")])), install("app"));
    assert_eq!(verb(farm.order("b1", &[("app", "2.0")])), install("base"));
    assert_eq!(
        verb(farm.order("a1", &[("base", "2.0"), ("app", "2.0")])),
        None
    );
}

/// Proves: a later package waits `install_order` while an earlier one is not installed on the
/// host, even when the earlier one's own plan keeps the host waiting for a later stage, and the
/// waiting host takes no place of the later package's plan: the host of the second stage is
/// ordered nothing of `app` while `base` has not reached it, then `base`, and `app` once `base`
/// is installed.
///
/// Catches: the later package's own plan ordering its install while the earlier one is still
/// waiting for its stage, which would put a config package on a host before its service.
#[tokio::test]
async fn a_later_package_waits_while_an_earlier_one_is_not_installed_even_at_a_later_stage() {
    let farm = Farm::new("io-later-stage");
    farm.group(
        "alpha",
        &["base", "app"],
        Some(policy(|p| {
            p.max_updating = Some(5);
            p.live_rollouts = Some(2);
            p.stages = stages(&[50, 100]);
        })),
    );
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.deploy("", "base", "2.0", 1).await;
    farm.deploy("", "app", "1.0", 2).await;
    let both = [("base", "1.0"), ("app", "1.0")];

    assert_eq!(farm.tell("a1", &both), update("base", "2.0"));

    assert_eq!(verb(farm.order("a2", &[])), None);
    assert_eq!(farm.why("a2", "base"), Some(Why::Gate(Gate::StageWait)));
    assert_eq!(farm.why("a2", "app"), INSTALL_ORDER);
    assert_eq!(verb(farm.order("a2", &[])), None);

    assert_eq!(farm.tell("a1", &[("base", "2.0"), ("app", "1.0")]), NOTHING);
    assert_eq!(verb(farm.order("a2", &[])), install("base"));
    assert_eq!(verb(farm.order("a2", &[("base", "2.0")])), install("app"));
}

/// Proves: a package that cannot be installed on the host does not hold the later ones back: one
/// with no desired version, one registered by a release index alone (no release kept, so no entry
/// is picked for any host) and one whose targets list none for it each say so for the host
/// (`no_desired`, `no_build_for_host`, `no_target`) and the package listed after them is ordered, as
/// the first host of the group is, and told only that the group's slot is taken to the second.
///
/// Catches: a package with nothing to install holding every package after it back for good.
#[tokio::test]
async fn a_package_with_nothing_to_install_does_not_block_the_later_ones() {
    let farm = Farm::new("io-skipped");
    farm.group(
        "alpha",
        &["ghost", "plat", "tgt", "app"],
        Some(policy(|p| {
            p.stages = stages(&[100]);
            p.max_updating = Some(1);
        })),
    );
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    let text = index_text("plat", "2.0", &[("linux-x86_64", 1)]);
    let body = json!({
        "service": "plat",
        "version": "2.0",
        "release_index": text,
        "release_index_digest": sha(&text),
    });
    farm.set("", &body, None).await;
    farm.set(
        "",
        &json!({"service": "tgt", "version": "2.0"}),
        Some(targets("tgt")),
    )
    .await;
    farm.deploy("", "app", "2.0", 3).await;
    let arm = || {
        let mut beat = farm.beat_with(&[]);
        beat.platform = Platform::LinuxArm64;
        beat
    };

    assert_eq!(verb(farm.order_on("a1", arm())), install("app"));
    assert_eq!(verb(farm.order_on("a2", arm())), None);

    assert_eq!(farm.why("a2", "ghost"), Some(Why::Skip(Skip::NoDesired)));
    assert_eq!(
        farm.why("a2", "plat"),
        Some(Why::Skip(Skip::NoBuildForHost))
    );
    assert_eq!(farm.why("a2", "tgt"), Some(Why::Skip(Skip::NoTarget)));
    assert_eq!(farm.why("a2", "app"), Some(Why::Gate(Gate::NoSlot)));
}

/// Proves: an earlier package that is halted in the group, or that an operator excluded the host
/// from, holds the later installs back: the host is told `install_order` for the later package
/// and nothing for the earlier one.
///
/// Catches: a package the host cannot be ordered for a reason of its own (a halt, an exclusion)
/// read as one that cannot be installed, which would put the later package on first.
#[tokio::test]
async fn an_earlier_package_that_is_halted_or_excluded_holds_the_later_ones_back() {
    let farm = Farm::new("io-halted");
    farm.group("alpha", &["base", "app"], Some(wide()));
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.deploy("", "base", "2.0", 1).await;
    farm.deploy("", "app", "2.0", 2).await;
    farm.world.seed_marks("alpha", "base", |marks| {
        marks.halt = Some(Halt {
            node_id: "a1".to_owned(),
            check: "artifact_changed".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Updater,
        });
    });

    assert_eq!(verb(farm.order("a1", &[])), None);
    assert_eq!(farm.why("a1", "base"), Some(Why::Gate(Gate::Halted)));
    assert_eq!(farm.why("a1", "app"), INSTALL_ORDER);

    farm.world
        .seed_marks("alpha", "base", |marks| marks.halt = None);
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "exclude"),
            |state| {
                state.excluded.push(Exclusion {
                    service: "base".to_owned(),
                    node_id: "a2".to_owned(),
                    reason: "for the test".to_owned(),
                });
            },
        )
        .expect("excluded");
    assert_eq!(verb(farm.order("a2", &[])), None);
    assert_eq!(farm.why("a2", "base"), Some(Why::Skip(Skip::NotAMember)));
    assert_eq!(farm.why("a2", "app"), INSTALL_ORDER);
    assert_eq!(verb(farm.order("a1", &[])), install("base"));
}

/// Excludes the host `host` from `package`, as `hosts exclude` does.
fn exclude(farm: &Farm, package: &str, host: &str) {
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "exclude"),
            |state| {
                state.excluded.push(Exclusion {
                    service: package.to_owned(),
                    node_id: host.to_owned(),
                    reason: "for the test".to_owned(),
                });
            },
        )
        .expect("excluded");
}

/// Proves: an earlier package that an operator excluded the host from and that the host runs, at
/// any version, counts as installed: with hostkeeper wanted at a version the hosts do not run and
/// each host excluded from hostkeeper, the package listed after it is ordered to a host whose
/// beat reports hostkeeper running (`a1`, and `a3` at first) and not told `install_order`; to a
/// host whose beat reports hostkeeper absent (`a2`), or whose last beat no longer lists it (`a3`
/// then), it is not, and waits `install_order`.
///
/// Catches: the beat of a package a host is excluded from, which the fleet keeps apart from the
/// package's counted hosts, read as no word of it, which holds every later package of the host's
/// group back for as long as the exclusion stands.
#[tokio::test]
async fn an_earlier_package_the_host_runs_and_is_excluded_from_does_not_hold_the_later_ones_back() {
    let farm = Farm::new("io-excluded-runs");
    farm.flag(4);
    farm.group("alpha", &["app"], Some(wide()));
    for host in ["a1", "a2", "a3"] {
        farm.host(host, "alpha");
    }
    farm.deploy("", "hostkeeper", "2.0", 1).await;
    farm.deploy("", "app", "2.0", 2).await;
    for host in ["a1", "a2", "a3"] {
        exclude(&farm, "hostkeeper", host);
    }

    assert_eq!(verb(farm.order("a1", &[])), install("app"));
    assert_ne!(farm.why("a1", "app"), INSTALL_ORDER);

    let mut absent = farm.beat_with(&[]);
    absent.services.as_mut().expect("services")[0].health = Health::Absent;
    assert_eq!(verb(farm.order_on("a2", absent)), None);
    assert_eq!(farm.why("a2", "app"), INSTALL_ORDER);

    assert_eq!(verb(farm.order("a3", &[])), install("app"));
    let mut unlisted = farm.beat_with(&[]);
    unlisted.services.as_mut().expect("services").clear();
    assert_eq!(verb(farm.order_on("a3", unlisted)), None);
    assert_eq!(farm.why("a3", "app"), INSTALL_ORDER);
}

/// What a host reports of `package` it was ordered to install at `target` and has not run: absent
/// at `target`, with the deferral of `target` its daemon reports while its wait lasts when
/// `declined`, as a service whose first install cannot go on without its config package's files
/// does (`join_token_missing`).
fn absent(package: &str, target: &str, declined: bool) -> ServiceBeat {
    let mut one = runs(package, target);
    one.health = Health::Absent;
    one.deferred = declined.then(|| DeferredReport {
        target: version(target),
        reason: Token::parse("join_token_missing").expect("a token"),
        retry_after_secs: 300,
    });
    one
}

/// The heartbeat of a daemon that takes installs and lists hostkeeper and `running`, each healthy
/// at the version given, and `other` as it is given.
fn beat_and(farm: &Farm, running: &[(&str, &str)], other: ServiceBeat) -> Heartbeat {
    let mut beat = farm.beat_with(running);
    beat.services.as_mut().expect("services").push(other);
    beat
}

/// Proves: a group that lists a service then its config package orders the config package on a
/// host where the service's install was ordered and the host declined it: while the host waits
/// out its deferral of the service (`deferred`), and while the config package's own order keeps
/// the host busy for the service (`busy_elsewhere`), the config package is not held
/// `install_order`; once its wait is over the service is ordered again, and the host that now has
/// its config package runs it.
///
/// Catches: a service its host reports absent, because it cannot start without its config
/// package, holding that config package back for good, which keeps the service's install
/// ordered again and again and its rollout live.
#[tokio::test]
async fn a_config_package_is_installed_where_its_service_declined_its_install() {
    let farm = Farm::new("io-declined");
    farm.flag(4);
    farm.group("alpha", &["svc", "cfg"], Some(wide()));
    farm.host("a1", "alpha");
    farm.deploy("", "svc", "2.0", 1).await;
    farm.deploy("", "cfg", "2.0", 2).await;
    assert_eq!(verb(farm.order("a1", &[])), install("svc"));

    let declined = || beat_and(&farm, &[], absent("svc", "2.0", true));
    assert_eq!(verb(farm.order_on("a1", declined())), install("cfg"));
    assert_eq!(farm.why("a1", "svc"), Some(Why::Gate(Gate::BusyElsewhere)));
    assert_ne!(farm.why("a1", "cfg"), INSTALL_ORDER);

    let configured = || beat_and(&farm, &[("cfg", "2.0")], absent("svc", "2.0", true));
    assert_eq!(verb(farm.order_on("a1", configured())), None);
    assert_eq!(farm.why("a1", "svc"), Some(Why::Skip(Skip::Deferred)));

    farm.world.rig.at(2_301);
    let waited = beat_and(&farm, &[("cfg", "2.0")], absent("svc", "2.0", false));
    assert_eq!(verb(farm.order_on("a1", waited)), install("svc"));
    assert_eq!(
        verb(farm.order("a1", &[("svc", "2.0"), ("cfg", "2.0")])),
        None
    );
}

/// Proves: an earlier package the host reports absent holds the later ones back unless the host
/// declined its install at its desired version and nothing else holds it: one absent with no
/// deferral (`b1`), one the host declined and an operator holds on the host (`b2`), and one the
/// host declined at a version the group no longer wants (`b3`, ordered the new version) each
/// leave the later package told `install_order` (on `b1` and `b3` once the install order of the
/// service just given no longer keeps the host busy).
///
/// Catches: every absent package counted as installed, which would order a config package to a
/// host before its service's install was ever tried; a hold of the service read past; and a
/// deferral of an older version taken for one of the version wanted.
#[tokio::test]
async fn an_absent_package_not_declined_at_its_desired_version_holds_the_later_ones_back() {
    let farm = Farm::new("io-not-declined");
    farm.flag(4);
    farm.group("alpha", &["svc", "cfg"], Some(wide()));
    for host in ["b1", "b2", "b3"] {
        farm.host(host, "alpha");
    }
    farm.deploy("", "svc", "2.0", 1).await;
    farm.deploy("", "cfg", "2.0", 2).await;

    let never_run = beat_and(&farm, &[], absent("svc", "2.0", false));
    assert_eq!(verb(farm.order_on("b1", never_run)), install("svc"));

    assert_eq!(verb(farm.order("b2", &[])), install("svc"));
    hold(&farm.world.rig, "svc", scope_host("b2")).expect("held");
    let declined = beat_and(&farm, &[], absent("svc", "2.0", true));
    assert_eq!(verb(farm.order_on("b2", declined)), None);
    assert_eq!(farm.why("b2", "svc"), Some(Why::Gate(Gate::Held)));
    assert_eq!(farm.why("b2", "cfg"), INSTALL_ORDER);

    assert_eq!(verb(farm.order("b3", &[])), install("svc"));
    farm.deploy("", "svc", "3.0", 3).await;
    let declined_older = beat_and(&farm, &[], absent("svc", "2.0", true));
    assert_eq!(verb(farm.order_on("b3", declined_older)), install("svc"));

    // The install orders of `svc` given to `b1` and `b3` keep each host busy for `cfg` until they
    // are older than an order counts for.
    farm.world.rig.at(2_000 + crate::fleet::ORDER_AGE.as_secs());
    assert_eq!(farm.why("b1", "cfg"), INSTALL_ORDER);
    assert_eq!(farm.why("b3", "cfg"), INSTALL_ORDER);
}

/// Proves: a package taken out of a group is uninstalled from each host that ran it paced by the
/// group's `max_updating`: with a slot of one the second host is told `no_slot` while the first
/// uninstalls and is ordered the uninstall once the first has finished; with two slots both are
/// ordered at once; and a group that took out nothing orders nothing.
///
/// Catches: an uninstall that holds no slot, which would take a whole group's service out at
/// once.
#[test]
fn an_uninstall_takes_a_slot_of_the_groups_max_updating() {
    let farm = Farm::new("io-uninstall-slot");
    let one = || Some(policy(|p| p.max_updating = Some(1)));
    farm.group("one", &["cfg"], one());
    let two = || Some(policy(|p| p.max_updating = Some(2)));
    farm.group("two", &["cfg"], two());
    farm.group("kept", &["cfg"], None);
    for (host, group) in [
        ("o1", "one"),
        ("o2", "one"),
        ("t1", "two"),
        ("t2", "two"),
        ("k1", "kept"),
    ] {
        farm.host(host, group);
        assert_eq!(verb(farm.order(host, &[("cfg", "1.0")])), None);
    }
    farm.world
        .put_with("one", &["hostkeeper"], one(), Some(1))
        .expect("put");
    farm.world
        .put_with("two", &["hostkeeper"], two(), Some(1))
        .expect("put");

    assert_eq!(verb(farm.order("o1", &[("cfg", "1.0")])), uninstall("cfg"));
    assert_eq!(verb(farm.order("o2", &[("cfg", "1.0")])), None);
    assert_eq!(farm.why("o2", "cfg"), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(verb(farm.order("t1", &[("cfg", "1.0")])), uninstall("cfg"));
    assert_eq!(verb(farm.order("t2", &[("cfg", "1.0")])), uninstall("cfg"));
    assert_eq!(verb(farm.order("k1", &[("cfg", "1.0")])), None);

    assert_eq!(verb(farm.order("o1", &[])), None);
    assert_eq!(verb(farm.order("o2", &[("cfg", "1.0")])), uninstall("cfg"));
}

/// Proves: a service and its config package taken out of a group together are uninstalled the
/// config package first, the reverse of the list, and the next one only once the one before is
/// gone from the host.
///
/// Catches: a config package left on a host after its service is gone.
#[test]
fn a_config_package_is_uninstalled_before_its_service() {
    let farm = Farm::new("io-config-first");
    farm.group("alpha", &["svc", "cfg"], Some(wide()));
    farm.host("a1", "alpha");
    let running = [("svc", "1.0"), ("cfg", "1.0")];
    assert_eq!(verb(farm.order("a1", &running)), None);
    farm.world
        .put("alpha", &["hostkeeper"], Some(1))
        .expect("put");

    assert_eq!(verb(farm.order("a1", &running)), uninstall("cfg"));
    assert_eq!(verb(farm.order("a1", &[("svc", "1.0")])), uninstall("svc"));
    assert_eq!(verb(farm.order("a1", &[])), None);
    assert_eq!(farm.world.intents(), []);
}

/// Proves: a `Put` that lists a package again before its uninstall is ordered cancels the
/// uninstall: no intent stands and the host is ordered nothing of the package.
///
/// Catches: an uninstall that outlives the package being listed again.
#[test]
fn listing_a_package_again_cancels_its_uninstall() {
    let farm = Farm::new("io-relist");
    farm.group("alpha", &["svc", "cfg"], Some(wide()));
    farm.host("a1", "alpha");
    let running = [("svc", "1.0"), ("cfg", "1.0")];
    assert_eq!(verb(farm.order("a1", &running)), None);
    farm.world
        .put("alpha", &["hostkeeper", "svc"], Some(1))
        .expect("put");
    assert_eq!(farm.world.intents().len(), 1);

    farm.world
        .put("alpha", &["hostkeeper", "svc", "cfg"], Some(2))
        .expect("put");

    assert_eq!(farm.world.intents(), []);
    assert_eq!(verb(farm.order("a1", &running)), None);
    assert_eq!(farm.why("a1", "cfg"), Some(Why::Skip(Skip::NoDesired)));
}

/// Proves: a service's uninstall waits for its config package's on each host, whichever gate holds
/// the config package's: with a slot of one, the second host is told `no_slot` for the config
/// package and `uninstall_order` for the service, whose own slot is free, and the service goes
/// once the config package is gone from the host. The uninstalls of the two packages on the
/// first host are live rollouts together, as the group's `live_rollouts` of 4 lets them be.
///
/// Catches: a service taken off a host before its config package, because the config package's
/// uninstall waited for a slot the service's did not need; and a package taken out of a group
/// held by the coordinator's `--live-rollouts` instead of the group's.
#[test]
fn a_services_uninstall_waits_for_its_config_packages_while_the_config_package_is_gated() {
    let farm = Farm::new("io-config-first-gated");
    let narrow = || {
        Some(policy(|p| {
            p.max_updating = Some(1);
            p.live_rollouts = Some(4);
            p.stages = stages(&[100]);
        }))
    };
    farm.group("alpha", &["svc", "cfg"], narrow());
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    let running = [("svc", "1.0"), ("cfg", "1.0")];
    assert_eq!(verb(farm.order("a1", &running)), None);
    assert_eq!(verb(farm.order("a2", &running)), None);
    farm.world
        .put_with("alpha", &["hostkeeper"], narrow(), Some(1))
        .expect("put");

    assert_eq!(verb(farm.order("a1", &running)), uninstall("cfg"));
    assert_eq!(verb(farm.order("a2", &running)), None);
    assert_eq!(farm.why("a2", "cfg"), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(farm.why("a2", "svc"), Some(Why::Gate(Gate::UninstallOrder)));

    assert_eq!(verb(farm.order("a1", &[("svc", "1.0")])), uninstall("svc"));
    assert_eq!(verb(farm.order("a2", &running)), uninstall("cfg"));
    assert_eq!(verb(farm.order("a2", &[("svc", "1.0")])), None);
    assert_eq!(farm.why("a2", "svc"), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(verb(farm.order("a1", &[])), None);
    assert_eq!(verb(farm.order("a2", &[("svc", "1.0")])), uninstall("svc"));
}
