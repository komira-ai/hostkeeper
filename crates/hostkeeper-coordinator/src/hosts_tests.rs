//! `Hosts.List` and `Hosts.Get` over a coordinator that heard its hosts: the pages, the
//! refusals, and each fact of a host as the status holds it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Agent, Heartbeat};
use serde_json::json;

use crate::hosts::MAX_HOSTS_PAGE;
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, beat_of, hold, operator, rig as marks_rig, runs, scope_host, start,
};
use crate::operator::Caller;
use crate::testutil::{OPERATOR, Rig, ops_of};

/// What the ops store's wall clock reads when the coordinator's clock reads 0.
const UNIX: u64 = 1_800_000_000;

/// A rig of the services `web` and `db` past its quiet start, whose ops store's wall clock
/// reads [`UNIX`] on from the coordinator's clock: it moves when the rig's clock does.
fn rig(test: &str) -> Rig {
    let mut rig = marks_rig(test, "");
    let seconds = Arc::clone(&rig.clock);
    let unix: crate::audit_file::UnixClock =
        Arc::new(move || UNIX + seconds.load(Ordering::SeqCst));
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(ops_of(&rig.dir).with_unix(unix));
    rig
}

/// A heartbeat of the daemon agent 0.4.1 running `web` and `db` 1.0.
fn daemon() -> Heartbeat {
    let mut beat = beat_of(NEW_DAEMON, vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    beat.agent = Some(
        serde_json::from_value::<Agent>(json!({"name": "hostkeeper-daemon", "version": "0.4.1"}))
            .expect("an agent"),
    );
    beat
}

fn list(
    rig: &Rig,
    limit: u32,
    token: &str,
) -> Result<proto::ListHostsResponse, proto::HostsRefusal> {
    rig.coordinator.list_hosts(
        &operator(),
        proto::ListHostsRequest {
            limit,
            page_token: token.to_owned(),
        },
    )
}

/// The node ids of a page and its next page token.
fn page(rig: &Rig, limit: u32, token: &str) -> (Vec<String>, String) {
    let listed = list(rig, limit, token).expect("a page");
    let ids = listed.hosts.into_iter().map(|host| host.node_id).collect();
    (ids, listed.next_page_token)
}

fn get(rig: &Rig, node_id: &str) -> Result<proto::Host, proto::HostsRefusal> {
    rig.coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: node_id.to_owned(),
            },
        )
        .map(|got| got.host.expect("a host"))
}

/// Proves: the hosts are listed by node id whatever order they were heard in, a page holds at
/// most `limit` and names the last node id of it as the next page's token while hosts remain,
/// a page that ends the list (exactly full, a `limit` past the count, a `limit` of 0, or a
/// token past every host) has no token, and an empty fleet lists nothing.
///
/// Catches: an order by arrival, a page one too long or short, a token on the last page, which
/// would send the UI asking for a page that is empty, or a page that skips or repeats a host.
#[test]
fn list_pages_the_hosts_by_node_id() {
    let rig = rig("hosts-pages");
    assert_eq!(page(&rig, 0, ""), (vec![], String::new()), "nothing heard");
    for host in ["c1", "a1", "b1"] {
        ask(&rig, host, "daemon", daemon());
    }
    let ids = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    assert_eq!(page(&rig, 2, ""), (ids(&["a1", "b1"]), "b1".to_owned()));
    assert_eq!(page(&rig, 2, "b1"), (ids(&["c1"]), String::new()));
    assert_eq!(page(&rig, 1, "a1"), (ids(&["b1"]), "b1".to_owned()));
    assert_eq!(page(&rig, 3, ""), (ids(&["a1", "b1", "c1"]), String::new()));
    assert_eq!(page(&rig, 0, ""), (ids(&["a1", "b1", "c1"]), String::new()));
    assert_eq!(page(&rig, 9, ""), (ids(&["a1", "b1", "c1"]), String::new()));
    // A token need not name a host the coordinator heard: the page starts after it.
    assert_eq!(page(&rig, 0, "a5"), (ids(&["b1", "c1"]), String::new()));
    assert_eq!(page(&rig, 0, "c1"), (vec![], String::new()));
    assert_eq!(page(&rig, 0, "z9"), (vec![], String::new()));
}

/// Proves: a page holds at most [`MAX_HOSTS_PAGE`] hosts, for a `limit` of 0 and for one above
/// it, and the host past it is the next page.
///
/// Catches: a `limit` taken as asked past the cap, or a cap with no next page.
#[test]
fn list_answers_at_most_the_cap_per_page() {
    let rig = rig("hosts-cap");
    for n in 0..=MAX_HOSTS_PAGE {
        ask(&rig, &format!("h{n:03}"), "daemon", daemon());
    }
    let last = format!("h{:03}", MAX_HOSTS_PAGE - 1);
    for limit in [0, 257, u32::MAX] {
        let (ids, token) = page(&rig, limit, "");
        assert_eq!(
            (ids.len(), token.as_str()),
            (MAX_HOSTS_PAGE, last.as_str()),
            "{limit}"
        );
    }
    let rest = page(&rig, 0, &last);
    assert_eq!(rest, (vec![format!("h{MAX_HOSTS_PAGE:03}")], String::new()));
}

/// Proves: `List` and `Get` refuse no caller `unauthorized` and CI `forbidden`; a page token
/// that is no node id, and a node id that is none (empty, upper case), `invalid_request`; and a
/// node id the coordinator has not heard from `unknown_host`.
///
/// Catches: a host list read by CI's token, or a typo of a node id answered as an empty host
/// rather than refused.
#[test]
fn list_and_get_refuse_with_the_value_of_their_cause() {
    let rig = rig("hosts-refusals");
    ask(&rig, "d1", "daemon", daemon());
    let c = &rig.coordinator;
    for (caller, refusal) in [
        (Caller::Anonymous, proto::HostsRefusal::Unauthorized),
        (Caller::Ci, proto::HostsRefusal::Forbidden),
    ] {
        assert_eq!(
            c.list_hosts(&caller, proto::ListHostsRequest::default()),
            Err(refusal)
        );
        let d1 = proto::GetHostRequest {
            node_id: "d1".to_owned(),
        };
        assert_eq!(c.get_host(&caller, &d1), Err(refusal));
    }
    assert_eq!(
        list(&rig, 0, "Not A Node"),
        Err(proto::HostsRefusal::InvalidRequest)
    );
    for bad in ["", "D1", "-d1", "d1 "] {
        assert_eq!(
            get(&rig, bad),
            Err(proto::HostsRefusal::InvalidRequest),
            "{bad:?}"
        );
    }
    assert_eq!(get(&rig, "ghost"), Err(proto::HostsRefusal::UnknownHost));
}

/// Proves: a host is what the coordinator holds of it: its role from its certificate, live, the
/// time of its last heartbeat on the ops store's wall clock (not the coordinator's own), its heartbeat put back together
/// (platform, agent, features, the entry of each service that heard it), the labels an
/// operator set, its exclusions, and a view of each service that heard it or does not count it,
/// equal to the view `Releases.Status` gives of the same pair; a service that excluded it shows
/// it excluded with the reason and nothing it no longer hears; `Get` answers the host `List`
/// does.
///
/// Catches: a fact of the host read from somewhere other than the state the status reads (a
/// label of another host, a hold of another service), a heartbeat time on the coordinator's
/// own clock, or a host whose excluded service disappears from its page.
#[test]
fn a_host_is_what_the_status_holds_of_it() {
    let rig = rig("hosts-facts");
    ask(&rig, "d1", "daemon", daemon());
    ask(&rig, "d2", "daemon", daemon());
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"set_labels","node_id":"d1","labels":{"rack":"r1"}}"#,
        )
        .expect("labelled");
    let held = hold(&rig, WEB, scope_host("d1"))
        .expect("held")
        .hold
        .expect("a hold");
    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: DB.to_owned(),
                node_id: "d1".to_owned(),
                reason: "noisy".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    ask(&rig, "d1", "daemon", daemon());
    rig.at(2_010);

    let host = get(&rig, "d1").expect("d1");
    let status = rig.coordinator.status(Some(OPERATOR)).expect("the status");
    let web = crate::grpc::report::status(&status, WEB).services[0]
        .hosts
        .iter()
        .find(|h| h.node_id == "d1")
        .and_then(|h| h.view.clone())
        .expect("web's view of d1");
    assert_eq!(web.holds, vec![held], "the hold of d1 is in web's view");
    let expected = proto::Host {
        node_id: "d1".to_owned(),
        role: "daemon".to_owned(),
        live: true,
        last_heartbeat_unix: UNIX + 2_000,
        heartbeat: Some(agent::HeartbeatRequest {
            platform: agent::Platform::LinuxX8664.into(),
            agent: Some(agent::AgentInfo {
                name: "hostkeeper-daemon".to_owned(),
                version: "0.4.1".to_owned(),
            }),
            features: NEW_DAEMON.iter().map(|f| (*f).to_owned()).collect(),
            services: vec![agent::ServiceBeat {
                name: WEB.to_owned(),
                version: "1.0".to_owned(),
                health: agent::Health::Healthy.into(),
                ..agent::ServiceBeat::default()
            }],
            sent_unix: None,
            cert_not_after_unix: None,
            last_renew_error: None,
        }),
        labels: HashMap::from([("rack".to_owned(), "r1".to_owned())]),
        exclusions: vec![proto::Exclusion {
            service: DB.to_owned(),
            reason: "noisy".to_owned(),
        }],
        services: vec![
            proto::HostService {
                service: DB.to_owned(),
                membership: proto::Membership::Excluded.into(),
                excluded_reason: Some("noisy".to_owned()),
                ..proto::HostService::default()
            },
            web,
        ],
        // A coordinator without enrollment holds no host as enrolled.
        inventory_record_id: String::new(),
        data_macs: Vec::new(),
        bmc_mac: String::new(),
        serial: String::new(),
        enrolled_unix: 0,
        cert_not_after_unix: None,
        last_renew_error: None,
        clock_skew_beats: 0,
    };
    assert_eq!(host, expected);
    let listed = list(&rig, 0, "").expect("a page");
    assert_eq!(listed.hosts.first(), Some(&expected));
    let d2 = &listed.hosts[1];
    assert_eq!(
        (d2.labels.len(), d2.exclusions.len(), d2.services.len()),
        (0, 0, 2),
        "d1's labels, exclusion and hold are d1's alone"
    );
    assert!(d2.services.iter().all(|view| view.holds.is_empty()));
}

/// Proves: a host is live until it has been silent for 45 seconds, and listed after, not live,
/// with the time of its last heartbeat unchanged; a host whose services do not take it is
/// listed with no service and no platform.
///
/// Catches: a silent host dropped from the list, a live flag that is off by one second at the
/// down limit, or a heartbeat time that moves while the host is silent.
#[test]
fn a_silent_host_stays_listed_and_is_not_live() {
    let rig = rig("hosts-silent");
    ask(&rig, "d1", "daemon", daemon());
    ask(&rig, "s1", "storage", beat_of(NEW_DAEMON, Vec::new()));
    let seen = |rig: &Rig| -> (bool, u64) {
        let host = get(rig, "d1").expect("d1");
        (host.live, host.last_heartbeat_unix)
    };
    rig.at(2_044);
    assert_eq!(seen(&rig), (true, UNIX + 2_000));
    rig.at(2_045);
    assert_eq!(seen(&rig), (false, UNIX + 2_000));
    let lone = get(&rig, "s1").expect("s1");
    assert_eq!(lone.services, Vec::new());
    let beat = lone.heartbeat.expect("a heartbeat");
    assert_eq!(
        (beat.platform(), beat.services.len()),
        (agent::Platform::Unspecified, 0)
    );
    assert_eq!(page(&rig, 0, "").0, vec!["d1".to_owned(), "s1".to_owned()]);
}

/// Proves: the platform of a host's heartbeat is the one it sent, for each platform a heartbeat
/// can name, with the entry of each service that heard it.
///
/// Catches: one platform said as another.
#[test]
fn the_heartbeat_of_a_host_names_the_platform_it_sent() {
    let rig = rig("hosts-platforms");
    for (host, platform, said) in [
        ("x1", Platform::LinuxX86_64, agent::Platform::LinuxX8664),
        ("a1", Platform::LinuxArm64, agent::Platform::LinuxArm64),
        ("m1", Platform::DarwinArm64, agent::Platform::DarwinArm64),
    ] {
        let mut beat = daemon();
        beat.platform = platform;
        ask(&rig, host, "daemon", beat);
        let got = get(&rig, host).expect("a host");
        let sent = got.heartbeat.expect("a heartbeat");
        assert_eq!((sent.platform(), sent.services.len()), (said, 2), "{host}");
    }
}
