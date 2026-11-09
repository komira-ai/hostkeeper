//! What each host runs, as `Hosts.List` and `Hosts.Get` answer it (`Host.running`): the services
//! of a heartbeat with the installer of each, for a host that heartbeats and, from what the host's
//! enrollment kept, for one that has not since the coordinator started; and what the enrollment
//! store is written (packages-native.md section 10.6, build item A18).

use std::sync::Arc;

use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat};

use crate::enrollment::os_random;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::first_serving_tests::{APPROVED, FIRST, Fixture, HK, WEB, daemon};
use crate::marks_support::{ask, operator, runs, who};
use crate::picks::TARGET_FEATURE;
use crate::testutil::{Log, Rig, beat, id};

const ARCHIVE: &str = "hostkeeper-archive";
const DB: &str = "db";

/// A heartbeat of the deployed daemon that also announces `target-v1`, listing `services`.
fn target_daemon(services: Vec<ServiceBeat>) -> Heartbeat {
    let mut b = daemon(services);
    b.agent_features.push(TARGET_FEATURE.to_owned());
    b
}

/// `service` at `version`, reported absent.
fn absent(service: &str, version: &str) -> ServiceBeat {
    ServiceBeat {
        health: Health::Absent,
        ..runs(service, version)
    }
}

/// A heartbeat of `host` at `at` by the enrollment's clock from a daemon that announces
/// `target-v1`, over the JSON route.
fn beat_target(f: &Fixture, host: &str, at: u64, services: Vec<ServiceBeat>) {
    f.enrollment.clock.set(at);
    ask(&f.rig, host, "storage", target_daemon(services));
}

/// What the daemon of `host` says over gRPC: it lists `deb` and the archive, and runs each of
/// `named` from the installer given.
fn say_over_grpc(f: &Fixture, host: &str, named: &[(&str, &str)]) {
    let request = agent::HeartbeatRequest {
        features: vec![TARGET_FEATURE.to_owned()],
        installers: vec![ARCHIVE.to_owned(), "deb".to_owned()],
        services: named
            .iter()
            .map(|(name, installer)| agent::ServiceBeat {
                name: (*name).to_owned(),
                installer: Some((*installer).to_owned()),
                ..agent::ServiceBeat::default()
            })
            .collect(),
        ..agent::HeartbeatRequest::default()
    };
    f.rig
        .coordinator
        .hear_installers(&who(host, "storage"), &request);
}

fn service(name: &str, version: &str, installer: &str) -> proto::RunningService {
    proto::RunningService {
        service: name.to_owned(),
        version: version.to_owned(),
        installer: installer.to_owned(),
    }
}

fn list(rig: &Rig, limit: u32, token: &str) -> proto::ListHostsResponse {
    rig.coordinator
        .list_hosts(
            &operator(),
            proto::ListHostsRequest {
                limit,
                page_token: token.to_owned(),
            },
        )
        .expect("a page")
}

/// The host `node_id` as `Hosts.List` lists it.
fn listed(rig: &Rig, node_id: &str) -> proto::Host {
    list(rig, 0, "")
        .hosts
        .into_iter()
        .find(|host| host.node_id == node_id)
        .unwrap_or_else(|| panic!("{node_id} is listed"))
}

/// The ids of a page and its next page token.
fn page(rig: &Rig, limit: u32, token: &str) -> (Vec<String>, String) {
    let listed = list(rig, limit, token);
    let ids = listed.hosts.into_iter().map(|host| host.node_id).collect();
    (ids, listed.next_page_token)
}

/// Proves: a host heartbeating over gRPC with `target-v1` lists each service it runs with the
/// version and the installer it named, `hostkeeper-archive` for one it named none for, by
/// service name; a service it reports absent and the agent's own service are left out; its
/// time is that of its heartbeat; and `Hosts.Get` answers the same.
///
/// Catches: an installer given to the wrong service, none named read as unknown rather than the
/// archive, an absent service listed as running, and `Get` answering without what `List` does.
#[test]
fn a_target_v1_daemon_over_grpc_lists_its_services_with_the_installers_it_named() {
    over_grpc(&Fixture::new("running-grpc"));
}

/// [`a_target_v1_daemon_over_grpc_lists_its_services_with_the_installers_it_named`], the
/// enrollment's records on Postgres.
#[test]
fn a_target_v1_daemon_over_grpc_lists_its_services_with_the_installers_it_named_on_postgres() {
    over_grpc(&Fixture::on_postgres("running-grpc-pg"));
}

fn over_grpc(f: &Fixture) {
    f.approve("m-1", "db-1", APPROVED, false);
    say_over_grpc(f, "db-1", &[(WEB, "deb")]);
    let services = vec![
        runs(WEB, "1.0"),
        runs(DB, "2.0"),
        absent("cache", "3.0"),
        runs(HK, "0.1.100"),
    ];
    beat_target(f, "db-1", FIRST, services);
    let host = listed(&f.rig, "db-1");
    let installed = vec![
        service(DB, "2.0", ARCHIVE),
        service(HK, "0.1.100", ARCHIVE),
        service(WEB, "1.0", "deb"),
    ];
    assert_eq!(host.running, installed);
    assert_eq!(host.running_unix, Some(host.last_heartbeat_unix));
    assert!(host.live);
    let got = f
        .rig
        .coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "db-1".to_owned(),
            },
        )
        .expect("answered")
        .host
        .expect("a host");
    assert_eq!(
        (got.running, got.running_unix),
        (installed, host.running_unix)
    );
}

/// Proves: a daemon that does not announce `target-v1` runs every service from archives,
/// whatever installers were said for its host, and a service the daemon names in its top level
/// (the legacy service) is listed beside the others.
///
/// Catches: an old daemon's services shown with no installer, or with one a gRPC heartbeat of
/// another daemon said, and the legacy service left out.
#[test]
fn an_old_daemons_services_all_run_from_the_archive_and_the_legacy_service_is_listed() {
    let f = Fixture::new("running-old");
    f.approve("m-1", "db-1", APPROVED, false);
    say_over_grpc(&f, "db-1", &[(WEB, "deb")]);
    f.beat(
        "db-1",
        "storage",
        FIRST,
        vec![runs(WEB, "1.0"), runs(DB, "2.0")],
    );
    assert_eq!(
        listed(&f.rig, "db-1").running,
        [service(DB, "2.0", ARCHIVE), service(WEB, "1.0", ARCHIVE)]
    );

    let rig = Rig::new("running-legacy");
    ask(&rig, "s1", "storage", beat("1.0"));
    assert_eq!(listed(&rig, "s1").running, [service("kbf", "1.0", ARCHIVE)]);
}

/// Proves: a `target-v1` host whose installers the coordinator has not read over gRPC shows
/// each installer empty (not known); a later heartbeat over JSON keeps the installer a heartbeat
/// over gRPC gave; and a daemon that announces `target-v1` after beating without it keeps the
/// installer of the service it ran, leaving a service new to the host empty.
///
/// Catches: an installer invented for a host whose installers are not known, one lost at the
/// next heartbeat over JSON, and one lost when the daemon starts to announce `target-v1`.
#[test]
fn a_target_v1_host_heard_only_over_json_shows_no_installer_and_keeps_one_it_was_given() {
    // A host the store never approved, so that only the coordinator's own record speaks.
    let f = Fixture::new("running-json");
    beat_target(&f, "web-7", FIRST, vec![runs(WEB, "1.0"), runs(DB, "2.0")]);
    assert_eq!(
        listed(&f.rig, "web-7").running,
        [service(DB, "2.0", ""), service(WEB, "1.0", "")],
        "no installer is known"
    );

    say_over_grpc(&f, "web-7", &[(WEB, "deb"), (DB, "deb")]);
    beat_target(
        &f,
        "web-7",
        FIRST + 15,
        vec![runs(WEB, "1.0"), runs(DB, "2.0")],
    );
    beat_target(
        &f,
        "web-7",
        FIRST + 30,
        vec![runs(WEB, "1.1"), runs(DB, "2.0")],
    );
    assert_eq!(
        listed(&f.rig, "web-7").running,
        [service(DB, "2.0", "deb"), service(WEB, "1.1", "deb")]
    );

    f.beat("web-8", "storage", FIRST, vec![runs(WEB, "1.0")]);
    beat_target(
        &f,
        "web-8",
        FIRST + 15,
        vec![runs(WEB, "1.0"), runs(DB, "2.0")],
    );
    assert_eq!(
        listed(&f.rig, "web-8").running,
        [service(DB, "2.0", ""), service(WEB, "1.0", ARCHIVE)]
    );
}

/// Proves: after a coordinator restart, an enrolled host that has not heartbeated is listed
/// with its role, not live, with no heartbeat time, its daemon's name and version, and the
/// services, versions and installers its enrollment kept with the time of the heartbeat that
/// last changed them; the host is still `unknown_host` to `Hosts.Get`; and its next heartbeat
/// over JSON, whose installers are not known, shows the installers the enrollment kept and
/// leaves them kept.
///
/// Catches: a host nothing was heard of since the start missing from the list (so the move's
/// counts would miss it), what the enrollment kept lost or shown for a live host, an installer
/// lost to a heartbeat that cannot tell it, and an unheard host given labels or services.
#[test]
fn an_enrolled_host_not_heard_since_the_restart_is_listed_with_what_it_ran() {
    after_restart(&mut Fixture::new("running-restart"));
}

/// [`an_enrolled_host_not_heard_since_the_restart_is_listed_with_what_it_ran`], the enrollment's
/// records on Postgres.
#[test]
fn an_enrolled_host_not_heard_since_the_restart_is_listed_with_what_it_ran_on_postgres() {
    after_restart(&mut Fixture::on_postgres("running-restart-pg"));
}

fn after_restart(f: &mut Fixture) {
    f.approve("m-1", "db-1", APPROVED, false);
    f.approve("m-2", "db-2", APPROVED, false);
    say_over_grpc(f, "db-1", &[(WEB, "deb")]);
    beat_target(f, "db-1", FIRST, vec![runs(WEB, "1.0")]);
    beat_target(
        f,
        "db-1",
        FIRST + 15,
        vec![runs(WEB, "1.1"), runs(DB, "2.0")],
    );
    beat_target(
        f,
        "db-1",
        FIRST + 30,
        vec![runs(WEB, "1.1"), runs(DB, "2.0")],
    );

    f.restart();
    let (ids, token) = page(&f.rig, 0, "");
    assert_eq!(
        (ids.as_slice(), token.as_str()),
        (&["db-1".to_owned(), "db-2".to_owned()][..], "")
    );
    let host = listed(&f.rig, "db-1");
    let want = proto::Host {
        node_id: "db-1".to_owned(),
        role: "storage".to_owned(),
        heartbeat: Some(agent::HeartbeatRequest {
            agent: Some(agent::AgentInfo {
                name: HK.to_owned(),
                version: "0.1.100".to_owned(),
            }),
            ..agent::HeartbeatRequest::default()
        }),
        running: vec![service(DB, "2.0", ARCHIVE), service(WEB, "1.1", "deb")],
        running_unix: Some(FIRST + 15),
        ..host.clone()
    };
    assert_eq!(host, want);
    assert!(!host.live);
    assert_eq!(host.last_heartbeat_unix, 0);
    assert_eq!(host.services, Vec::<proto::HostService>::new());
    assert_eq!(host.labels.len(), 0);
    assert_eq!(host.enrolled_unix, APPROVED);
    assert_eq!(host.first_heartbeat_unix, Some(FIRST));
    let silent = listed(&f.rig, "db-2");
    assert_eq!(
        (silent.heartbeat, silent.running, silent.running_unix),
        (None, Vec::new(), None),
        "nothing kept of a host that never heartbeated"
    );
    let get = f.rig.coordinator.get_host(
        &operator(),
        &proto::GetHostRequest {
            node_id: "db-1".to_owned(),
        },
    );
    assert_eq!(get, Err(proto::HostsRefusal::UnknownHost));

    f.enrollment.clock.set(FIRST + 200);
    ask(
        &f.rig,
        "db-1",
        "storage",
        target_daemon(vec![runs(WEB, "1.1"), runs(DB, "2.0")]),
    );
    let heard = listed(&f.rig, "db-1");
    assert!(heard.live);
    assert_ne!(
        heard.last_heartbeat_unix, 0,
        "a host heard since the start has a heartbeat time"
    );
    assert_eq!(
        heard.running,
        [service(DB, "2.0", ARCHIVE), service(WEB, "1.1", "deb")],
        "the installers the enrollment kept"
    );
    f.restart();
    assert_eq!(listed(&f.rig, "db-1").running_unix, Some(FIRST + 15));
    assert_eq!(
        listed(&f.rig, "db-1").running,
        [service(DB, "2.0", ARCHIVE), service(WEB, "1.1", "deb")]
    );
}

/// Proves: a host heard since the start whose record the coordinator holds no longer is listed
/// with what its enrollment kept.
///
/// Catches: a heard host listed with no services when only its enrollment knows them.
#[test]
fn a_heard_host_without_a_record_of_its_own_shows_what_its_enrollment_kept() {
    let f = Fixture::new("running-fallback");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(WEB, "1.0")]);
    f.rig.coordinator.running.forget(&id("db-1"));
    let host = listed(&f.rig, "db-1");
    assert!(host.live);
    assert_eq!(host.running, [service(WEB, "1.0", ARCHIVE)]);
    assert_eq!(host.running_unix, Some(FIRST));
    let farm = Rig::new("running-fallback-farm");
    ask(&farm, "s1", "storage", beat("1.0"));
    farm.coordinator.running.forget(&id("s1"));
    let host = listed(&farm, "s1");
    assert_eq!((host.running, host.running_unix), (Vec::new(), None));
}

/// Proves: the enrollment store is written when what a host runs changes (a version, an
/// installer, a service gained or lost, the daemon) and not for a repeated heartbeat, whose
/// time alone differs; an approval of the host clears what was kept, and the next heartbeat
/// writes it again; a host the store never approved (a farm host) is listed from the coordinator's
/// own record and writes nothing.
///
/// Catches: a write at every heartbeat (the time of the file moves), a change that is not
/// written, what a host ran before its re-approval kept, and a file made for a host never
/// approved.
#[test]
fn the_store_is_written_only_when_what_the_host_runs_changes() {
    let f = Fixture::new("running-writes");
    f.approve("m-1", "db-1", APPROVED, false);
    let file = f.enrollment.files.store.join("hosts/db-1.pb");
    f.beat("db-1", "storage", FIRST, vec![runs(WEB, "1.0")]);
    let first = std::fs::read(&file).expect("the host file");
    f.beat("db-1", "storage", FIRST + 15, vec![runs(WEB, "1.0")]);
    f.beat("db-1", "storage", FIRST + 30, vec![runs(WEB, "1.0")]);
    assert_eq!(std::fs::read(&file).expect("read"), first, "a repeat");
    let mut last = first;
    let changes = [
        vec![runs(WEB, "1.1")],
        vec![runs(WEB, "1.1"), runs(DB, "2.0")],
        vec![runs(DB, "2.0")],
    ];
    for (n, services) in changes.into_iter().enumerate() {
        f.beat("db-1", "storage", FIRST + 45 + 15 * n as u64, services);
        let now = std::fs::read(&file).expect("read");
        assert_ne!(now, last, "change {n}");
        last = now;
    }
    let mut agent_changed = daemon(vec![runs(DB, "2.0")]);
    agent_changed.agent = Some(hostkeeper_wire::wire::Agent {
        name: hostkeeper_wire::wire::AgentName::parse(HK).expect("a name"),
        version: crate::testutil::version("0.1.101"),
    });
    f.enrollment.clock.set(FIRST + 120);
    ask(&f.rig, "db-1", "storage", agent_changed);
    assert_ne!(std::fs::read(&file).expect("read"), last, "the daemon");

    f.approve("m-9", "db-1", FIRST + 500, true);
    assert_eq!(listed_kept(&f, "db-1"), None);
    f.beat("db-1", "storage", FIRST + 530, vec![runs(DB, "2.0")]);
    assert_eq!(listed_kept(&f, "db-1"), Some(FIRST + 530));

    f.beat("web-7", "storage", FIRST + 540, vec![runs(WEB, "1.0")]);
    assert!(!f.enrollment.files.store.join("hosts/web-7.pb").exists());
    assert_eq!(
        listed(&f.rig, "web-7").running,
        [service(WEB, "1.0", ARCHIVE)]
    );
}

/// What the enrollment store kept of `host`: when.
fn listed_kept(f: &Fixture, host: &str) -> Option<u64> {
    f.enrollment
        .enrollment
        .store()
        .enrolled_host(&hostkeeper_enrollment::names::HostId::parse(host).expect("a host id"))
        .expect("read")
        .expect("enrolled")
        .running_unix
}

/// Proves: when the store cannot record what a host runs, the heartbeat is still answered, the
/// failure is logged, the host is listed as it said, and the next heartbeat records it.
///
/// Catches: a store failure that refuses the heartbeat, goes unlogged, or marks the record as
/// kept so it is never written.
#[test]
fn what_the_store_cannot_write_is_recorded_by_the_next_heartbeat() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = EnrollmentRig::over("running-fault-enrollment", fs.clone(), os_random());
    let f = Fixture::over(enrollment, "running-fault");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(WEB, "1.0")]);
    let log = Log::default();
    let guard = log.install();
    let failed = (1..=12).find(|offset| {
        fs.fail_at(fs.count() + offset);
        f.beat("db-1", "storage", FIRST + 15, vec![runs(WEB, "1.1")]);
        log.text().contains("what a host runs was not recorded")
    });
    drop(guard);
    assert!(failed.is_some(), "{}", log.text());
    assert_eq!(listed_kept(&f, "db-1"), Some(FIRST), "not written");
    assert_eq!(
        listed(&f.rig, "db-1").running,
        [service(WEB, "1.1", ARCHIVE)],
        "listed as it said"
    );
    f.beat("db-1", "storage", FIRST + 30, vec![runs(WEB, "1.1")]);
    assert_eq!(listed_kept(&f, "db-1"), Some(FIRST + 30));
}

/// Proves: `Hosts.List` pages over the hosts heard and the hosts enrolled together, by node id:
/// a page may end between a host not heard and one heard, `limit` and the token behave as for
/// heard hosts alone, a host heard that is not enrolled is listed, and a host that is denied
/// is listed only while it is heard.
///
/// Catches: a page that skips or repeats a host at the boundary of the two kinds, a token that
/// names a host the next page lists again, a denied host listed from the store's rows, and a
/// retired host's record kept by the coordinator.
#[test]
fn the_list_pages_over_hosts_heard_and_hosts_enrolled() {
    let f = Fixture::new("running-pages");
    for (n, host) in ["db-1", "db-2", "db-3", "db-4"].iter().enumerate() {
        f.approve(&format!("m-{n}"), host, APPROVED, false);
    }
    f.beat("db-2", "storage", FIRST, vec![runs(WEB, "1.0")]);
    f.beat("web-7", "storage", FIRST, vec![runs(WEB, "1.0")]);
    f.beat("db-4", "storage", FIRST, vec![runs(WEB, "1.0")]);
    let ids = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        page(&f.rig, 2, ""),
        (ids(&["db-1", "db-2"]), "db-2".to_owned())
    );
    assert_eq!(
        page(&f.rig, 2, "db-2"),
        (ids(&["db-3", "db-4"]), "db-4".to_owned())
    );
    assert_eq!(page(&f.rig, 2, "db-4"), (ids(&["web-7"]), String::new()));
    assert_eq!(
        page(&f.rig, 0, "db-1"),
        (ids(&["db-2", "db-3", "db-4", "web-7"]), String::new())
    );
    assert_eq!(page(&f.rig, 3, "").0.len(), 3);
    assert_eq!(page(&f.rig, 0, "zz"), (Vec::new(), String::new()));

    f.rig
        .coordinator
        .retire_host(
            &operator(),
            &proto::RetireRequest {
                node_id: "db-1".to_owned(),
                reason: "gone".to_owned(),
            },
        )
        .expect("retired");
    f.rig
        .coordinator
        .retire_host(
            &operator(),
            &proto::RetireRequest {
                node_id: "db-4".to_owned(),
                reason: "gone".to_owned(),
            },
        )
        .expect("retired");
    assert_eq!(
        page(&f.rig, 0, "").0,
        ids(&["db-2", "db-3", "web-7"]),
        "denied hosts are not listed, heard or not"
    );
    assert_eq!(f.rig.coordinator.running.of(&id("db-4")), None);
}

/// Proves: the final report of a retiring host that frees its slot forgets what the coordinator
/// kept of what it runs.
///
/// Catches: the record of a released host kept for as long as the coordinator runs.
#[test]
fn the_final_report_of_a_retiring_host_forgets_what_it_ran() {
    let f = Fixture::new("running-final");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(WEB, "1.0")]);
    assert!(f.rig.coordinator.running.of(&id("db-1")).is_some());
    f.enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("db-1").expect("a host id"))
        .expect("denied");
    assert!(
        f.rig
            .coordinator
            .final_report(who("db-1", "storage"), daemon(vec![runs(WEB, "1.0")]))
    );
    assert_eq!(f.rig.coordinator.running.of(&id("db-1")), None);
}

/// Proves: a coordinator without enrollment lists only the hosts it heard, each with what it
/// said it runs.
///
/// Catches: a list of enrolled hosts read from a store that is not there.
#[test]
fn a_coordinator_without_enrollment_lists_the_hosts_it_heard() {
    let rig = Rig::new("running-no-enrollment");
    ask(&rig, "s2", "storage", beat("1.0"));
    ask(&rig, "s1", "storage", beat("1.0"));
    assert_eq!(
        page(&rig, 0, ""),
        (vec!["s1".to_owned(), "s2".to_owned()], String::new())
    );
}

/// Proves: a page entry that is not heard and that the enrollment holds nothing of (it was
/// denied after the ids were read) is left out of the page, with its enrollment.
///
/// Catches: a host denied during the call listed as an empty host.
#[test]
fn a_host_denied_while_the_page_was_made_is_not_listed() {
    let rig = Rig::new("running-denied-between");
    let page = vec![(id("db-1"), None)];
    let (hosts, kept) = rig.coordinator.page_hosts(page, vec![None]);
    assert_eq!((hosts, kept.len()), (Vec::new(), 0));
}
