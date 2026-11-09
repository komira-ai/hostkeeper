//! The drift a daemon's installer found (`ServiceBeat.drift`, docs/design/packages-repositories.md
//! section 4.4), as an operator sees it: `Hosts.Get` shows what the daemon's last heartbeat over
//! gRPC reported for each service, until a heartbeat without it, and shows nothing the coordinator
//! does not read.

mod support;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use support::beats::{Daemon, grpc_beat, host, json_beat, runs};
use support::rig::{Rig, Spec};

const SERVICES: &str = r#"
[[service]]
name = "web"
roles = ["daemon"]
stages = [100]

[[service]]
name = "db"
roles = ["daemon"]
stages = [100]
"#;

const X86: agent::Platform = agent::Platform::LinuxX8664;
const ARCHIVE: &str = "hostkeeper-archive";

/// A time past the coordinator's quiet start.
const LATER: u64 = 2_000;

fn rig(name: &str) -> Rig {
    Rig::new(
        name,
        Spec::recorded()
            .with_services(SERVICES)
            .with_oidc_services(&["web"])
            .without_legacy(),
    )
    .routed()
}

/// A daemon of `target-v1` on `d1` running `web` from `deb` with `one`.
fn daemon(one: support::beats::Runs) -> Daemon {
    Daemon::new("d1", X86, &[ARCHIVE, "deb"]).running(one)
}

/// The drift `Hosts.Get` shows for `web` on `d1`.
async fn shown(rig: &Rig) -> Option<agent::DriftReport> {
    let host: proto::Host = host(rig, "d1").await;
    host.heartbeat.expect("a heartbeat").services[0]
        .drift
        .clone()
}

/// Proves: `Hosts.Get` shows the drift of a service as its daemon reported it over gRPC, with an
/// absent `found` (the package is not installed) and an absent `recorded` kept absent, and the
/// next heartbeat without drift clears it.
/// Catches: a drift never shown, an absent string shown as an empty one, or a drift that
/// outlives the heartbeat that stopped reporting it.
#[tokio::test]
async fn a_drift_is_shown_until_a_heartbeat_without_it() {
    let rig = rig("drift-shown");
    rig.at(LATER);
    let plain = runs("web", "1.0", Some("deb"));
    assert_eq!(shown_after(&rig, &daemon(plain.clone())).await, None);

    let both = plain.clone().drifted(Some("2:1.0~rc1-1"), Some("1.0-1"));
    assert_eq!(
        shown_after(&rig, &daemon(both)).await,
        Some(agent::DriftReport {
            found: Some("2:1.0~rc1-1".to_owned()),
            recorded: Some("1.0-1".to_owned()),
        })
    );
    let removed = plain.clone().drifted(None, Some("1.0-1"));
    assert_eq!(
        shown_after(&rig, &daemon(removed)).await,
        Some(agent::DriftReport {
            found: None,
            recorded: Some("1.0-1".to_owned()),
        })
    );
    let unrecorded = plain.clone().drifted(Some("1.0-2"), None);
    assert_eq!(
        shown_after(&rig, &daemon(unrecorded)).await,
        Some(agent::DriftReport {
            found: Some("1.0-2".to_owned()),
            recorded: None,
        })
    );
    assert_eq!(shown_after(&rig, &daemon(plain)).await, None);
}

/// Sends the heartbeat of `daemon` over gRPC and answers the drift `Hosts.Get` shows after it.
async fn shown_after(rig: &Rig, daemon: &Daemon) -> Option<agent::DriftReport> {
    grpc_beat(rig, daemon).await;
    shown(rig).await
}

/// Proves: a drift with a string outside a package version's grammar (empty, 129 bytes, a space)
/// is not shown, and the installers that heartbeat listed are still read and shown.
/// Catches: a bad version shown to an operator, or a bad drift that costs the host its list.
#[tokio::test]
async fn a_drift_that_breaks_the_grammar_is_not_shown() {
    let rig = rig("drift-grammar");
    rig.at(LATER);
    let long = "9".repeat(129);
    for (found, recorded) in [
        (Some(""), None),
        (Some(long.as_str()), None),
        (None, Some("1 0")),
        (Some("2.0"), Some("1/0")),
    ] {
        let one = runs("web", "1.0", Some("deb")).drifted(found, recorded);
        assert_eq!(
            shown_after(&rig, &daemon(one)).await,
            None,
            "{found:?} {recorded:?}"
        );
        assert_eq!(host(&rig, "d1").await.installers, [ARCHIVE, "deb"]);
    }
}

/// Proves: the drift of a service of one host is shown on that service of that host only, and a
/// drift of a service the host stopped listing is gone with it.
/// Catches: a drift shown on the wrong service or host.
#[tokio::test]
async fn a_drift_is_shown_on_its_own_service_and_host() {
    let rig = rig("drift-own");
    rig.at(LATER);
    let drifting = Daemon::new("d1", X86, &[ARCHIVE, "deb"])
        .running(runs("web", "1.0", Some("deb")).drifted(Some("2.0"), None))
        .running(runs("db", "1.0", Some("deb")));
    grpc_beat(&rig, &drifting).await;
    let other = Daemon::new("d2", X86, &[ARCHIVE, "deb"]).running(runs("web", "1.0", Some("deb")));
    grpc_beat(&rig, &other).await;
    let services = host(&rig, "d1")
        .await
        .heartbeat
        .expect("a heartbeat")
        .services;
    let drifts: Vec<(&str, bool)> = services
        .iter()
        .map(|one| (one.name.as_str(), one.drift.is_some()))
        .collect();
    assert_eq!(drifts, [("db", false), ("web", true)]);
    let services = host(&rig, "d2")
        .await
        .heartbeat
        .expect("a heartbeat")
        .services;
    assert_eq!(services[0].drift, None);
}

/// Proves: a heartbeat of a daemon without `target-v1` is not read for drift, as it is not for
/// installers, and the JSON route, which has no key for it, shows none.
/// Catches: a drift read from a daemon that did not announce it.
#[tokio::test]
async fn a_drift_is_read_only_from_a_target_v1_daemon() {
    let rig = rig("drift-old");
    rig.at(LATER);
    let mut old =
        Daemon::old("d1", X86).running(runs("web", "1.0", None).drifted(Some("2.0"), None));
    grpc_beat(&rig, &old).await;
    assert_eq!(shown(&rig).await, None);
    old.features.push("target-v1".to_owned());
    grpc_beat(&rig, &old).await;
    assert!(shown(&rig).await.is_some());
    json_beat(&rig, &old).await;
    assert!(
        shown(&rig).await.is_some(),
        "a heartbeat over JSON leaves what gRPC said"
    );
}
