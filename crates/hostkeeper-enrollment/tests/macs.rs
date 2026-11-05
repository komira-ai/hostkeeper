//! MAC addresses: the spellings a MAC reads in and the one it is written in, which addresses
//! bind a host, the MACs a request claims, and the duplicate claim of a new request that claims a
//! MAC bound to an enrolled host.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    Claims, EnrollmentStore, NewRequest, PostError, RequestState, StoreError,
};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::MacAddress;
use support::{HostKey, NOW, TestCa, capture, decision, host_id, renewal, request, scratch};

fn mac(text: &str) -> MacAddress {
    MacAddress::parse(text).expect("a MAC")
}

/// A first enrollment of `key` from `machine_id`, claiming `addresses` and the BMC MAC `bmc`.
fn claiming(key: &HostKey, machine_id: &str, addresses: &[&str], bmc: &str) -> NewRequest {
    let mut out = request(key, machine_id);
    out.claims.addresses = addresses.iter().map(|a| (*a).to_owned()).collect();
    out.claims.bmc_mac = bmc.to_owned();
    out
}

/// A store in a scratch directory of its own.
fn store(name: &str) -> (EnrollmentStore, hostkeeper_testkit::Scratch) {
    let dir = scratch(name);
    let store = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("open");
    (store, dir)
}

/// Posts `new` and approves it as `host`.
fn enrolled(store: &EnrollmentStore, new: NewRequest, host: &str) {
    let id = store.post(new, NOW).expect("post").request_id;
    store
        .approve(&id, &id, &decision(host, false))
        .expect("approve");
}

/// Catches: `aa:bb:..` and `AA-BB-..` read as two machines, a MAC written in more than one
/// spelling, and text that is not six octets (five or seven octets, mixed or misplaced
/// separators, a non-hex digit, a sign, a multi-byte character) read as a MAC.
#[test]
fn a_mac_reads_in_every_spelling_and_is_written_in_one() {
    let one = mac("0c:c4:7a:1f:ab:02");
    for text in [
        "0C:C4:7A:1F:AB:02",
        "0c-c4-7a-1f-ab-02",
        "0C-c4-7A-1f-AB-02",
        "0CC47A1FAB02",
        "0cc47a1fab02",
    ] {
        assert_eq!(mac(text), one, "{text}");
    }
    assert_eq!(one.to_string(), "0c:c4:7a:1f:ab:02");
    assert_eq!(format!("{one:?}"), "MacAddress(0c:c4:7a:1f:ab:02)");
    assert_eq!(one.octets(), [0x0c, 0xc4, 0x7a, 0x1f, 0xab, 0x02]);
    for text in [
        "",
        "0c:c4:7a:1f:ab",
        "0c:c4:7a:1f:ab:02:03",
        "0c:c4:7a-1f:ab:02",
        "0c:c4:7a:1f:ab:0g",
        "0c.c4.7a.1f.ab.02",
        "0cc:4:7a:1f:ab:02",
        "0c:c47:a1:fab:02:",
        "0cc47a1fab0",
        "0cc47a1fab0233",
        "0cc47a1fab+2",
        "0cc47a1fab\u{e9}",
        "0c:c4:7a:1f:ab:\u{e9}",
    ] {
        let refused = MacAddress::parse(text).expect_err("not a MAC");
        assert_eq!(
            (refused.what, refused.value.as_str(), refused.grammar),
            ("MAC address", text, MacAddress::GRAMMAR),
            "{text}"
        );
    }
}

/// Catches: a made-up address (locally administered: a bridge, a veth, a virtual machine's) or a
/// multicast address treated as the maker's, which would bind every host that happens to share
/// it, and the all-zero address treated as one.
#[test]
fn only_a_universally_administered_unicast_mac_is_the_makers() {
    assert!(mac("0c:c4:7a:00:00:01").is_universal_unicast());
    assert!(!mac("02:42:ac:11:00:02").is_universal_unicast());
    assert!(!mac("01:00:5e:00:00:fb").is_universal_unicast());
    assert!(!mac("03:00:00:00:00:01").is_universal_unicast());
    assert!(!mac("00:00:00:00:00:00").is_universal_unicast());
}

/// Catches: an IPoIB or other non-MAC address read as a MAC, one interface's MAC claimed twice
/// in two spellings, and a BMC MAC that is not a MAC used as one.
#[test]
fn the_macs_a_host_claims_are_its_mac_shaped_addresses_and_its_bmc_mac() {
    let claims = Claims {
        addresses: vec![
            "0c:c4:7a:00:00:02".to_owned(),
            "80:00:02:08:fe:80:00:00:00:00:00:00:0c:c4:7a:00:00:02".to_owned(),
            "0C-C4-7A-00-00-02".to_owned(),
            "0c:c4:7a:00:00:01".to_owned(),
        ],
        bmc_mac: "0C:C4:7A:00:00:FF".to_owned(),
        ..Claims::default()
    };
    assert_eq!(
        claims.data_macs(),
        [mac("0c:c4:7a:00:00:01"), mac("0c:c4:7a:00:00:02")]
    );
    assert_eq!(claims.bmc(), Some(mac("0c:c4:7a:00:00:ff")));
    assert_eq!(
        claims.all_macs(),
        [
            mac("0c:c4:7a:00:00:01"),
            mac("0c:c4:7a:00:00:02"),
            mac("0c:c4:7a:00:00:ff")
        ]
    );
    let unreadable = Claims {
        bmc_mac: "not a mac".to_owned(),
        ..Claims::default()
    };
    assert_eq!(
        (unreadable.bmc(), unreadable.all_macs()),
        (None, Vec::new())
    );
}

/// Catches: a second machine (or the same machine with a wiped disk and a new key) claiming a
/// data MAC or the BMC MAC of an enrolled host taken as an ordinary pending request, in any
/// spelling, with no alert; and the flag kept on the record of the request across a reopen.
#[test]
fn a_new_key_claiming_a_bound_mac_is_a_duplicate_claim() {
    let (store, dir) = store("macs-bound");
    enrolled(
        &store,
        claiming(
            &HostKey::new(),
            "m-1",
            &["0c:c4:7a:00:00:02"],
            "0c:c4:7a:00:00:01",
        ),
        "web-1",
    );
    for (n, (addresses, bmc)) in [
        (&["0C-C4-7A-00-00-02"][..], ""),
        (&["0c:c4:7a:00:00:09"][..], "0C:C4:7A:00:00:01"),
        (&["0c:c4:7a:00:00:01"][..], ""),
    ]
    .into_iter()
    .enumerate()
    {
        let key = HostKey::new();
        let (posted, events) =
            capture(|| store.post(claiming(&key, &format!("m-new-{n}"), addresses, bmc), NOW));
        let posted = posted.expect("posted");
        assert_eq!(
            posted.state,
            RequestState::DuplicateClaim,
            "{addresses:?} {bmc}"
        );
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(
            events[0].get("reason").map(String::as_str),
            Some("a MAC is bound to an enrolled host")
        );
        let reopened = EnrollmentStore::open(Arc::new(RealFs), dir.path()).expect("reopen");
        let read = reopened
            .request(&key.key_hash())
            .expect("read")
            .expect("held");
        assert_eq!(read.state, RequestState::DuplicateClaim);
    }
    assert_eq!(
        store
            .bound_macs()
            .expect("read")
            .into_iter()
            .collect::<Vec<_>>(),
        [mac("0c:c4:7a:00:00:01"), mac("0c:c4:7a:00:00:02")]
    );
}

/// Catches: a MAC bound by a request no operator approved, by a host that is denied, by a key a
/// replacement took the host id from, or by a made-up address shared by two hosts; and a
/// renewal of the enrolled host flagged by its own MACs. None of them binds.
#[test]
fn only_an_enrolled_hosts_own_maker_macs_bind() {
    let (store, _dir) = store("macs-unbound");
    let waiting = HostKey::new();
    store
        .post(claiming(&waiting, "m-0", &["0c:c4:7a:00:00:10"], ""), NOW)
        .expect("pending");
    let other = HostKey::new();
    let posted = store
        .post(claiming(&other, "m-1", &["0c:c4:7a:00:00:10"], ""), NOW)
        .expect("posted");
    assert_eq!(
        posted.state,
        RequestState::Pending,
        "a pending request binds nothing"
    );

    let renewing = HostKey::new();
    enrolled(
        &store,
        claiming(
            &renewing,
            "m-2",
            &["0c:c4:7a:00:00:20", "02:42:ac:11:00:02"],
            "",
        ),
        "web-2",
    );
    let ca = TestCa::new("macs-unbound-ca");
    store.issue(&ca.ca, &renewing.key_hash()).expect("issued");
    let shared = store
        .post(
            claiming(&HostKey::new(), "m-3", &["02:42:ac:11:00:02"], ""),
            NOW,
        )
        .expect("posted");
    assert_eq!(
        shared.state,
        RequestState::Pending,
        "a made-up address binds nothing"
    );
    let mut renewal = renewal(&renewing, "m-2", "web-2");
    renewal.claims.addresses = vec!["0c:c4:7a:00:00:20".to_owned()];
    let renewed = store.post(renewal, NOW).expect("a renewal");
    assert_eq!(
        (renewed.state, renewed.round, renewed.reposted),
        (RequestState::Pending, 1, false),
        "a renewal keeps its own MACs"
    );

    let denied = HostKey::new();
    enrolled(
        &store,
        claiming(&denied, "m-4", &["0c:c4:7a:00:00:40"], ""),
        "web-4",
    );
    store.deny(&host_id("web-4")).expect("denied");
    let after_deny = store
        .post(
            claiming(&HostKey::new(), "m-5", &["0c:c4:7a:00:00:40"], ""),
            NOW,
        )
        .expect("posted");
    assert_eq!(
        after_deny.state,
        RequestState::Pending,
        "a denied host binds nothing"
    );

    let old = HostKey::new();
    enrolled(
        &store,
        claiming(&old, "m-6", &["0c:c4:7a:00:00:60"], ""),
        "web-6",
    );
    let new = HostKey::new();
    let id = store
        .post(claiming(&new, "m-7", &["0c:c4:7a:00:00:70"], ""), NOW)
        .expect("posted")
        .request_id;
    store
        .approve(&id, &id, &decision("web-6", true))
        .expect("replaced");
    let after_replace = store
        .post(
            claiming(&HostKey::new(), "m-8", &["0c:c4:7a:00:00:60"], ""),
            NOW,
        )
        .expect("posted");
    assert_eq!(
        after_replace.state,
        RequestState::Pending,
        "a replaced key binds nothing"
    );
    let bound: Vec<String> = store
        .bound_macs()
        .expect("read")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(bound, ["0c:c4:7a:00:00:20", "0c:c4:7a:00:00:70"]);
}

/// Catches: a host whose request file is gone read as binding nothing, which would let any MAC
/// through unflagged; the post fails as a store failure, and nothing is written.
#[test]
fn a_host_whose_request_is_missing_fails_the_post() {
    let (store, dir) = store("macs-missing");
    let key = HostKey::new();
    enrolled(
        &store,
        claiming(&key, "m-1", &["0c:c4:7a:00:00:02"], ""),
        "web-1",
    );
    let file = dir
        .path()
        .join("requests")
        .join(format!("{}.pb", key.key_hash()));
    std::fs::remove_file(file).expect("removed");
    let err = store
        .post(
            claiming(&HostKey::new(), "m-2", &["0c:c4:7a:00:00:09"], ""),
            NOW,
        )
        .expect_err("refused");
    assert!(
        matches!(&err, PostError::Store(StoreError::Corrupt { reason, .. }) if reason == "the host's request is missing"),
        "{err:?}"
    );
    assert_eq!(err.token(), None);
    let left = std::fs::read_dir(dir.path().join("requests"))
        .expect("list")
        .count();
    assert_eq!(left, 0);
}
