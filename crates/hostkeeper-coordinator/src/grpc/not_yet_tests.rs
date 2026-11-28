//! The listing of one host of a wait that did not end converged, from the row `Releases.Status`
//! answers for it: each state in its turn, the first that holds winning, and what each says.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{MAX_HOSTS, Read, cap, token, waiting_host};

/// The version the tests wait for.
const VERSION: &str = "2.0";

/// The store's clock in the tests.
const NOW: u64 = 1_800_000_000;

/// What the listing reads besides the row: the host heard, silent `silent_secs`, not baking.
fn read(silent_secs: Option<u64>) -> Read<'static> {
    Read {
        version: VERSION,
        silent_secs,
        baking: false,
        unix_now: NOW,
    }
}

/// A live host of the role `storage` with `installed` of the service, the gate's `gate` and
/// `beat` as its last word of the service.
fn row(
    installed: Option<&str>,
    gate: proto::GateReason,
    beat: agent::ServiceBeat,
) -> proto::ServiceHost {
    proto::ServiceHost {
        node_id: "s1".to_owned(),
        role: "storage".to_owned(),
        live: true,
        view: Some(proto::HostService {
            service: "kbf".to_owned(),
            installed_version: installed.map(str::to_owned),
            gate_reason: gate.into(),
            membership: proto::Membership::Present.into(),
            ..proto::HostService::default()
        }),
        beat: Some(beat),
        excluded_reason: None,
    }
}

/// A beat of the service at `version`.
fn beat_at(version: &str) -> agent::ServiceBeat {
    agent::ServiceBeat {
        name: "kbf".to_owned(),
        version: version.to_owned(),
        health: agent::Health::Healthy.into(),
        ..agent::ServiceBeat::default()
    }
}

/// What `host` is listed as: its state, reason and since.
fn listed(
    host: proto::ServiceHost,
    read: &Read<'_>,
) -> Option<(proto::WaitingState, String, Option<u64>)> {
    waiting_host(host, read).map(|w| (w.state(), w.reason, w.since_unix))
}

/// Proves: a host on the version, up, with no promotion pending and not baking, is not listed;
/// with its promotion pending it is SETTLING `promotion_pending`, and baking in its plan
/// SETTLING `baking`.
///
/// Catches: a converged host listed as one the wait waits on, or a host the convergence still
/// waits for (promotion, bake) left out.
#[test]
fn a_host_on_the_version_is_listed_only_while_it_settles() {
    let settled = row(Some(VERSION), proto::GateReason::UpToDate, beat_at(VERSION));
    assert_eq!(listed(settled.clone(), &read(Some(3))), None);

    let mut pending = beat_at(VERSION);
    pending.promotion_pending = true;
    let promoting = row(Some(VERSION), proto::GateReason::UpToDate, pending);
    assert_eq!(
        listed(promoting, &read(Some(3))),
        Some((
            proto::WaitingState::Settling,
            "promotion_pending".to_owned(),
            None
        ))
    );

    let baking = Read {
        baking: true,
        ..read(Some(3))
    };
    assert_eq!(
        listed(settled, &baking),
        Some((proto::WaitingState::Settling, "baking".to_owned(), None))
    );
}

/// Proves: a host whose update runs is UPDATING, with the target, the step and its try, and the
/// version it still runs; with no step said, none is listed.
///
/// Catches: a host stuck in an update listed without the step it is stuck in, or as behind.
#[test]
fn an_updating_host_says_its_target_and_its_step() {
    let mut beat = beat_at("1.0");
    beat.updating = Some(VERSION.to_owned());
    beat.step = Some(agent::StepRun {
        name: "verify".to_owned(),
        attempt: 2,
    });
    let host = row(Some("1.0"), proto::GateReason::Updating, beat.clone());
    let waiting = waiting_host(host, &read(Some(3))).expect("listed");
    assert_eq!(
        waiting,
        proto::WaitingHost {
            node_id: "s1".to_owned(),
            role: "storage".to_owned(),
            state: proto::WaitingState::Updating.into(),
            installed_version: Some("1.0".to_owned()),
            updating_to: Some(VERSION.to_owned()),
            step: Some("verify".to_owned()),
            step_attempt: 2,
            reason: String::new(),
            since_unix: None,
        }
    );
    beat.step = None;
    let host = row(Some("1.0"), proto::GateReason::Updating, beat);
    let waiting = waiting_host(host, &read(Some(3))).expect("listed");
    assert_eq!((waiting.step, waiting.step_attempt), (None, 0));
}

/// Proves: a host not heard for 45 seconds is SILENT since its last heartbeat (the store's clock
/// less its silent seconds), whatever its last word said, an update running among it; a node the
/// service never heard is SILENT with no since.
///
/// Catches: a silent host listed as still updating, or with a since that is not its last
/// heartbeat.
#[test]
fn a_silent_host_is_silent_since_its_last_heartbeat() {
    let mut beat = beat_at("1.0");
    beat.updating = Some(VERSION.to_owned());
    let mut host = row(Some("1.0"), proto::GateReason::Updating, beat);
    host.live = false;
    assert_eq!(
        listed(host.clone(), &read(Some(600))),
        Some((proto::WaitingState::Silent, String::new(), Some(NOW - 600)))
    );
    host.beat = None;
    assert_eq!(
        listed(host, &read(None)),
        Some((proto::WaitingState::Silent, String::new(), None))
    );
}

/// Proves: a host whose last update to the version failed is FAILED with that update's reason and
/// start; one whose failed update was to another version is BEHIND, by its gate.
///
/// Catches: a failure of an older version blamed on this release, or a failed host listed with no
/// reason.
#[test]
fn a_failed_host_says_the_reason_and_the_start_of_its_update_to_the_version() {
    let failed = |target: &str| {
        let mut beat = beat_at("1.0");
        beat.last_update = Some(agent::UpdateResult {
            target: target.to_owned(),
            outcome: agent::Outcome::Failed.into(),
            reason: "health_check".to_owned(),
            started_unix: Some(NOW - 900),
            ..agent::UpdateResult::default()
        });
        beat
    };
    let host = row(Some("1.0"), proto::GateReason::FailedHere, failed(VERSION));
    assert_eq!(
        listed(host, &read(Some(3))),
        Some((
            proto::WaitingState::Failed,
            "health_check".to_owned(),
            Some(NOW - 900)
        ))
    );
    let mut viewed = row(Some("1.0"), proto::GateReason::FailedHere, beat_at("1.0"));
    if let Some(view) = viewed.view.as_mut() {
        view.last_update = failed(VERSION).last_update;
    }
    assert_eq!(
        listed(viewed, &read(Some(3))).map(|(state, ..)| state),
        Some(proto::WaitingState::Failed),
        "the view's last update is read first"
    );
    let older = row(Some("1.0"), proto::GateReason::NoSlot, failed("1.5"));
    assert_eq!(
        listed(older, &read(Some(3))),
        Some((proto::WaitingState::Behind, "no_slot".to_owned(), None))
    );
    let mut succeeded = failed(VERSION);
    if let Some(update) = succeeded.last_update.as_mut() {
        update.outcome = agent::Outcome::Succeeded.into();
    }
    let host = row(Some("1.0"), proto::GateReason::BusyElsewhere, succeeded);
    assert_eq!(
        listed(host, &read(Some(3))),
        Some((
            proto::WaitingState::Behind,
            "busy_elsewhere".to_owned(),
            None
        ))
    );
}

/// Proves: a live host behind the version says the gate's reason; a node the service has no
/// heartbeat of, whose gate says nothing, says where it stands (`missing`, `absent`,
/// `unmanaged`); one whose gate says something says the gate.
///
/// Catches: a host the plan does not order listed with no reason a reader can act on.
#[test]
fn a_host_behind_says_why_it_is_not_ordered() {
    let unheard = |gate: proto::GateReason, membership: proto::Membership| proto::ServiceHost {
        node_id: "s2".to_owned(),
        role: "storage".to_owned(),
        live: true,
        view: Some(proto::HostService {
            gate_reason: gate.into(),
            membership: membership.into(),
            ..proto::HostService::default()
        }),
        beat: None,
        excluded_reason: None,
    };
    for (gate, membership, want) in [
        (
            proto::GateReason::None,
            proto::Membership::Missing,
            "missing",
        ),
        (
            proto::GateReason::Unspecified,
            proto::Membership::Absent,
            "absent",
        ),
        (
            proto::GateReason::None,
            proto::Membership::Unmanaged,
            "unmanaged",
        ),
        (proto::GateReason::None, proto::Membership::Present, "none"),
        (
            proto::GateReason::InstallOrder,
            proto::Membership::Missing,
            "install_order",
        ),
    ] {
        assert_eq!(
            listed(unheard(gate, membership), &read(None)),
            Some((proto::WaitingState::Behind, want.to_owned(), None)),
            "{gate:?} {membership:?}"
        );
    }
    let mut no_view = unheard(proto::GateReason::None, proto::Membership::Missing);
    no_view.view = None;
    assert_eq!(
        listed(no_view, &read(None)),
        Some((proto::WaitingState::Behind, "unspecified".to_owned(), None))
    );
}

/// Proves: a host the convergence does not count (a pair an operator holds, a host the release
/// picks no entry for) is not listed, whatever its state, silent among it; a host behind with
/// another gate is.
///
/// Catches: a failed wait whose message names a host that never kept the service from
/// converging, or whose 64 places fill with such hosts and leave the one it waits on out.
#[test]
fn a_host_the_convergence_does_not_count_is_not_listed() {
    for gate in [
        proto::GateReason::Held,
        proto::GateReason::NoTarget,
        proto::GateReason::NoBuildForHost,
        proto::GateReason::InstallerDropped,
    ] {
        let mut host = row(Some("1.0"), gate, beat_at("1.0"));
        assert_eq!(listed(host.clone(), &read(Some(3))), None, "{gate:?}");
        host.live = false;
        assert_eq!(listed(host, &read(Some(600))), None, "{gate:?} silent");
    }
    let behind = row(Some("1.0"), proto::GateReason::MinUp, beat_at("1.0"));
    assert_eq!(
        listed(behind, &read(Some(3))),
        Some((proto::WaitingState::Behind, "min_up".to_owned(), None))
    );
}

/// Proves: an excluded host not on the version is EXCLUDED with the operator's reason, before any
/// other state; an excluded host on the version is not listed.
///
/// Catches: an exclusion hidden behind the host's silence, or an excluded host on the version
/// listed as one the wait waits on.
#[test]
fn an_excluded_host_says_the_operators_reason() {
    let mut host = row(Some("1.0"), proto::GateReason::NotAMember, beat_at("1.0"));
    host.excluded_reason = Some("disk swap".to_owned());
    host.live = false;
    assert_eq!(
        listed(host, &read(Some(600))),
        Some((proto::WaitingState::Excluded, "disk swap".to_owned(), None))
    );
    let mut on = row(
        Some(VERSION),
        proto::GateReason::NotAMember,
        beat_at(VERSION),
    );
    on.excluded_reason = Some("disk swap".to_owned());
    assert_eq!(listed(on, &read(Some(3))), None);
}

/// Proves: at most [`MAX_HOSTS`] hosts are kept, in the order given, and the rest counted.
///
/// Catches: an answer that grows with the fleet, or one that drops hosts without saying so.
#[test]
fn the_listing_keeps_sixty_four_hosts_and_counts_the_rest() {
    let host = |n: usize| proto::WaitingHost {
        node_id: format!("h{n:03}"),
        ..proto::WaitingHost::default()
    };
    let (kept, left_out) = cap((0..MAX_HOSTS + 2).map(host).collect());
    assert_eq!(kept.len(), MAX_HOSTS);
    assert_eq!(kept.last().map(|h| h.node_id.as_str()), Some("h063"));
    assert_eq!(left_out, 2);
    let (kept, left_out) = cap((0..3).map(host).collect());
    assert_eq!((kept.len(), left_out), (3, 0));
}

/// Proves: an enum value's name is read less its prefix, in lower case, and a name without the
/// prefix as it is.
#[test]
fn a_token_is_the_value_name_less_its_prefix() {
    assert_eq!(token("GATE_REASON_NO_SLOT", "GATE_REASON_"), "no_slot");
    assert_eq!(token("OTHER", "GATE_REASON_"), "other");
}
