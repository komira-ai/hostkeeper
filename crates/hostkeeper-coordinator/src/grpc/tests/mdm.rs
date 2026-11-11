//! `Mdm.ListDevices` end to end, through the router as the private listener serves it, by gRPC
//! and by the JSON gateway: every field of a record, paging, the page cap, a page token that is
//! no enrollment id, and a coordinator that runs no gate.

use bytes::Bytes;
use hostkeeper_mdm_gate::{CommandUuid, DeviceStore, FileDeviceStore, Phase, Sent};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};
use tonic::Code;

use super::support::{Served, call, decode, grpc_request, raw, served, served_built};
use crate::http::Surface;
use crate::mdm::tests::{devices_dir, flags_in, mdm_with, mdm_with_baseline, record};
use crate::mdm::{Devices, Mdm};
use crate::operator_mdm::MAX_DEVICE_PAGE;
use crate::testutil::{OPERATOR, Rig, ci_token, scratch};

/// The path of `Mdm.ListDevices`.
const LIST: &str = "/hostkeeper.operator.v1.Mdm/ListDevices";

/// A coordinator running a gate over `records`, and the directory of the records, which must
/// outlive it.
pub(super) fn gated(
    name: &str,
    records: &[hostkeeper_mdm_gate::DeviceRecord],
) -> (Served, Scratch) {
    let dir = scratch(&format!("{name}-gate"));
    let mdm = mdm_with(&dir, records);
    let served = served_built(name, |rig_dir, clock| {
        Rig::start(rig_dir, clock).with_mdm(Some(mdm))
    });
    (served, dir)
}

/// The answer to `ListDevices` of `limit` and `page_token` by gRPC with the operator's token.
async fn page(
    served: &Served,
    limit: u32,
    page_token: &str,
) -> (Code, String, proto::ListDevicesResponse) {
    let message = proto::ListDevicesRequest {
        limit,
        page_token: page_token.to_owned(),
    };
    let request = grpc_request(LIST, message, Some(OPERATOR));
    let (code, said, body) = call(&served.coordinator, Surface::Control, &None, request).await;
    let listed = body.map_or_else(proto::ListDevicesResponse::default, |body| decode(&body));
    (code, said, listed)
}

/// The same call by the gateway: the status and the JSON body.
async fn gateway(served: &Served, bearer: Option<&str>, body: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(LIST)
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let (status, _, bytes) = raw(&served.coordinator, Surface::Control, &None, request).await;
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

/// The enrollment ids of `listed`.
fn ids(listed: &proto::ListDevicesResponse) -> Vec<&str> {
    listed
        .devices
        .iter()
        .map(|device| device.enrollment_id.as_str())
        .collect()
}

/// A record of a device with every field set, and a record of one that has said nothing.
pub(super) fn two_records() -> [hostkeeper_mdm_gate::DeviceRecord; 2] {
    let mut full = record(
        "AAAA-FULL",
        Some("C02XK1ABJG5J"),
        Phase::Enrolled,
        Some(true),
    );
    full.udid = "AAAA-FULL-UDID".to_owned();
    full.authenticate_serial = Some("C02AUTHSER1".to_owned());
    full.enrollment = 2;
    full.enrolled_via_dep = Some(false);
    full.user_approved = Some(true);
    full.enrolled_since_unix = Some(1_799_999_000);
    full.last_checkin_unix = 1_800_000_100;
    full.checked_out_unix = Some(1_800_000_200);
    full.device_information_unix = Some(1_799_999_100);
    full.security_info_unix = Some(1_799_999_200);
    full.sent = vec![
        Sent {
            uuid: CommandUuid::parse("A1A1A1A1-2222-4333-8444-555555555555").expect("a uuid"),
            request_type: "SecurityInfo".to_owned(),
            sent_unix: 1_800_000_050,
            queued: true,
        },
        Sent {
            uuid: CommandUuid::parse("66666666-7777-4888-8999-000000000000").expect("a uuid"),
            request_type: "DeviceInformation".to_owned(),
            sent_unix: 1_800_000_060,
            queued: true,
        },
    ];
    full.last_error = Some("NanoHUB did not queue the command".to_owned());
    full.install = Some(hostkeeper_mdm_gate::InstallRecord {
        sha256: hostkeeper_mdm_gate::PkgDigest::parse(&"ab".repeat(32)).expect("a digest"),
        sends: 3,
        last: hostkeeper_mdm_gate::ProfileCommand::new(
            CommandUuid::parse("B2B2B2B2-2222-4333-8444-555555555555").expect("a uuid"),
            1_800_000_070,
        ),
    });
    let bare = record("BBBB-BARE", None, Phase::Authenticated, None);
    [full, bare]
}

/// Proves: the operator's token lists the gate's records by enrollment id, each with every field
/// of `MdmDevice` set from its record (the phase, the counts, the optional flags and times, the
/// unanswered commands with their UUIDs, the last error, the pkg's sends, last send and digest) and a field the record lacks absent,
/// not zero; `gate` is true; and the gateway answers the same device in canonical JSON, the
/// absent fields not shown.
///
/// Catches: a field of the record dropped or swapped on its way to the answer, an absent
/// optional shown as false or 0, and the gateway answering another device than gRPC.
#[tokio::test]
async fn the_operator_lists_every_field_of_a_device_over_grpc_and_the_gateway() {
    let (served, _gate) = gated("grpc-mdm-fields", &two_records());
    let (code, said, listed) = page(&served, 0, "").await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let full = proto::MdmDevice {
        enrollment_id: "AAAA-FULL".to_owned(),
        udid: "AAAA-FULL-UDID".to_owned(),
        serial: "C02XK1ABJG5J".to_owned(),
        authenticate_serial: "C02AUTHSER1".to_owned(),
        phase: proto::MdmPhase::Enrolled.into(),
        enrollments: 2,
        supervised: Some(true),
        enrolled_via_dep: Some(false),
        user_approved: Some(true),
        enrolled_since_unix: Some(1_799_999_000),
        last_checkin_unix: 1_800_000_100,
        checked_out_unix: Some(1_800_000_200),
        device_information_unix: Some(1_799_999_100),
        security_info_unix: Some(1_799_999_200),
        unanswered: vec![
            proto::MdmCommand {
                request_type: "SecurityInfo".to_owned(),
                command_uuid: "A1A1A1A1-2222-4333-8444-555555555555".to_owned(),
                sent_unix: 1_800_000_050,
            },
            proto::MdmCommand {
                request_type: "DeviceInformation".to_owned(),
                command_uuid: "66666666-7777-4888-8999-000000000000".to_owned(),
                sent_unix: 1_800_000_060,
            },
        ],
        last_error: "NanoHUB did not queue the command".to_owned(),
        install_sends: 3,
        install_last_sent_unix: Some(1_800_000_070),
        install_sha256: "ab".repeat(32),
        // This coordinator's gate has no baseline: no state is shown.
        baseline_state: proto::MdmBaselineState::Unspecified.into(),
        baseline_reason: String::new(),
    };
    let bare = proto::MdmDevice {
        enrollment_id: "BBBB-BARE".to_owned(),
        udid: "BBBB-BARE".to_owned(),
        phase: proto::MdmPhase::Authenticated.into(),
        last_checkin_unix: 1_800_000_000,
        ..proto::MdmDevice::default()
    };
    let want = proto::ListDevicesResponse {
        devices: vec![full, bare],
        next_page_token: String::new(),
        gate: true,
    };
    assert_eq!(listed, want);
    assert_eq!(listed.devices[1].supervised, None);
    assert_eq!(listed.devices[1].checked_out_unix, None);
    assert_eq!(listed.devices[1].install_last_sent_unix, None);

    let (status, json) = gateway(&served, Some(OPERATOR), "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "devices": [
                {
                    "enrollmentId": "AAAA-FULL",
                    "udid": "AAAA-FULL-UDID",
                    "serial": "C02XK1ABJG5J",
                    "authenticateSerial": "C02AUTHSER1",
                    "phase": "MDM_PHASE_ENROLLED",
                    "enrollments": 2,
                    "supervised": true,
                    "enrolledViaDep": false,
                    "userApproved": true,
                    "enrolledSinceUnix": "1799999000",
                    "lastCheckinUnix": "1800000100",
                    "checkedOutUnix": "1800000200",
                    "deviceInformationUnix": "1799999100",
                    "securityInfoUnix": "1799999200",
                    "unanswered": [
                        {
                            "requestType": "SecurityInfo",
                            "commandUuid": "A1A1A1A1-2222-4333-8444-555555555555",
                            "sentUnix": "1800000050"
                        },
                        {
                            "requestType": "DeviceInformation",
                            "commandUuid": "66666666-7777-4888-8999-000000000000",
                            "sentUnix": "1800000060"
                        }
                    ],
                    "lastError": "NanoHUB did not queue the command",
                    "installSends": 3,
                    "installLastSentUnix": "1800000070",
                    "installSha256": "abababababababababababababababababababababababababababababababab"
                },
                {
                    "enrollmentId": "BBBB-BARE",
                    "udid": "BBBB-BARE",
                    "phase": "MDM_PHASE_AUTHENTICATED",
                    "lastCheckinUnix": "1800000000"
                }
            ],
            "gate": true
        })
    );
}

/// Proves: a coordinator whose gate has an enrollment baseline for the host group of an enrolled
/// Mac lists that Mac with the baseline state the gate says (`pending`: Screen Sharing is due and
/// has not been answered), by gRPC and by the gateway, and the device the gate has no baseline
/// for (one that is not enrolled) with no state.
///
/// Catches: `Mdm.ListDevices` that leaves the baseline columns at their defaults for a gate that
/// has a baseline, or shows a state for a device the baseline does not cover.
#[tokio::test]
async fn a_device_under_a_baseline_is_listed_with_its_state_over_grpc_and_the_gateway() {
    let dir = scratch("grpc-mdm-baseline-gate");
    let mdm = mdm_with_baseline(&dir, &two_records(), "build");
    let served = served_built("grpc-mdm-baseline", |rig_dir, clock| {
        Rig::start(rig_dir, clock).with_mdm(Some(mdm))
    });
    let (code, said, listed) = page(&served, 0, "").await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let states: Vec<(&str, i32, &str)> = listed
        .devices
        .iter()
        .map(|d| {
            (
                d.enrollment_id.as_str(),
                d.baseline_state,
                d.baseline_reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            ("AAAA-FULL", i32::from(proto::MdmBaselineState::Pending), ""),
            (
                "BBBB-BARE",
                i32::from(proto::MdmBaselineState::Unspecified),
                ""
            ),
        ]
    );

    let (status, json) = gateway(&served, Some(OPERATOR), "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["devices"][0]["baselineState"],
        "MDM_BASELINE_STATE_PENDING"
    );
    assert!(json["devices"][0].get("baselineReason").is_none());
    assert!(json["devices"][1].get("baselineState").is_none());
}

/// Proves: a limit of 1 over three devices gives three pages of one, each page but the last
/// naming the enrollment id of its device as the token of the next, the last page naming none;
/// a limit of 2 gives two and one; and a gate with no record answers no device with `gate`
/// true.
///
/// Catches: a page that repeats or skips a device, a token that is not the last id of the
/// page, a last page that offers one more, and an empty gate taken for no gate.
#[tokio::test]
async fn devices_are_paged_by_enrollment_id() {
    let records = [
        record("CCCC-3", None, Phase::Enrolled, None),
        record("AAAA-1", None, Phase::Enrolled, None),
        record("BBBB-2", None, Phase::CheckedOut, None),
    ];
    let (served, _gate) = gated("grpc-mdm-paging", &records);
    let mut token = String::new();
    let mut seen = Vec::new();
    for want_token in ["AAAA-1", "BBBB-2", ""] {
        let (code, said, listed) = page(&served, 1, &token).await;
        assert_eq!((code, said.as_str()), (Code::Ok, ""));
        assert_eq!(listed.devices.len(), 1);
        assert_eq!(listed.next_page_token, want_token);
        assert!(listed.gate);
        seen.push(listed.devices[0].enrollment_id.clone());
        token = listed.next_page_token;
    }
    assert_eq!(seen, ["AAAA-1", "BBBB-2", "CCCC-3"]);

    let (_, _, first) = page(&served, 2, "").await;
    assert_eq!(
        (ids(&first), first.next_page_token.as_str()),
        (vec!["AAAA-1", "BBBB-2"], "BBBB-2")
    );
    let (_, _, rest) = page(&served, 2, &first.next_page_token).await;
    assert_eq!(
        (ids(&rest), rest.next_page_token.as_str()),
        (vec!["CCCC-3"], "")
    );
    let (_, _, past) = page(&served, 2, "CCCC-3").await;
    assert_eq!((past.devices.len(), past.next_page_token.as_str()), (0, ""));

    let (empty, _gate) = gated("grpc-mdm-empty", &[]);
    let (_, _, listed) = page(&empty, 0, "").await;
    assert_eq!(
        listed,
        proto::ListDevicesResponse {
            gate: true,
            ..Default::default()
        }
    );
}

/// Proves: a limit of 0 and a limit of 1000 both give a page of [`MAX_DEVICE_PAGE`] devices
/// when 257 are held, whose token is the id of the 256th; a limit of 255 gives 255; and the
/// token leads to the one device left.
///
/// Catches: a limit of 0 read as no device, a limit over the cap honoured, and a cap that
/// drops or repeats the device at its edge.
#[tokio::test]
async fn a_page_holds_at_most_the_cap_and_a_limit_of_zero_is_the_cap() {
    let dir = scratch("grpc-mdm-cap-gate");
    let store = FileDeviceStore::open(
        std::sync::Arc::new(hostkeeper_enrollment::fs::RealFs),
        &devices_dir(&dir),
    )
    .expect("a store");
    for n in 0..=MAX_DEVICE_PAGE {
        store
            .save(&record(&format!("D{n:04}"), None, Phase::Enrolled, None))
            .expect("saved");
    }
    let flags = flags_in(&dir);
    let mdm = Mdm::open(
        &flags,
        Devices::Files(dir.path()),
        crate::enrollment::tests::support::FixedClock::at(1),
        crate::enrollment::os_random(),
        &crate::mdm_setup::MdmSetup::volatile(),
    )
    .expect("opens")
    .expect("a gate");
    let served = served_built("grpc-mdm-cap", |rig_dir, clock| {
        Rig::start(rig_dir, clock).with_mdm(Some(mdm))
    });
    let last = format!("D{:04}", MAX_DEVICE_PAGE - 1);
    for limit in [0, 1000, 256, 257] {
        let (code, _, listed) = page(&served, limit, "").await;
        assert_eq!(code, Code::Ok, "limit {limit}");
        assert_eq!(listed.devices.len(), MAX_DEVICE_PAGE, "limit {limit}");
        assert_eq!(listed.next_page_token, last, "limit {limit}");
    }
    let (_, _, short) = page(&served, 255, "").await;
    assert_eq!(short.devices.len(), 255);
    assert_eq!(short.next_page_token, "D0254");
    let (_, _, rest) = page(&served, 0, &last).await;
    assert_eq!(
        (ids(&rest), rest.next_page_token.as_str()),
        (vec!["D0256"], "")
    );
}

/// Proves: a page token that is no enrollment id (a path, 65 characters, a space) is refused
/// `invalid_request` with `INVALID_ARGUMENT` by gRPC and 400 by the gateway, with a gate and
/// without one; and a valid token that no record has pages from where it would sort.
///
/// Catches: a token that is looked at only when a gate runs, a token used as a file name before
/// it is checked, and a refusal the gateway words differently.
#[tokio::test]
async fn a_page_token_that_is_no_enrollment_id_is_invalid_request() {
    let (gate, _dir) = gated("grpc-mdm-token", &two_records());
    let bare = served("grpc-mdm-token-bare");
    let long = "A".repeat(65);
    for served in [&gate, &bare] {
        for token in ["../x", long.as_str(), "a b", "x/y"] {
            let (code, said, listed) = page(served, 0, token).await;
            assert_eq!(
                (code, said.as_str(), listed),
                (Code::InvalidArgument, "invalid_request", Default::default()),
                "{token}"
            );
            let body = json!({"pageToken": token}).to_string();
            let (status, json) = gateway(served, Some(OPERATOR), &body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{token}");
            assert_eq!(
                json,
                json!({"code": "INVALID_ARGUMENT", "reason": "invalid_request"}),
                "{token}"
            );
        }
    }
    let (_, _, after) = page(&gate, 0, "AAAA-GONE").await;
    assert_eq!(ids(&after), ["BBBB-BARE"]);
    let (_, _, exact) = page(&gate, 0, &"A".repeat(64)).await;
    assert_eq!(ids(&exact), ["BBBB-BARE"]);
}

/// Proves: a coordinator that runs no gate answers `ListDevices` with no device, no token and
/// `gate` false, by gRPC and by the gateway (where the JSON is `{}`, the false not shown).
///
/// Catches: a coordinator without the gate that refuses the call, answers `gate` true, or
/// lists a device.
#[tokio::test]
async fn a_coordinator_without_the_gate_lists_nothing_and_says_so() {
    let bare = served("grpc-mdm-none");
    for (limit, token) in [(0, ""), (1, ""), (1000, "AAAA-1")] {
        let (code, said, listed) = page(&bare, limit, token).await;
        assert_eq!((code, said.as_str()), (Code::Ok, ""));
        assert_eq!(listed, proto::ListDevicesResponse::default());
        assert!(!listed.gate);
    }
    let (status, json) = gateway(&bare, Some(OPERATOR), "{}").await;
    assert_eq!((status, json), (StatusCode::OK, json!({})));
}

/// Proves: CI's token is refused `forbidden` and no token `unauthorized` by `Mdm.ListDevices`,
/// even with a gate and devices to list, and nothing is listed.
///
/// Catches: the devices of the gate read with a token the operator API does not take.
#[tokio::test]
async fn listing_devices_takes_the_operator_token_only() {
    let (served, _gate) = gated("grpc-mdm-auth", &two_records());
    for (bearer, want) in [
        (Some(ci_token()), (Code::PermissionDenied, "forbidden")),
        (None, (Code::Unauthenticated, "unauthorized")),
    ] {
        let request = grpc_request(
            LIST,
            proto::ListDevicesRequest::default(),
            bearer.as_deref(),
        );
        let (code, said, body) = call(&served.coordinator, Surface::Control, &None, request).await;
        assert_eq!((code, said.as_str()), want);
        assert!(body.is_none());
    }
    // The token is checked before the page token: a bad token does not show which is wrong.
    let request = grpc_request(
        LIST,
        proto::ListDevicesRequest {
            limit: 0,
            page_token: "../x".to_owned(),
        },
        Some(&ci_token()),
    );
    let (code, said, _) = call(&served.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, said.as_str()), (Code::PermissionDenied, "forbidden"));
}
