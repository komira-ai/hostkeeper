//! The rules a service identities list keeps, and what the coordinator keeps, shows and forgets of
//! it.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;

use super::{FACT, Identities, MAX_ERROR, MAX_SERVICES, kept, show};
use crate::fleet::Fleet;
use crate::testutil::id;

fn report(service: &str, error: Option<&str>) -> agent::ServiceIdentityReport {
    agent::ServiceIdentityReport {
        service: service.to_owned(),
        not_after_unix: Some(7),
        last_error: error.map(str::to_owned),
    }
}

fn named(names: &[&str]) -> Vec<agent::ServiceIdentityReport> {
    names.iter().map(|name| report(name, None)).collect()
}

/// Proves: a list of no service and a list of exactly 16 are good and kept as they are; one of 17
/// is not.
///
/// Catches: a bound off by one, and an empty list refused.
#[test]
fn a_list_names_at_most_16_services() {
    let many = |n: usize| -> Vec<agent::ServiceIdentityReport> {
        (0..n).map(|k| report(&format!("svc-{k}"), None)).collect()
    };
    assert_eq!(MAX_SERVICES, 16);
    assert_eq!(kept(&[]), Some(Vec::new()));
    assert_eq!(kept(&many(16)), Some(many(16)));
    assert_eq!(kept(&many(17)), None);
}

/// Proves: a name that is no service name (empty, with a slash or a space, beginning with `-`,
/// 256 bytes) breaks the rules of the whole list, wherever it is, and a service named twice does
/// too, wherever the two are; names that differ only by case are two services.
///
/// Catches: only the first name checked, and a service reported twice kept.
#[test]
fn every_name_is_a_service_name_and_no_name_is_twice() {
    for bad in ["", "a/b", "a b", "-x", ".x", &"x".repeat(256)] {
        assert_eq!(kept(&named(&["good", bad])), None, "{bad:?} last");
        assert_eq!(kept(&named(&[bad, "good"])), None, "{bad:?} first");
    }
    assert_eq!(kept(&named(&["a", "b", "a"])), None);
    assert_eq!(kept(&named(&["a", "a"])), None);
    assert!(kept(&named(&["a", "A", "a.b", "a_b", "a-b", "0"])).is_some());
    assert!(kept(&named(&[&"x".repeat(255)])).is_some());
}

/// Proves: a `last_error` of up to 256 bytes is kept whole and a longer one is cut to 256 bytes,
/// at a character boundary when the 256th byte is inside a character; the other fields and an
/// absent error are kept as they are.
///
/// Catches: an error text kept without a bound, a cut that splits a character, and one that
/// drops the other fields.
#[test]
fn an_error_is_cut_to_256_bytes_at_a_character_boundary() {
    assert_eq!(MAX_ERROR, 256);
    let text = |list: Vec<agent::ServiceIdentityReport>| {
        kept(&list).expect("a good list")[0].last_error.clone()
    };
    let with = |error: &str| vec![report("a", Some(error))];
    assert_eq!(text(with(&"e".repeat(256))), Some("e".repeat(256)));
    assert_eq!(text(with(&"e".repeat(257))), Some("e".repeat(256)));
    // 3 bytes each: 256 is inside the 86th character, so 85 of them are kept.
    assert_eq!(
        text(with(&"\u{20ac}".repeat(100))),
        Some("\u{20ac}".repeat(85))
    );
    assert_eq!(text(vec![report("a", None)]), None);
    assert_eq!(text(with("")), Some(String::new()));
    assert_eq!(
        kept(&with("x")).expect("a good list")[0].not_after_unix,
        Some(7)
    );
}

/// Proves: the list kept for a host is shown in the heartbeat of its row, a host that reported none
/// shows none and its row's heartbeat stays as it was, and once the fleet forgets the host the
/// list is gone.
///
/// Catches: a list shown for a host that did not report it, and one that outlives its host.
#[test]
fn a_list_is_shown_for_its_host_and_forgotten_with_it() {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let host = id("node-01");
    let list = named(&["js-agent"]);
    fleet.service_identities = Identities::default();
    fleet
        .service_identities
        .0
        .insert(host.clone(), list.clone());
    let mut row = operator::Host::default();
    show(&fleet, &id("node-02"), &mut row);
    assert_eq!(row.heartbeat, None);
    show(&fleet, &host, &mut row);
    assert_eq!(row.heartbeat.expect("a heartbeat").service_identities, list);

    fleet.service_identities.forget(&host);
    let mut row = operator::Host::default();
    show(&fleet, &host, &mut row);
    assert_eq!(row.heartbeat, None);
    assert_eq!(FACT, "service_identities");
}
