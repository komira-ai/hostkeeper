//! The health a service's view of a host says once the host has gone silent: what the host last
//! reported while it is live, `HEALTH_SILENT` from the 45th second after its last heartbeat, in
//! `Releases.Status` and in `Hosts.List` and `Hosts.Get`, with the raw copy of its last heartbeat
//! left as the host sent it.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::ServiceBeat;
use serde_json::json;

use crate::grpc::report;
use crate::marks_support::{NEW_DAEMON, WEB, ask, beat_of, operator, rig, runs};
use crate::testutil::Rig;

/// The second the rig's hosts heartbeat at, past the coordinator's quiet start.
const BEAT_AT: u64 = 2_000;

/// What the host reports of `service`: version 1.0 and `health` (`unhealthy` or `absent`).
fn reports(service: &str, health: &str) -> ServiceBeat {
    serde_json::from_value(json!({"name": service, "version": "1.0", "health": health}))
        .expect("a service beat")
}

/// `web` on the host `d1`, which reports it as `beat` says at [`BEAT_AT`].
fn heard(test: &str, beat: ServiceBeat) -> Rig {
    let rig = rig(test, "");
    ask(&rig, "d1", "daemon", beat_of(NEW_DAEMON, vec![beat]));
    rig
}

/// `Releases.Status` as the gRPC call answers it: the host `d1` of `web`, with its view.
fn status_of_d1(rig: &Rig) -> proto::ServiceHost {
    let answer = report::status(&rig.coordinator.status_now(), "web");
    let mut hosts = answer.services.into_iter().next().expect("web").hosts;
    assert_eq!(hosts.len(), 1);
    hosts.remove(0)
}

/// `Hosts.Get` of `d1`.
fn host_d1(rig: &Rig) -> proto::Host {
    rig.coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "d1".to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
}

/// The first host `Hosts.List` lists.
fn listed(rig: &Rig) -> proto::Host {
    let all = rig
        .coordinator
        .list_hosts(&operator(), proto::ListHostsRequest::default())
        .expect("the list");
    all.hosts.into_iter().next().expect("d1")
}

/// The health `web`'s view of `d1` says in `Releases.Status`.
fn status_health(rig: &Rig) -> i32 {
    status_of_d1(rig).view.expect("a view").health
}

/// Proves: through `Releases.Status`, a host that beat healthy at second 2000 is
/// `HEALTH_HEALTHY` (live, up) 44 seconds on, `HEALTH_SILENT` (not live, not up) at 45 seconds,
/// still `HEALTH_SILENT` nine hours on, and `HEALTH_HEALTHY` again once it beats again.
///
/// Catches: a silent host reported healthy for as long as its last heartbeat said so, the
/// answer a Mac whose daemon had been silent for nine hours gave; a threshold that is off by a
/// second from the one that makes the host not live; and a host that stays silent after it beats.
#[test]
fn a_host_is_silent_from_the_45th_second_and_healthy_again_after_a_beat() {
    let rig = heard("silent-status", runs(WEB, "1.0"));
    let healthy = agent::Health::Healthy as i32;
    let silent = agent::Health::Silent as i32;

    rig.at(BEAT_AT + 44);
    let host = status_of_d1(&rig);
    let view = host.view.expect("a view");
    assert_eq!((host.live, view.up, view.health), (true, true, healthy));

    rig.at(BEAT_AT + 45);
    let host = status_of_d1(&rig);
    let view = host.view.expect("a view");
    assert_eq!((host.live, view.up, view.health), (false, false, silent));

    rig.at(BEAT_AT + 9 * 3_600);
    assert_eq!(status_health(&rig), silent);

    ask(
        &rig,
        "d1",
        "daemon",
        beat_of(NEW_DAEMON, vec![runs(WEB, "1.0")]),
    );
    assert_eq!(status_health(&rig), healthy);
}

/// Proves: silence overrides whatever the host last reported: an unhealthy service and an absent
/// one are `HEALTH_SILENT` once the host is not live, the installed version stays as last
/// reported (1.0 for the unhealthy service, none for the absent one), and the raw copy of the
/// service's heartbeat entry keeps the health the host sent.
///
/// Catches: a silent host whose unhealthy or absent service keeps saying so, as if it were
/// current, and a silence that wipes the version or the raw entry.
#[test]
fn silence_overrides_an_unhealthy_or_absent_report_and_keeps_the_rest() {
    for (said, kept, version) in [
        ("unhealthy", agent::Health::Unhealthy, Some("1.0")),
        ("absent", agent::Health::Absent, None),
    ] {
        let rig = heard(&format!("silent-status-{said}"), reports(WEB, said));
        rig.at(BEAT_AT + 44);
        let view = status_of_d1(&rig).view.expect("a view");
        assert_eq!(view.health, kept as i32, "{said} while live");

        rig.at(BEAT_AT + 45);
        let host = status_of_d1(&rig);
        let view = host.view.expect("a view");
        assert_eq!(
            view.health,
            agent::Health::Silent as i32,
            "{said} once silent"
        );
        assert_eq!(view.installed_version.as_deref(), version, "{said}");
        assert_eq!(host.beat.expect("a beat").health, kept as i32, "{said}");
    }
}

/// Proves: `Hosts.Get` and `Hosts.List` say `HEALTH_SILENT` in the host's view of a service once
/// the host is not live, and the host's heartbeat, as it sent it, still says the health it
/// reported; while the host is live the view says that health.
///
/// Catches: the per-service views of the host calls aging differently from `Releases.Status`,
/// and a heartbeat copy rewritten to the coordinator's word.
#[test]
fn hosts_get_and_list_say_silent_in_the_view_and_keep_the_heartbeat_as_sent() {
    let rig = heard("silent-hosts", runs(WEB, "1.0"));
    let healthy = agent::Health::Healthy as i32;
    let silent = agent::Health::Silent as i32;
    let sent =
        |host: &proto::Host| host.heartbeat.as_ref().expect("a heartbeat").services[0].health;

    rig.at(BEAT_AT + 44);
    for host in [host_d1(&rig), listed(&rig)] {
        assert!(host.live);
        assert_eq!(host.services[0].health, healthy);
        assert_eq!(sent(&host), healthy);
    }

    rig.at(BEAT_AT + 45);
    for host in [host_d1(&rig), listed(&rig)] {
        assert!(!host.live);
        assert_eq!(host.services[0].service, "web");
        assert_eq!(host.services[0].health, silent);
        assert_eq!(host.services[0].installed_version.as_deref(), Some("1.0"));
        assert_eq!(sent(&host), healthy);
    }
}
