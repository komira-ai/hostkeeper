//! What the gate reads of a webhook event, and of the names in it: every way an event is
//! refused or passed over, with the text of each refusal, and the grammar of the identifiers.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_mdm_gate::{
    Command, CommandUuid, EnrollmentId, Event, EventError, Handled, WebhookError, is_serial,
    parse_event,
};
use plist::Value;
use serde_json::json;
use support::{
    Rig, UDID, authenticate, check_out, checkin, checkin_of, plist, result, token_update,
};

/// A webhook body of `topic` whose event object `key` is `inner`.
fn event(topic: &str, key: &str, inner: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"topic": topic, key: inner})).expect("json")
}

/// The base64 of a plist dictionary of `MessageType` `said`.
fn payload_of(said: &str) -> String {
    STANDARD.encode(plist(support::dict(&[("MessageType", Value::from(said))])))
}

fn id_of(id: &str) -> EnrollmentId {
    support::id(id)
}

/// Catches: a refusal whose text names the wrong part of the event, or a body that is not JSON
/// (or lacks a topic) taken as an event.
#[test]
fn a_body_that_is_not_an_event_is_refused_with_the_parser_s_reason() {
    assert_eq!(
        parse_event(b"{").expect_err("not JSON").to_string(),
        "the body is not a NanoHUB webhook event: EOF while parsing an object at line 1 column 1"
    );
    assert_eq!(
        parse_event(b"{}").expect_err("no topic"),
        EventError::Json("missing field `topic` at line 1 column 2".to_owned())
    );
    assert_eq!(
        parse_event(b"[]").expect_err("an array").to_string(),
        "the body is not a NanoHUB webhook event: invalid length 0, expected struct Wire with 3 elements at line 1 column 2"
    );
}

/// Catches: an event of a topic the gate reads that has no event object (or the other topic's
/// object) taken as whole, or a missing `ids`.
#[test]
fn a_topic_without_its_event_object_or_ids_is_refused() {
    let no_object = |topic: &str, body: Vec<u8>| {
        assert_eq!(
            parse_event(&body).expect_err("refused").to_string(),
            format!("the {topic} event carries no event object")
        );
    };
    no_object(
        "mdm.TokenUpdate",
        event("mdm.TokenUpdate", "acknowledge_event", json!({})),
    );
    no_object(
        "mdm.Authenticate",
        br#"{"topic":"mdm.Authenticate"}"#.to_vec(),
    );
    no_object(
        "mdm.CheckOut",
        br#"{"topic":"mdm.CheckOut","checkin_event":null}"#.to_vec(),
    );
    no_object(
        "mdm.Connect",
        event("mdm.Connect", "checkin_event", json!({})),
    );

    for (topic, key) in [
        ("mdm.Authenticate", "checkin_event"),
        ("mdm.Connect", "acknowledge_event"),
    ] {
        assert_eq!(
            parse_event(&event(topic, key, json!({"udid": UDID})))
                .expect_err("no ids")
                .to_string(),
            format!("the {topic} event carries no ids")
        );
    }
}

/// Catches: an enrollment id that would name a file outside the device directory, or an empty
/// or 65-character one, taken; a 64-character one refused.
#[test]
fn an_enrollment_id_that_is_not_one_to_sixty_four_letters_digits_and_dashes_is_refused() {
    let with_id = |id: &str| {
        let ids = json!({"id": id, "type": "Device"});
        parse_event(&event("mdm.CheckOut", "checkin_event", json!({"ids": ids})))
    };
    for bad in ["", "../x", "a/b", "a.b", "a_b", "a b", "é", &"a".repeat(65)] {
        assert_eq!(
            with_id(bad).expect_err(bad).to_string(),
            "the mdm.CheckOut event's enrollment id is not one the gate keeps",
            "{bad:?}"
        );
    }
    // The id is checked before the payload, which here is missing.
    assert_eq!(
        with_id(&"a".repeat(64)),
        Err(EventError::Payload("mdm.CheckOut".to_owned()))
    );
}

/// Catches: a topic outside the four, or a user channel's event, acted on; a user
/// channel with an id the gate would refuse reported as an error instead of passed over.
#[test]
fn another_topic_and_a_user_channel_are_passed_over() {
    for topic in [
        "mdm.DeclarativeManagement",
        "mdm.UserAuthenticate",
        "mdm.Other",
        "",
    ] {
        let body = event(topic, "checkin_event", json!({"junk": 1}));
        assert_eq!(parse_event(&body), Ok(None), "{topic:?}");
    }
    let user = checkin_of("TokenUpdate", UDID, "User", &[], Some(1));
    assert_eq!(parse_event(&user), Ok(None));
    let ids = json!({"id": "not an id!", "type": "User"});
    let body = event("mdm.CheckOut", "checkin_event", json!({"ids": ids}));
    assert_eq!(parse_event(&body), Ok(None));
    let ids = json!({"id": UDID, "type": "User"});
    let body = event("mdm.Connect", "acknowledge_event", json!({"ids": ids}));
    assert_eq!(parse_event(&body), Ok(None));
}

/// Catches: a `raw_payload` that is not base64, not a plist, or a plist that is not a
/// dictionary, read as a check-in or a result; or an absent one read as empty.
#[test]
fn a_payload_that_is_not_a_plist_dictionary_is_refused() {
    let array_plist = {
        let mut bytes = Vec::new();
        Value::Array(vec![Value::from("x")])
            .to_writer_xml(&mut bytes)
            .expect("a plist");
        STANDARD.encode(bytes)
    };
    let payloads = [
        json!("!!!not base64!!!"),
        json!(STANDARD.encode(b"not a plist")),
        json!(array_plist),
        json!(""),
    ];
    for (topic, key) in [
        ("mdm.TokenUpdate", "checkin_event"),
        ("mdm.Connect", "acknowledge_event"),
    ] {
        let ids = json!({"id": UDID, "type": "Device"});
        for raw in &payloads {
            let body = event(topic, key, json!({"ids": ids, "raw_payload": raw}));
            assert_eq!(
                parse_event(&body).expect_err("refused").to_string(),
                format!("the {topic} event's raw_payload is not a property list")
            );
        }
        let body = event(topic, key, json!({"ids": ids}));
        assert_eq!(
            parse_event(&body),
            Err(EventError::Payload(topic.to_owned())),
            "no raw_payload"
        );
    }
}

/// Catches: a check-in whose plist says another `MessageType` than its topic (or none) acted on
/// as the topic says.
#[test]
fn a_check_in_whose_message_type_differs_from_its_topic_is_refused() {
    let ids = json!({"id": UDID, "type": "Device"});
    let with = |topic: &str, said: &str| {
        let inner = json!({"ids": ids, "raw_payload": payload_of(said)});
        parse_event(&event(topic, "checkin_event", inner))
    };
    assert_eq!(
        with("mdm.TokenUpdate", "Authenticate")
            .expect_err("refused")
            .to_string(),
        "the mdm.TokenUpdate event's payload says MessageType Authenticate"
    );
    assert_eq!(
        with("mdm.CheckOut", "").expect_err("refused").to_string(),
        "the mdm.CheckOut event's payload says MessageType "
    );
    // A plist with no `MessageType` at all, and one whose is not a string, say nothing.
    let none = STANDARD.encode(plist(support::dict(&[("UDID", Value::from(UDID))])));
    let inner = json!({"ids": ids, "raw_payload": none});
    assert_eq!(
        parse_event(&event("mdm.Authenticate", "checkin_event", inner)),
        Err(EventError::MessageType(
            "mdm.Authenticate".to_owned(),
            String::new()
        ))
    );
    let number = STANDARD.encode(plist(support::dict(&[(
        "MessageType",
        Value::Integer(5.into()),
    )])));
    let inner = json!({"ids": ids, "raw_payload": number});
    assert_eq!(
        parse_event(&event("mdm.Authenticate", "checkin_event", inner)),
        Err(EventError::MessageType(
            "mdm.Authenticate".to_owned(),
            String::new()
        ))
    );
    // The same topic and type is read; a result (`mdm.Connect`) carries no `MessageType`.
    assert!(matches!(
        with("mdm.CheckOut", "CheckOut"),
        Ok(Some(Event::CheckOut { .. }))
    ));
}

/// Catches: the fields of each topic lost or misread: the UDID, the tally, the serial, and the
/// result's UUID, status and payload.
#[test]
fn each_topic_is_read_into_its_event() {
    let serial = |body: &[u8]| match parse_event(body) {
        Ok(Some(Event::Authenticate { id, udid, serial })) => (id, udid, serial),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        serial(&authenticate(UDID, Some("C02MT66KFLHH"))),
        (
            id_of(UDID),
            UDID.to_owned(),
            Some("C02MT66KFLHH".to_owned())
        )
    );
    // A serial that is not one is dropped: too long, empty, with a space, or not a string.
    for bad in [&"A".repeat(65)[..], "", "C02 MT66", "C02-MT66"] {
        assert_eq!(serial(&authenticate(UDID, Some(bad))).2, None, "{bad:?}");
    }
    assert_eq!(
        serial(&authenticate(UDID, Some(&"A".repeat(64)))).2,
        Some("A".repeat(64))
    );
    assert_eq!(serial(&authenticate(UDID, None)).2, None);
    let numeric = checkin(
        "Authenticate",
        UDID,
        &[("SerialNumber", Value::Integer(7.into()))],
        None,
    );
    assert_eq!(serial(&numeric).2, None);

    assert_eq!(
        parse_event(&token_update(UDID, 3)),
        Ok(Some(Event::TokenUpdate {
            id: id_of(UDID),
            udid: UDID.to_owned(),
            tally: Some(3)
        }))
    );
    assert_eq!(
        parse_event(&checkin("TokenUpdate", UDID, &[], None)),
        Ok(Some(Event::TokenUpdate {
            id: id_of(UDID),
            udid: UDID.to_owned(),
            tally: None
        }))
    );
    assert_eq!(
        parse_event(&check_out(UDID)),
        Ok(Some(Event::CheckOut { id: id_of(UDID) }))
    );

    let body = result(
        UDID,
        Some("ABCDEF01-0000-4000-8000-000000000000"),
        "NotNow",
        &[],
    );
    let Ok(Some(Event::Result {
        id,
        command_uuid,
        status,
        payload,
    })) = parse_event(&body)
    else {
        panic!("a result");
    };
    assert_eq!(id, id_of(UDID));
    assert_eq!(
        command_uuid.as_deref(),
        Some("ABCDEF01-0000-4000-8000-000000000000")
    );
    assert_eq!(status, "NotNow");
    let dict = Value::from_reader(std::io::Cursor::new(&payload))
        .expect("the payload is the plist")
        .into_dictionary()
        .expect("a dictionary");
    assert_eq!(dict["Status"].as_string(), Some("NotNow"));
}

/// Catches: an event without `udid` or `status` refused, or the missing UDID read as a name.
#[test]
fn a_missing_udid_and_a_missing_status_read_as_empty() {
    let ids = json!({"id": UDID, "type": "Device"});
    let raw = payload_of("TokenUpdate");
    let inner = json!({"ids": ids, "raw_payload": raw});
    assert_eq!(
        parse_event(&event("mdm.TokenUpdate", "checkin_event", inner)),
        Ok(Some(Event::TokenUpdate {
            id: id_of(UDID),
            udid: String::new(),
            tally: None
        }))
    );
    let inner = json!({"ids": ids, "raw_payload": raw});
    let Ok(Some(Event::Result {
        command_uuid,
        status,
        ..
    })) = parse_event(&event("mdm.Connect", "acknowledge_event", inner))
    else {
        panic!("a result");
    };
    assert_eq!((command_uuid, status.as_str()), (None, ""));
}

/// Catches: the refusals of the parser reaching the caller of the gate as something other than
/// `WebhookError::Event`, or changing a record on the way; and a passed-over topic being
/// recorded.
#[tokio::test]
async fn the_gate_reports_an_event_error_and_passes_over_a_topic_it_does_not_read() {
    let rig = Rig::new("event-errors").await;
    let refused = rig.gate.webhook(b"nope").await.expect_err("refused");
    assert!(
        matches!(refused, WebhookError::Event(EventError::Json(_))),
        "{refused}"
    );
    assert_eq!(
        refused.to_string(),
        "the body is not a NanoHUB webhook event: expected ident at line 1 column 2"
    );
    let other = event("mdm.DeclarativeManagement", "checkin_event", json!({}));
    assert_eq!(
        rig.gate.webhook(&other).await.expect("taken"),
        Handled::PassedOver
    );
    let user = checkin_of("Authenticate", UDID, "User", &[], None);
    assert_eq!(
        rig.gate.webhook(&user).await.expect("taken"),
        Handled::PassedOver
    );
    assert!(rig.gate.records().is_empty());
    assert!(rig.hub.heard().is_empty());
}

/// Catches: a UUID whose version or variant bits are not set, or one spelled in lowercase or
/// with other groups.
#[test]
fn a_command_uuid_is_version_four_uppercase() {
    let uuid = |bytes: [u8; 16]| CommandUuid::from_random(bytes).to_string();
    assert_eq!(uuid([0; 16]), "00000000-0000-4000-8000-000000000000");
    assert_eq!(uuid([0xff; 16]), "FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF");
    let counted: [u8; 16] = core::array::from_fn(|i| 0xA0 + u8::try_from(i).expect("small"));
    assert_eq!(uuid(counted), "A0A1A2A3-A4A5-46A7-A8A9-AAABACADAEAF");
    assert_eq!(
        CommandUuid::from_random(counted).as_str(),
        "A0A1A2A3-A4A5-46A7-A8A9-AAABACADAEAF"
    );
}

/// Catches: a result's UUID of the wrong length or with a non-hexadecimal byte accepted as a
/// command UUID, or its case changed (NanoHUB gives it back as sent).
#[test]
fn a_command_uuid_parses_thirty_six_hex_digits_and_dashes() {
    let good = "0a1b2c3d-AAAA-4bbb-8ccc-0123456789aB";
    assert_eq!(CommandUuid::parse(good).expect("a UUID").as_str(), good);
    assert_eq!(CommandUuid::parse(good).expect("a UUID").to_string(), good);
    assert!(CommandUuid::parse(&good[..35]).is_none());
    assert!(CommandUuid::parse(&format!("{good}0")).is_none());
    assert!(CommandUuid::parse("").is_none());
    assert!(CommandUuid::parse(&good.replace('0', "g")).is_none());
    // 36 bytes, one of them not ASCII.
    assert!(CommandUuid::parse(&format!("{}é", &good[..34])).is_none());
}

/// Catches: the limits of an enrollment id or a serial moving: 1 and 64 accepted, 0 and 65
/// refused, and a byte outside the set refused (an id takes `-`, a serial does not).
#[test]
fn identifier_grammars_have_their_boundaries() {
    for len in [1usize, 2, 63, 64] {
        let text = "aZ9-".repeat(16)[..len].to_owned();
        assert_eq!(EnrollmentId::parse(&text).expect("an id").as_str(), text);
        assert_eq!(EnrollmentId::parse(&text).expect("an id").to_string(), text);
        let letters = "aZ9".repeat(22)[..len].to_owned();
        assert!(is_serial(&letters), "{len}");
    }
    assert!(EnrollmentId::parse("").is_none());
    assert!(EnrollmentId::parse(&"a".repeat(65)).is_none());
    assert!(!is_serial(""));
    assert!(!is_serial(&"a".repeat(65)));
    for bad in ["a b", "a.b", "a/b", "a_b", "é", "a\n", "a\0"] {
        assert!(EnrollmentId::parse(bad).is_none(), "{bad:?}");
        assert!(!is_serial(bad), "{bad:?}");
    }
    assert!(EnrollmentId::parse("a-b").is_some());
    assert!(!is_serial("a-b"));
}

/// Catches: the bytes the gate hands NanoHUB changing (the key order, the escaping or the
/// header of the plist) without the golden moving.
#[test]
fn a_command_is_the_xml_plist_nanohub_takes() {
    let uuid = CommandUuid::from_random([0; 16]);
    let info = Command::device_information(uuid.clone());
    assert_eq!(
        String::from_utf8(info.to_plist()).expect("utf-8"),
        DEVICE_INFORMATION_PLIST
    );
    let security = Command::security_info(uuid.clone());
    assert_eq!(
        String::from_utf8(security.to_plist()).expect("utf-8"),
        SECURITY_INFO_PLIST
    );
    assert_eq!(
        (info.request_type(), info.uuid()),
        ("DeviceInformation", &uuid)
    );
    assert!(info.is_allowed() && security.is_allowed());
    assert_eq!(
        hostkeeper_mdm_gate::QUERIES,
        ["SerialNumber", "UDID", "IsSupervised"]
    );
}

const DEVICE_INFORMATION_PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Command</key>\n\t<dict>\n\t\t<key>Queries</key>\n\t\t<array>\n\t\t\t<string>SerialNumber</string>\n\t\t\t<string>UDID</string>\n\t\t\t<string>IsSupervised</string>\n\t\t</array>\n\t\t<key>RequestType</key>\n\t\t<string>DeviceInformation</string>\n\t</dict>\n\t<key>CommandUUID</key>\n\t<string>00000000-0000-4000-8000-000000000000</string>\n</dict>\n</plist>";
const SECURITY_INFO_PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Command</key>\n\t<dict>\n\t\t<key>RequestType</key>\n\t\t<string>SecurityInfo</string>\n\t</dict>\n\t<key>CommandUUID</key>\n\t<string>00000000-0000-4000-8000-000000000000</string>\n</dict>\n</plist>";
