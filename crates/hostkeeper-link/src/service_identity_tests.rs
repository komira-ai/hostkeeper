//! The services' identities report: its text bound, its mapping to `hostkeeper.agent.v1`, and its
//! JSON form that the size bound measures.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde_json::json;

use super::{MAX_IDENTITY_ERROR, ServiceIdentityReport};
use crate::beat::{ANNOUNCED, Heartbeat, Platform};
use crate::proto_map::heartbeat;

fn beat(reports: Vec<ServiceIdentityReport>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        operator_keys: None,
        prerequisites: None,
        ssh: None,
        service_identities: reports,
    }
}

fn reports() -> Vec<ServiceIdentityReport> {
    vec![
        ServiceIdentityReport::new("js-agent".to_owned(), Some(1_900_028_800), None),
        ServiceIdentityReport::new(
            "js-worker".to_owned(),
            None,
            Some("service_not_assigned".to_owned()),
        ),
    ]
}

/// Catches a report mapped to the wrong field, or an absent end or error sent as an empty one.
#[test]
fn the_reports_go_out_as_service_identities_field_for_field() {
    let request = heartbeat(&beat(reports())).expect("a request");
    assert_eq!(
        request.service_identities,
        vec![
            pb::ServiceIdentityReport {
                service: "js-agent".to_owned(),
                not_after_unix: Some(1_900_028_800),
                last_error: None,
            },
            pb::ServiceIdentityReport {
                service: "js-worker".to_owned(),
                not_after_unix: None,
                last_error: Some("service_not_assigned".to_owned()),
            },
        ]
    );
    let none = heartbeat(&beat(Vec::new())).expect("a request");
    assert_eq!(none.service_identities, vec![]);
}

/// Catches the size measure ignoring the report: its JSON names each service with its end and
/// error, and a heartbeat with none has no such key.
#[test]
fn the_json_form_counts_the_reports_and_a_host_with_none_has_no_key() {
    let json = beat(reports()).to_json();
    assert_eq!(
        json.get("service_identities"),
        Some(&json!([
            {"service": "js-agent", "not_after_unix": 1_900_028_800u64, "last_error": null},
            {"service": "js-worker", "not_after_unix": null, "last_error": "service_not_assigned"},
        ]))
    );
    assert_eq!(beat(Vec::new()).to_json().get("service_identities"), None);
}

/// Catches an error longer than the bound the coordinator reads being sent whole, or cut inside a
/// character: the cut is at a character boundary, at most the bound.
#[test]
fn a_last_error_is_cut_to_the_bound_at_a_character_boundary() {
    let long = "a".repeat(MAX_IDENTITY_ERROR + 10);
    let report = ServiceIdentityReport::new("svc".to_owned(), None, Some(long));
    assert_eq!(report.last_error, Some("a".repeat(MAX_IDENTITY_ERROR)));
    // A two-byte character straddling the bound is dropped whole.
    let straddling = format!("{}é", "a".repeat(MAX_IDENTITY_ERROR - 1));
    let report = ServiceIdentityReport::new("svc".to_owned(), None, Some(straddling));
    assert_eq!(report.last_error, Some("a".repeat(MAX_IDENTITY_ERROR - 1)));
    let exact = "é".repeat(MAX_IDENTITY_ERROR / 2);
    let report = ServiceIdentityReport::new("svc".to_owned(), None, Some(exact.clone()));
    assert_eq!(report.last_error, Some(exact));
}
