use super::inputs::{HostTimes, host, pending, pxe_log};
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

/// Catches a log of the current name (`pxe serve: `) not read, a log of the earlier name
/// (`hostkeeper-pxe serve: `) no longer read, the two not read when mixed in one file (a log
/// kept across the rename), and a line with another program's prefix read as a stamped line.
#[test]
fn the_log_is_read_under_either_prefix_and_not_under_another() {
    let discover = |prefix: &str, secs: u64| {
        format!(
            "{prefix}{secs} dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, PXEClient, on the DHCP port: proxy offer sent\n"
        )
    };
    let seed = |prefix: &str, secs: u64| {
        format!(
            "{prefix}{secs} seed seed/52:54:00:4e:10:01/meta-data: fetched by 52:54:00:4e:10:01, host net-1: its install started\n"
        )
    };
    let current = format!(
        "{}{}",
        discover("pxe serve: ", 1100),
        seed("pxe serve: ", 1140)
    );
    assert_eq!(
        pxe_log(&current, &mac(), Mode::Proxy, HOST),
        (Some(1100), Some(1140))
    );
    let earlier = format!(
        "{}{}",
        discover("hostkeeper-pxe serve: ", 900),
        seed("hostkeeper-pxe serve: ", 940)
    );
    assert_eq!(
        pxe_log(&earlier, &mac(), Mode::Proxy, HOST),
        (Some(900), Some(940))
    );
    let mixed = format!("{earlier}{current}");
    assert_eq!(
        pxe_log(&mixed, &mac(), Mode::Proxy, HOST),
        (Some(900), Some(940))
    );
    let neither = format!(
        "{}{}",
        discover("other serve: ", 700),
        seed("pxe-serve: ", 740)
    );
    assert_eq!(pxe_log(&neither, &mac(), Mode::Proxy, HOST), (None, None));
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
/// a time that is not canonical JSON's string of digits read as one.
#[test]
fn a_pending_file_that_is_not_a_list_of_requests_is_refused() {
    let cases = [
        (
            "[]",
            "--pending: not a JSON object, as the operator API's canonical JSON",
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

/// Catches approved, the first heartbeat or serving read from another field (the last
/// heartbeat is not serving), and an absent or zero time (canonical JSON leaves a zero out) read as a time.
#[test]
fn the_host_gives_approved_the_first_heartbeat_and_serving() {
    let got = r#"{"host": {"nodeId": "net-1", "enrolledUnix": "1600", "firstHeartbeatUnix": "1615",
        "firstServingUnix": "1650", "lastHeartbeatUnix": "1900"}}"#;
    assert_eq!(
        host(got, HOST),
        Ok(HostTimes {
            approved: Some(1600),
            first_heartbeat: Some(1615),
            serving: Some(1650),
        })
    );
    let heard =
        r#"{"host": {"nodeId": "net-1", "enrolledUnix": "1600", "firstHeartbeatUnix": "1615"}}"#;
    assert_eq!(
        host(heard, HOST),
        Ok(HostTimes {
            approved: Some(1600),
            first_heartbeat: Some(1615),
            serving: None,
        })
    );
    let pending = r#"{"host": {"nodeId": "net-1", "enrolledUnix": "0"}}"#;
    assert_eq!(host(pending, HOST), Ok(HostTimes::default()));
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
            r#"{"host": {"nodeId": "net-1", "firstServingUnix": 1650}}"#,
            "--host: `firstServingUnix` is 1650, not unix seconds as a string of digits",
        ),
        (
            "\"net-1\"",
            "--host: not a JSON object, as the operator API's canonical JSON",
        ),
    ];
    for (text, why) in cases {
        assert_eq!(host(text, HOST), Err(refused(why)), "{text}");
    }
}
