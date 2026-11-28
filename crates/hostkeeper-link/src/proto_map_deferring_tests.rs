//! The deferrals of a running `verify` (`ServiceFacts::deferring`) through the conversions of
//! `proto_map`, the JSON facts and the bound of the heartbeat.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::json;

use super::heartbeat;
use crate::beat::{
    ANNOUNCED, Health, Heartbeat, MAX_HEARTBEAT_BYTES, OptionalFact, Platform, ServiceFacts,
    VerifyDeferrals, fit,
};
use crate::names::Reason;
use crate::ports::RecordedDeferrals;

fn facts(deferring: Option<VerifyDeferrals>) -> ServiceFacts {
    ServiceFacts {
        deferring,
        name: ServiceName::new("tierstore").expect("a service name"),
        version: Version::new("0.3.211").expect("a version"),
        health: Health::Healthy,
        updating: Some(Version::new("0.3.212").expect("a version")),
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: None,
        record_try: None,
        installer: None,
        drift: None,
        local_halt: None,
        health_reason: None,
        health_body: None,
        restarts: None,
    }
}

fn deferring(reason: Option<&str>, detail: Option<&str>) -> VerifyDeferrals {
    VerifyDeferrals {
        attempt: 4,
        count: 3,
        reason: reason.map(|text| Reason::new(text).expect("a reason")),
        detail: detail.map(str::to_owned),
        since_unix: Some(1_790_000_000),
    }
}

fn beat(services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: None,
        ssh: None,
        prerequisites: None,
        bmc: None,
        serial_console: None,
        maintenance: None,
        health: None,
    }
}

/// Proves the gRPC service entry carries a deferring `verify` as its `step`: the step `verify`,
/// its run, and the count, last reason, last detail and time of the first deferral; deferrals
/// with no reason or detail carry none, and facts with none carry no step.
///
/// Catches: what a host waits for dropped on the gRPC route, or a missing reason sent as an
/// empty string.
#[test]
fn the_grpc_service_carries_a_deferring_verify_as_its_step() {
    let sent = heartbeat(&beat(vec![
        facts(Some(deferring(Some("provider_busy"), Some("read refused")))),
        facts(Some(deferring(None, None))),
        facts(None),
    ]))
    .expect("a request");
    let steps: Vec<_> = sent.services.iter().map(|s| s.step.clone()).collect();
    assert_eq!(
        steps[0],
        Some(pb::StepRun {
            name: "verify".to_owned(),
            attempt: 4,
            deferrals: Some(pb::StepDeferrals {
                count: 3,
                reason: Some("provider_busy".to_owned()),
                detail: Some("read refused".to_owned()),
                since_unix: Some(1_790_000_000),
            }),
        })
    );
    let bare = steps[1].as_ref().and_then(|step| step.deferrals.clone());
    assert_eq!(
        bare.map(|d| (d.count, d.reason, d.detail)),
        Some((3, None, None))
    );
    assert_eq!(steps[2], None);
}

/// Proves the facts as JSON name a deferring `verify` as `step` only when there is one, with the
/// wire's names.
///
/// Catches: a `step` key (or a null) on facts with no deferrals, which the bound would count.
#[test]
fn the_facts_as_json_name_the_step_only_when_deferring() {
    let named = facts(Some(deferring(Some("provider_busy"), Some("read refused")))).to_json();
    assert_eq!(
        named["step"],
        json!({
            "name": "verify",
            "attempt": 4,
            "deferrals": {
                "count": 3,
                "reason": "provider_busy",
                "detail": "read refused",
                "since_unix": 1_790_000_000_u64,
            },
        })
    );
    assert_eq!(facts(None).to_json().get("step"), None);
}

/// Proves the time of the first deferral is the heartbeat's time less the age the record gives, 0
/// for an age greater than that time, and none for an age that cannot be measured.
///
/// Catches: a time that underflows, or one made up for a stamp of another boot.
#[test]
fn the_first_deferral_is_dated_from_its_age() {
    let recorded = |age_secs| RecordedDeferrals {
        attempt: 2,
        count: 1,
        reason: None,
        detail: None,
        age_secs,
    };
    let at = |age_secs| VerifyDeferrals::at(recorded(age_secs), 1000).since_unix;
    assert_eq!(at(Some(400)), Some(600));
    assert_eq!(at(Some(1500)), Some(0));
    assert_eq!(at(None), None);
    let read = VerifyDeferrals::at(recorded(Some(0)), 1000);
    assert_eq!((read.attempt, read.count), (2, 1));
}

/// Proves the deferrals are an optional fact of their group: a heartbeat over its bound only for
/// a deferring `verify` loses them with the deferral group, and keeps the service and its update.
///
/// Catches: a heartbeat not sent at all because of what a host waits for.
#[test]
fn a_heartbeat_over_its_bound_drops_the_deferrals_with_the_deferral() {
    let long = "x".repeat(MAX_HEARTBEAT_BYTES);
    let fitted = fit(beat(vec![facts(Some(deferring(None, Some(&long))))])).expect("it fits");
    assert!(fitted.dropped.contains(&OptionalFact::DeferralDetail));
    let service = &fitted.beat.services[0];
    assert_eq!(service.deferring, None);
    assert!(service.updating.is_some());
}
