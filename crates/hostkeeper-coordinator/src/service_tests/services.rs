//! Services besides the legacy one, and the allow-list entries that name them.

use hostkeeper_oidc::testkit::config_text;

use super::*;
use crate::service::HeartbeatRefused;
use crate::testutil::with_kbf;

const OTHER: &str = r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#;

fn with_services(rig: &mut Rig, text: &str) {
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(with_kbf(text));
}

const OTHER_SERVICE: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";

/// The testkit's single table with `services` appended to it.
fn single_table(services: &str) -> String {
    format!("{}{services}\n", config_text(STATIC_ISSUER))
}

/// Catches: a defined service that cannot be set, or one the file does not define that can:
/// with `other` defined and the entry naming it CI sets it (the status lists it beside
/// kbf's), the heartbeat of a kbf host is still answered from kbf alone, and a service
/// nobody defined is refused.
#[tokio::test]
async fn ci_sets_a_defined_service_and_kbf_is_unchanged() {
    let mut rig = Rig::new("svc-defined");
    rig.coordinator = Rig::start_with_allow_list(
        &rig.dir,
        &rig.clock,
        &single_table("services = [\"kbf\", \"other\"]"),
    )
    .with_services(with_kbf(OTHER_SERVICE));
    rig.set(OTHER).await;
    rig.set(STATE).await;
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.services["other"].version.as_str(), "3.0");
    assert_eq!(status.desired.expect("kbf").version.as_str(), "2.0");
    rig.at(2_000);
    let reply = rig
        .coordinator
        .heartbeat(who("d1", role("daemon")), beat("1.0"))
        .expect("a heartbeat");
    assert_eq!(reply, update("2.0"));
    let unknown = OTHER.replace("other", "unknown");
    let set = rig
        .coordinator
        .set_desired(Some(&ci_token()), unknown.as_bytes())
        .await;
    assert!(matches!(set, Err(SetError::Invalid(_))));
}

/// Catches: a single-table allow-list that means more than kbf, or less: it sets kbf, and a
/// defined service the table does not name is a 403 that changes nothing.
#[tokio::test]
async fn a_single_table_sets_kbf_and_is_refused_for_another_service() {
    let mut rig = Rig::new("svc-single");
    with_services(&mut rig, OTHER_SERVICE);
    let set = rig
        .coordinator
        .set_desired(Some(&ci_token()), OTHER.as_bytes())
        .await;
    assert!(matches!(set, Err(SetError::Forbidden)));
    assert!(SetError::Forbidden.to_string().contains("may not"));
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    assert!(status.desired.is_none());
    assert!(status.services.is_empty());
    rig.set(STATE).await;
}

/// A beat of a new updater: kbf at `kbf`, `other` at `other`.
fn new_beat(kbf: &str, other: &str) -> hostkeeper_wire::wire::Heartbeat {
    let mut b = beat(kbf);
    b.services = Some(vec![service_beat(other)]);
    b
}

fn service_beat(running: &str) -> hostkeeper_wire::wire::ServiceBeat {
    let b = beat(running);
    hostkeeper_wire::wire::ServiceBeat {
        name: "other".to_owned(),
        version: b.version,
        health: b.health,
        last_update: None,
        updating: None,
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
    }
}

fn update_of(service: &str, target: &str) -> hostkeeper_wire::wire::ServiceReply {
    hostkeeper_wire::wire::ServiceReply::Update {
        service: Some(service.to_owned()),
        version: version(target),
    }
}

/// `other` rolled in one stage (`stages = [100]`), so the state's limit alone holds a host.
const OTHER_ONE_STAGE: &str =
    "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\nstages = [100]\n";

/// The state of `other` with one daemon updating at a time.
const OTHER_ONE_AT_A_TIME: &str = r#"{"service":"other","version":"3.0","max_updating":{"daemon":1},"min_up":{"storage":0,"frontend":0}}"#;

/// A rig whose coordinator defines `other` beside kbf: `other` is rolled in one stage, one
/// daemon at a time (its `max_updating`).
async fn services_rig(name: &str) -> Rig {
    let mut rig = Rig::new(name);
    rig.coordinator = Rig::start_with_allow_list(
        &rig.dir,
        &rig.clock,
        &single_table("services = [\"kbf\", \"other\"]"),
    )
    .with_services(with_kbf(OTHER_ONE_STAGE));
    rig.set(STATE).await;
    rig.set(OTHER_ONE_AT_A_TIME).await;
    rig.at(2_000);
    rig
}

/// Behaviour: a host that sends `services` is ordered a service in the reply shape that
/// names it, and its service beat is recorded as that service's state of the host: while d1
/// reports `other` updating, d2 (one slot) is told nothing, and when d1 is done d2 is told.
#[tokio::test]
async fn a_new_updater_is_ordered_a_service_and_its_beat_is_recorded() {
    let rig = services_rig("svc-wire-order").await;
    let c = &rig.coordinator;
    assert_eq!(
        c.heartbeat(who("d1", role("daemon")), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        update_of("other", "3.0")
    );
    let mut busy = new_beat("2.0", "1.0");
    busy.services.as_mut().expect("services")[0].updating = Some(version("3.0"));
    c.heartbeat(who("d1", role("daemon")), busy)
        .expect("a heartbeat");
    assert_eq!(
        c.heartbeat(who("d2", role("daemon")), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        hostkeeper_wire::wire::ServiceReply::Nothing
    );
    c.heartbeat(who("d1", role("daemon")), new_beat("2.0", "3.0"))
        .expect("a heartbeat");
    assert_eq!(
        c.heartbeat(who("d2", role("daemon")), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        update_of("other", "3.0")
    );
}

/// Behaviour: one order per reply, kbf first. A host behind on both is told kbf in the old
/// shape; while that order stands it is told nothing for `other`; once it runs kbf's version
/// it is told `other`.
#[tokio::test]
async fn kbf_is_ordered_before_a_service_and_one_order_per_reply() {
    let rig = services_rig("svc-wire-first").await;
    let c = &rig.coordinator;
    let host = || who("d1", role("daemon"));
    assert_eq!(
        c.heartbeat(host(), new_beat("1.0", "1.0"))
            .expect("a heartbeat"),
        update("2.0")
    );
    assert_eq!(
        c.heartbeat(host(), new_beat("1.0", "1.0"))
            .expect("a heartbeat"),
        update("2.0"),
        "asked again before it reports, it is told the same"
    );
    assert_eq!(
        c.heartbeat(host(), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        update_of("other", "3.0")
    );
}

/// Behaviour: a host that sent no `services` is never named a service, even
/// when other hosts are ordered `other` and it is behind on it; and entries for a service nobody
/// defined, or one that does not run on the host's role, are ignored (not recorded, not
/// ordered).
#[tokio::test]
async fn only_an_updater_that_announced_a_service_is_ordered_it() {
    let rig = services_rig("svc-wire-old").await;
    let c = &rig.coordinator;
    let host = || who("d1", role("daemon"));
    assert_eq!(
        c.heartbeat(host(), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        update_of("other", "3.0")
    );
    assert_eq!(
        c.heartbeat(who("d3", role("daemon")), beat("2.0"))
            .expect("a heartbeat"),
        hostkeeper_wire::wire::ServiceReply::Nothing,
        "an old updater is a kbf-only host"
    );
    assert_eq!(
        c.heartbeat(host(), beat("2.0")).expect("a heartbeat"),
        hostkeeper_wire::wire::ServiceReply::Nothing,
        "the latest beat says what the host is: it announces nothing now"
    );
    let mut stray = new_beat("2.0", "1.0");
    stray.services.as_mut().expect("services")[0].name = "unheard-of".to_owned();
    assert_eq!(
        c.heartbeat(who("d4", role("daemon")), stray)
            .expect("a heartbeat"),
        hostkeeper_wire::wire::ServiceReply::Nothing
    );
    assert_eq!(
        c.heartbeat(who("s1", role("storage")), new_beat("2.0", "1.0"))
            .expect("a heartbeat"),
        hostkeeper_wire::wire::ServiceReply::Nothing,
        "other does not run on storage hosts"
    );
}

const SERVER_AGENT: &str = "[[service]]\nname = \"js-server\"\nroles = [\"daemon\"]\n\
    [[service]]\nname = \"js-agent\"\nroles = [\"daemon\"]\n";

fn body_for(service: &str) -> String {
    format!(r#"{{"service":"{service}","version":"3.0","min_up":{{"storage":0,"frontend":0}}}}"#)
}

fn entry_b_token() -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    claims["repository_id"] = json!("777");
    claims["job_workflow_ref"] =
        json!("example-org/other/.github/workflows/release.yml@refs/heads/main");
    mint(&keys()[0], &claims)
}

/// Catches: services leaking between entries, or one entry's services ignored. Entry A
/// names `kbf` and entry B `js-server` and `js-agent`: A's token sets kbf and gets a 403 for
/// js-server; B's token sets js-server and js-agent and gets a 403 for kbf; a token no entry
/// admits is refused as before, and each refusal leaves the state as it was.
#[tokio::test]
async fn each_entry_sets_only_its_own_services() {
    let mut rig = Rig::new("svc-entries");
    let list = format!(
        "{}services = [\"kbf\"]\n\n[[remote_operator]]\nissuer = \"{STATIC_ISSUER}\"\n\
         audience = \"hostkeeper-test\"\nrepository_id = \"777\"\n\
         job_workflow_ref = [\"example-org/other/.github/workflows/release.yml@refs/heads/main\"]\n\
         refs = [\"refs/heads/main\"]\nservices = [\"js-server\", \"js-agent\"]\n",
        config_text(STATIC_ISSUER).replace("[remote_operator]", "[[remote_operator]]"),
    );
    rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(with_kbf(SERVER_AGENT));
    let c = &rig.coordinator;

    let a = ci_token();
    assert!(matches!(
        c.set_desired(Some(&a), body_for("js-server").as_bytes())
            .await,
        Err(SetError::Forbidden)
    ));
    let a = ci_token();
    c.set_desired(Some(&a), STATE.as_bytes())
        .await
        .expect("A sets kbf");

    let b = entry_b_token();
    assert!(matches!(
        c.set_desired(Some(&b), STATE.as_bytes()).await,
        Err(SetError::Forbidden)
    ));
    for service in ["js-server", "js-agent"] {
        let b = entry_b_token();
        c.set_desired(Some(&b), body_for(service).as_bytes())
            .await
            .expect("B sets its service");
    }

    let mut stranger = good_claims(STATIC_ISSUER);
    stranger["repository_id"] = json!("999");
    let stranger = mint(&keys()[0], &stranger);
    let refused = c.set_desired(Some(&stranger), STATE.as_bytes()).await;
    assert!(
        matches!(refused, Err(SetError::Unauthorized(_))),
        "{refused:?}"
    );

    let status = c.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.desired.expect("kbf").version.as_str(), "2.0");
    assert_eq!(status.services["js-server"].version.as_str(), "3.0");
    assert_eq!(status.services["js-agent"].version.as_str(), "3.0");
    assert_eq!(status.services.len(), 3);
}

/// Behaviour: the convergence and the status of a second service come from the heartbeats
/// that carry that service and from its own desired state. While `other` has one of its two
/// daemons at 3.0 and the other behind, kbf (all at 2.0) reads converged and `other` in
/// progress; when the second daemon reports 3.0 `other` converges, and the status counts
/// each service's hosts on their own. A name the coordinator does not manage is an error,
/// and so is a read with a bad token.
#[tokio::test]
async fn a_second_services_convergence_and_status_come_from_its_own_hosts() {
    let mut rig = Rig::new("svc-converge-other");
    rig.coordinator = Rig::start_with_allow_list(
        &rig.dir,
        &rig.clock,
        &single_table("services = [\"kbf\", \"other\"]"),
    )
    .with_services(with_kbf(OTHER_SERVICE));
    rig.set(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#)
        .await;
    rig.set(OTHER).await;
    rig.at(2_000);
    let c = &rig.coordinator;
    let read = |service: &'static str| async move {
        c.convergence(Some(&ci_token()), service)
            .await
            .expect("read")
    };
    c.heartbeat(who("d1", role("daemon")), new_beat("2.0", "3.0"))
        .expect("a heartbeat");
    c.heartbeat(who("d2", role("daemon")), new_beat("2.0", "1.0"))
        .expect("a heartbeat");
    let kbf = read("kbf").await;
    assert_eq!(kbf.state, State::Converged);
    assert_eq!(kbf.roles[0].at_desired, 2);
    let other = read("other").await;
    assert_eq!(other.state, State::InProgress);
    assert_eq!(other.desired.expect("desired").as_str(), "3.0");
    assert_eq!(other.roles[0].at_desired, 1);
    c.heartbeat(who("d2", role("daemon")), new_beat("2.0", "3.0"))
        .expect("a heartbeat");
    assert_eq!(read("other").await.state, State::Converged);

    let status = c.status(Some(OPERATOR)).expect("status");
    assert_eq!(
        status
            .per_service
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["kbf", "other"]
    );
    assert_eq!(status.per_service["kbf"].roles[0].at_desired, 2);
    assert_eq!(status.per_service["other"].roles[0].at_desired, 2);
    assert_eq!(
        status.per_service["other"]
            .desired
            .as_ref()
            .expect("other")
            .version
            .as_str(),
        "3.0"
    );

    let unknown = c.convergence(Some(&ci_token()), "nope").await;
    assert_eq!(unknown.map(|_| ()), Err(ConvergenceError::UnknownService));
    let refused = c.convergence(None, "other").await;
    assert_eq!(
        refused.map(|_| ()),
        Err(ConvergenceError::Refused(CiRefused::Unauthorized(
            "missing"
        )))
    );
    assert_eq!(
        ConvergenceError::UnknownService.to_string(),
        "no such service"
    );
}

/// Catches: a service matched by prefix or without regard to case. The entry names `kbf`
/// and `js`, and the rollout file defines `js` and `js-server`: `js` is set, `js-server`
/// (which starts with `js`) is a 403, and `JS` is no defined service, so a 400. An entry
/// that names `JS` gets a 403 for `js`. Each refusal leaves the state as it was.
#[tokio::test]
async fn a_service_is_allowed_by_its_exact_name_only() {
    let start = |rig: &Rig, named: &str| {
        let list = config_text(STATIC_ISSUER) + &format!("services = [\"kbf\", \"{named}\"]\n");
        Rig::start_with_allow_list(&rig.dir, &rig.clock, &list).with_services(with_kbf(
            "[[service]]\nname = \"js\"\nroles = [\"daemon\"]\n\
             [[service]]\nname = \"js-server\"\nroles = [\"daemon\"]\n",
        ))
    };
    let mut rig = Rig::new("svc-exact");
    rig.coordinator = start(&rig, "js");
    let c = &rig.coordinator;
    c.set_desired(Some(&ci_token()), body_for("js").as_bytes())
        .await
        .expect("js is named");
    assert!(matches!(
        c.set_desired(Some(&ci_token()), body_for("js-server").as_bytes())
            .await,
        Err(SetError::Forbidden)
    ));
    assert!(matches!(
        c.set_desired(Some(&ci_token()), body_for("JS").as_bytes())
            .await,
        Err(SetError::Invalid(_))
    ));
    let status = c.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.services.keys().collect::<Vec<_>>(), ["js"]);

    let mut rig = Rig::new("svc-exact-case");
    rig.coordinator = start(&rig, "JS");
    let c = &rig.coordinator;
    assert!(matches!(
        c.set_desired(Some(&ci_token()), body_for("js").as_bytes())
            .await,
        Err(SetError::Forbidden)
    ));
    assert!(
        c.status(Some(OPERATOR))
            .expect("status")
            .services
            .is_empty()
    );
}

/// A coordinator with no legacy service over the service `other` alone, whose allow-list
/// entry names `other`.
fn without_legacy(name: &str) -> Rig {
    let mut rig = Rig::new(name);
    rig.coordinator = Rig::start_with_allow_list(
        &rig.dir,
        &rig.clock,
        &single_table("services = [\"other\"]"),
    )
    .with_services(crate::services::Services::parse(OTHER_SERVICE).expect("services"))
    .with_legacy_service(None);
    rig
}

/// A beat that lists `other` at `running`.
fn lists_other(running: &str) -> hostkeeper_wire::wire::Heartbeat {
    let mut b = beat(running);
    b.services = Some(vec![service_beat(running)]);
    b
}

/// Catches: a heartbeat that belongs to no service being taken for one: with no legacy
/// service a heartbeat that lists no services is refused and its host is not noted, while one
/// that lists a service (even an empty list) is answered.
#[test]
fn without_a_legacy_service_a_heartbeat_listing_no_services_is_refused() {
    let rig = without_legacy("svc-no-legacy-beat");
    rig.at(2_000);
    let c = &rig.coordinator;
    assert_eq!(c.legacy_service(), None);
    let refused = c.heartbeat(who("d1", role("daemon")), beat("1.0"));
    assert!(matches!(refused, Err(HeartbeatRefused)));
    assert_eq!(
        HeartbeatRefused.to_string(),
        "it lists no services and no legacy service is set"
    );
    let status = c.status(Some(OPERATOR)).expect("status");
    assert!(status.hosts.is_empty());
    assert!(status.per_service["other"].hosts.is_empty());

    let mut empty = beat("1.0");
    empty.services = Some(Vec::new());
    let answer = c
        .heartbeat(who("d1", role("daemon")), empty)
        .expect("an empty list is a list");
    assert_eq!(answer.reply, hostkeeper_wire::wire::ServiceReply::Nothing);
    c.heartbeat(who("d2", role("daemon")), lists_other("1.0"))
        .expect("a heartbeat that lists a service");
    let status = c.status(Some(OPERATOR)).expect("status");
    assert!(status.hosts.is_empty(), "the top level is nobody's");
    assert_eq!(status.per_service["other"].hosts.len(), 1);
}

/// Catches: a desired state with no `service` landing somewhere when there is no legacy
/// service to take it: it is refused as a state for no service, nothing is stored, and a
/// body that names an admitted service is set.
#[tokio::test]
async fn without_a_legacy_service_a_body_naming_no_service_is_refused() {
    let rig = without_legacy("svc-no-legacy-set");
    let c = &rig.coordinator;
    let set = c.set_desired(Some(&ci_token()), STATE.as_bytes()).await;
    assert_eq!(
        set.map(|_| ()).map_err(|e| e.to_string()),
        Err("the desired state is malformed: service is required".to_owned())
    );
    let status = c.status(Some(OPERATOR)).expect("status");
    assert!(status.desired.is_none());
    assert!(status.services.is_empty());
    c.set_desired(Some(&ci_token()), OTHER.as_bytes())
        .await
        .expect("other is named by the entry");
    let status = c.status(Some(OPERATOR)).expect("status");
    assert!(status.desired.is_none(), "the legacy state stays empty");
    assert_eq!(status.services["other"].version.as_str(), "3.0");
}

/// Catches: an allow-list entry without `services` admitting more than the legacy service,
/// or nothing: with kbf as the legacy service it sets a body without `service`, a body naming
/// `kbf`, and no other service; with no legacy service it sets nothing at all.
#[tokio::test]
async fn an_entry_without_services_stands_for_the_legacy_service_alone() {
    let mut rig = Rig::new("svc-entry-legacy");
    with_services(&mut rig, OTHER_SERVICE);
    let c = &rig.coordinator;
    let named = STATE.replacen('{', r#"{"service":"kbf","#, 1);
    c.set_desired(Some(&ci_token()), STATE.as_bytes())
        .await
        .expect("no service is the legacy one");
    c.set_desired(Some(&ci_token()), named.as_bytes())
        .await
        .expect("the legacy service by name");
    let other = c.set_desired(Some(&ci_token()), OTHER.as_bytes()).await;
    assert!(matches!(other, Err(SetError::Forbidden)));

    let mut rig = Rig::new("svc-entry-no-legacy");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(with_kbf(OTHER_SERVICE))
        .with_legacy_service(None);
    let c = &rig.coordinator;
    for body in [OTHER, named.as_str()] {
        let set = c.set_desired(Some(&ci_token()), body.as_bytes()).await;
        assert!(matches!(set, Err(SetError::Forbidden)), "{body}");
    }
    let set = c.set_desired(Some(&ci_token()), STATE.as_bytes()).await;
    assert!(matches!(set, Err(SetError::Invalid(_))));
}

/// Catches: the agent of a host missing from the status or shown twice: a heartbeat without
/// `agent` shows the legacy agent's name and no version, one with `agent` shows its name and
/// version, in the status of the host and in the host of each service, and the host's
/// flattened heartbeat carries no `agent` key of its own.
#[test]
fn the_status_shows_the_agent_of_each_host() {
    use hostkeeper_wire::wire::{Agent, AgentName, LEGACY_AGENT_NAME};

    let rig = Rig::new("svc-agent");
    rig.at(2_000);
    let c = &rig.coordinator;
    let mut named = beat("1.0");
    named.agent = Some(Agent {
        name: AgentName::parse("hostkeeper-updater").expect("an agent name"),
        version: version("0.3.1"),
    });
    c.heartbeat(who("d1", role("daemon")), beat("1.0"))
        .expect("a heartbeat");
    c.heartbeat(who("d2", role("daemon")), named)
        .expect("a heartbeat");
    let status = c.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.hosts[0].agent.name, LEGACY_AGENT_NAME);
    assert_eq!(status.hosts[0].agent.version, None);
    assert_eq!(status.hosts[1].agent.name, "hostkeeper-updater");
    assert_eq!(status.hosts[1].agent.version, Some(version("0.3.1")));

    let text = serde_json::to_value(&status).expect("JSON");
    let hosts = [&text["hosts"], &text["per_service"]["kbf"]["hosts"]];
    for hosts in hosts {
        assert_eq!(hosts[0]["agent"], json!({"name": "kbf-updater"}));
        assert_eq!(
            hosts[1]["agent"],
            json!({"name": "hostkeeper-updater", "version": "0.3.1"})
        );
    }
    let raw = serde_json::to_string(&status).expect("JSON");
    assert_eq!(raw.matches(r#""agent":"#).count(), 4, "{raw}");
}

/// Catches: an agent shown for a host that has since said something else: the status
/// follows the host's latest heartbeat.
#[test]
fn the_status_follows_the_latest_agent_of_a_host() {
    use hostkeeper_wire::wire::{Agent, AgentName};

    let rig = Rig::new("svc-agent-latest");
    rig.at(2_000);
    let c = &rig.coordinator;
    let mut named = beat("1.0");
    named.agent = Some(Agent {
        name: AgentName::parse("hostkeeper-updater").expect("an agent name"),
        version: version("0.3.1"),
    });
    c.heartbeat(who("d1", role("daemon")), named)
        .expect("a heartbeat");
    c.heartbeat(who("d1", role("daemon")), beat("1.0"))
        .expect("a heartbeat");
    let status = c.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.hosts[0].agent.name, "kbf-updater");
    assert_eq!(status.hosts[0].agent.version, None);
}
