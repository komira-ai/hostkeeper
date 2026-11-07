use super::inputs::{Serving, admin_status, host, pending, pxe_log};
use super::{Error, Mac, Mode};

const HOST: &str = "net-1";

fn mac() -> Mac {
    "52:54:00:4e:10:01".parse().expect("a MAC")
}

fn refused(why: &str) -> Error {
    Error::Refused(why.to_owned())
}

/// The log of two runs of `serve` as the end-to-end test keeps it, prefixed as `serve` writes
/// it to standard error, with another MAC, a MAC that begins as this one does, the router's
/// lines of a later run, a line of another program and lines that are not stamped mixed in.
const LOG: &str = "\
hostkeeper-pxe serve: 1000 started in proxy mode as uid 65534 gid 65534, answering as 192.0.2.10
hostkeeper-pxe serve: 1005 dhcp: DHCPDISCOVER from 52:54:00:4e:10:02, architecture 7, PXEClient, on the DHCP port: proxy offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)
hostkeeper-pxe serve: 1009 dhcp: DHCPDISCOVER from 52:54:00:4e:10:011, architecture 7, PXEClient, on the DHCP port: no answer
hostkeeper-pxe serve: 1010 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: proxy offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)
hostkeeper-pxe serve: 1012 dhcp: DHCPREQUEST from 52:54:00:4e:10:01, architecture 7, PXEClient, on the proxyDHCP port: proxy ACK sent, boot file hostkeeper/shimx64.efi (linux-x86_64)
hostkeeper-pxe serve: 1011 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: proxy offer sent, boot file hostkeeper/shimx64.efi (linux-x86_64)
hostkeeper-pxe serve: 1050 seed seed/52:54:00:4e:10:02/meta-data: fetched by 52:54:00:4e:10:02, host net-2: its install started, and it leaves the answer set for the rest of the run
hostkeeper-pxe serve: 1060 seed seed/52:54:00:4e:10:01/meta-data: fetched by 52:54:00:4e:10:01, host net-1: its install started, and it leaves the answer set for the rest of the run
hostkeeper-pxe serve: 1300 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: no answer: not in the answer set: the record of host net-1: its seed was fetched in this run, so its install started
hostkeeper-pxe serve: stopped without a stamp
the guest's console said something
hostkeeper-pxe serve: 2000 started in router mode as uid 65534 gid 65534, answering as 192.0.2.10
2003 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: its architecture is kept for its REQUEST
2004 dhcp: DHCPREQUEST from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: it takes 192.0.2.50
";

/// Catches power-on read from another MAC, from a MAC that only begins as this one, from the
/// other DHCP message, or from a line that is not its first (the log is not in time order: the
/// earliest is taken); and the install's start read from another host's seed or from the
/// `no answer` line that also says "its install started".
#[test]
fn the_log_gives_the_first_power_on_of_the_mac_in_its_mode_and_its_seed_fetch() {
    assert_eq!(
        pxe_log(LOG, &mac(), Mode::Proxy, HOST),
        (Some(1010), Some(1060))
    );
    assert_eq!(
        pxe_log(LOG, &mac(), Mode::Router, HOST),
        (Some(1012), Some(1060))
    );
}

/// Catches a seed fetched as another host read as this record's install, lines without the
/// prefix not read, and a missing event read as anything but `None`.
#[test]
fn a_log_without_the_event_gives_none() {
    assert_eq!(
        pxe_log(LOG, &mac(), Mode::Proxy, "net-9"),
        (Some(1010), None)
    );
    assert_eq!(pxe_log("", &mac(), Mode::Proxy, HOST), (None, None));
    let bare = "\
2004 dhcp: DHCPREQUEST from 52:54:00:4e:10:01, architecture 11, HTTPClient, on the DHCP port: it takes 192.0.2.50
x2005 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 11, HTTPClient, on the DHCP port: proxy offer sent
+2006 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 11, HTTPClient, on the DHCP port: proxy offer sent
99999999999999999999 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 11, HTTPClient, on the DHCP port: proxy offer sent
 2007 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 11, HTTPClient, on the DHCP port: proxy offer sent
2008
";
    assert_eq!(
        pxe_log(bare, &mac(), Mode::Router, HOST),
        (Some(2004), None)
    );
    assert_eq!(pxe_log(bare, &mac(), Mode::Proxy, HOST), (None, None));
}

/// Catches a MAC flag in upper case not matched to the log's lower case, and each malformed
/// MAC accepted.
#[test]
fn a_mac_is_six_pairs_of_hex_digits_read_in_lower_case() {
    let upper: Mac = "52:54:00:4E:10:01".parse().expect("a MAC");
    assert_eq!(upper, mac());
    assert_eq!(upper.to_string(), "52:54:00:4e:10:01");
    for bad in [
        "52:54:00:4e:10",
        "52:54:00:4e:10:01:02",
        "52:54:00:4e:10:0g",
        "52:54:00:4e:10:001",
        "52-54-00-4e-10-01",
        "",
    ] {
        assert_eq!(
            bad.parse::<Mac>(),
            Err(format!(
                "`{bad}` is not a MAC address: six pairs of hex digits, separated by colons"
            ))
        );
    }
}

/// Catches pending read from a request of another record, from a later request than the
/// first, or from one with no time.
#[test]
fn pending_is_the_first_seen_time_of_the_records_request() {
    let list = r#"{"requests": [
        {"requestId": "a", "recordHostId": "net-2", "firstSeenUnix": "1100"},
        {"requestId": "b", "recordHostId": "net-1", "firstSeenUnix": "1500"},
        {"requestId": "c", "recordHostId": "net-1"},
        {"requestId": "d", "recordHostId": "net-1", "firstSeenUnix": "1400"},
        {"requestId": "e", "recordHostId": "net-1", "firstSeenUnix": "0"},
        {"requestId": "f", "recordHostId": "net-1", "firstSeenUnix": "1450"},
        {"requestId": "g"},
        "not a request"
    ]}"#;
    assert_eq!(pending(list, HOST), Ok(Some(1400)));
    assert_eq!(pending(list, "net-3"), Ok(None));
    assert_eq!(pending("{}", HOST), Ok(None));
}

/// Catches a file that is not what `enrollment list` prints read as a list with no request, and
/// a time that is not ctl's string of digits read as one.
#[test]
fn a_pending_file_that_is_not_a_list_of_requests_is_refused() {
    let cases = [
        (
            "[]",
            "--pending: not a JSON object, as `hostkeeper ctl --output json` prints",
        ),
        (
            r#"{"requests": {}}"#,
            "--pending: `requests` is not a list, as `enrollment list` prints",
        ),
        (
            r#"{"requests": [{"recordHostId": "net-1", "firstSeenUnix": 1400}]}"#,
            "--pending: `firstSeenUnix` is 1400, not unix seconds as a string of digits",
        ),
        (
            r#"{"requests": [{"recordHostId": "net-1", "firstSeenUnix": "-5"}]}"#,
            "--pending: `firstSeenUnix` is \"-5\", not unix seconds as a string of digits",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(pending(text, HOST), Err(refused(why)), "{text}");
    }
    let Err(Error::Refused(why)) = pending("not json", HOST) else {
        panic!("text that is not JSON is refused");
    };
    assert!(why.starts_with("--pending: not JSON: "), "{why}");
}

/// Catches approved or the first heartbeat read from another field, and an absent or zero time
/// (ctl leaves a zero out) read as a time.
#[test]
fn the_host_gives_approved_and_the_first_heartbeat() {
    let got = r#"{"host": {"nodeId": "net-1", "enrolledUnix": "1600", "firstHeartbeatUnix": "1615",
        "lastHeartbeatUnix": "1900"}}"#;
    assert_eq!(host(got, HOST), Ok((Some(1600), Some(1615))));
    let pending = r#"{"host": {"nodeId": "net-1", "enrolledUnix": "0"}}"#;
    assert_eq!(host(pending, HOST), Ok((None, None)));
}

/// Catches the file of another host, or one that is not what `hosts get` prints, read as this
/// one.
#[test]
fn a_host_file_of_another_host_or_shape_is_refused() {
    let cases = [
        (
            r#"{"hosts": []}"#,
            "--host: no `host`, as `hosts get` prints",
        ),
        (
            r#"{"host": "net-1"}"#,
            "--host: no `host`, as `hosts get` prints",
        ),
        (
            r#"{"host": {"nodeId": "net-2"}}"#,
            "--host: the host is net-2, not net-1",
        ),
        (
            r#"{"host": {"role": "web"}}"#,
            "--host: the host is not named, not net-1",
        ),
        (
            r#"{"host": {"nodeId": "net-1", "firstHeartbeatUnix": "soon"}}"#,
            "--host: `firstHeartbeatUnix` is \"soon\", not unix seconds as a string of digits",
        ),
        (
            "\"net-1\"",
            "--host: not a JSON object, as `hostkeeper ctl --output json` prints",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(host(text, HOST), Err(refused(why)), "{text}");
    }
}

/// `hostkeeper admin status` of a host with `api` installed and healthy, `db` failed and `web`
/// not installed, as the daemon prints it.
const STATUS: &str = "\
enrollment: enrolled
enrollment.host_id: net-1
link.last_success_unix: 1900
link.consecutive_failures: 0
trusted_root.age_secs: 86400
trusted_root.reason: none
services: 3
service[api].lifecycle: running
service[api].current: none
service[api].last.version: 1.0.0
service[api].last.outcome: succeeded
service[api].last.started_unix: 1640
service[api].last.finished_unix: 1700
service[cache].lifecycle: running
service[cache].current: none
service[cache].last.outcome: succeeded
service[cache].last.started_unix: 1630
service[cache].last.finished_unix: 1690
service[db].lifecycle: absent
service[db].current: none
service[db].last.outcome: failed
service[db].last.finished_unix: 1650
";

fn names(services: &[&str]) -> Vec<String> {
    services.iter().map(|s| (*s).to_owned()).collect()
}

/// Catches serving read from a service the flags do not name, from one whose last order
/// failed, from a later install than the first, or from its start rather than its end.
#[test]
fn serving_is_the_first_named_service_whose_last_order_succeeded() {
    let serving = |unix, service: &str| {
        Ok(Some(Serving {
            unix,
            service: service.to_owned(),
        }))
    };
    assert_eq!(admin_status(STATUS, &names(&["api"])), serving(1700, "api"));
    assert_eq!(
        admin_status(STATUS, &names(&["api", "cache"])),
        serving(1690, "cache")
    );
    assert_eq!(
        admin_status(STATUS, &names(&["cache", "api"])),
        serving(1690, "cache")
    );
    assert_eq!(
        admin_status(STATUS, &names(&["db", "web", "api"])),
        serving(1700, "api")
    );
    assert_eq!(admin_status(STATUS, &names(&["db", "web"])), Ok(None));
}

/// Catches a file that is not the admin status read as a host serving nothing, and a daemon
/// that prints no step times, or a time that is not one, read as serving.
#[test]
fn an_admin_status_without_its_lines_or_times_is_refused() {
    let older = "services: 1\nservice[api].last.outcome: succeeded\n";
    let none = "services: 1\nservice[api].last.outcome: succeeded\n\
                service[api].last.finished_unix: none\n";
    let cases = [
        (
            "{\"host\": {}}",
            "--admin-status: no `services` line, as `hostkeeper admin status` prints",
        ),
        (
            older,
            "--admin-status: no `service[api].last.finished_unix`: the host's hostkeeper \
             predates the step record's times",
        ),
        (
            none,
            "--admin-status: `service[api].last.finished_unix` is `none`, not unix seconds",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(
            admin_status(text, &names(&["api"])),
            Err(refused(why)),
            "{text}"
        );
    }
}
