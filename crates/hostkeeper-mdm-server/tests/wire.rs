//! The wire types against Apple's schemas and against messages with a defect, one negative case
//! per message type.

use hostkeeper_mdm_server::{
    CheckIn, CommandRequest, CommandResult, DeviceId, Error, ErrorEntry, Status,
};
use plist::{Dictionary, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/plist/", $name, ".plist")).as_slice()
    };
}

const UDID: &str = "00000000-0000-4000-8000-0000000000AA";
const COMMAND: &str = "F431F51C-AF60-441E-BB4E-921669EF989D";

fn dict(bytes: &[u8]) -> Dictionary {
    Value::from_reader(std::io::Cursor::new(bytes))
        .unwrap()
        .into_dictionary()
        .unwrap()
}

fn xml(d: &Dictionary) -> Vec<u8> {
    let mut out = Vec::new();
    Value::Dictionary(d.clone())
        .to_writer_xml(&mut out)
        .unwrap();
    out
}

fn without(bytes: &[u8], key: &str) -> Vec<u8> {
    let mut d = dict(bytes);
    assert!(d.remove(key).is_some(), "the fixture has no key {key}");
    xml(&d)
}

fn with(bytes: &[u8], key: &str, value: Value) -> Vec<u8> {
    let mut d = dict(bytes);
    d.insert(key.to_owned(), value);
    xml(&d)
}

fn binary(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    Value::Dictionary(dict(bytes))
        .to_writer_binary(&mut out)
        .unwrap();
    out
}

fn top_level_keys(schema: &str) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = Vec::new();
    for line in schema.lines() {
        if let Some(name) = line.strip_prefix("- key: ") {
            keys.push((name.trim().to_owned(), String::new()));
        } else if let Some(p) = line.strip_prefix("  presence: ")
            && let Some(last) = keys.last_mut()
            && last.1.is_empty()
        {
            last.1 = p.trim().to_owned();
        }
    }
    keys
}

fn request_type(schema: &str) -> String {
    let line = schema
        .lines()
        .find(|l| l.trim_start().starts_with("requesttype: "))
        .unwrap();
    line.trim_start()
        .strip_prefix("requesttype: ")
        .unwrap()
        .trim()
        .to_owned()
}

const SCHEMAS: [(&str, &str, &[u8]); 6] = [
    (
        "Authenticate",
        include_str!("fixtures/apple-schema/checkin-authenticate.yaml"),
        fixture!("checkin-authenticate"),
    ),
    (
        "TokenUpdate",
        include_str!("fixtures/apple-schema/checkin-tokenupdate.yaml"),
        fixture!("checkin-tokenupdate"),
    ),
    (
        "CheckOut",
        include_str!("fixtures/apple-schema/checkin-checkout.yaml"),
        fixture!("checkin-checkout"),
    ),
    (
        "DeclarativeManagement",
        include_str!("fixtures/apple-schema/checkin-declarativemanagement.yaml"),
        fixture!("checkin-declarativemanagement"),
    ),
    (
        "GetBootstrapToken",
        include_str!("fixtures/apple-schema/checkin-getbootstraptoken.yaml"),
        fixture!("checkin-getbootstraptoken"),
    ),
    (
        "SetBootstrapToken",
        include_str!("fixtures/apple-schema/checkin-setbootstraptoken.yaml"),
        fixture!("checkin-setbootstraptoken"),
    ),
];

#[test]
fn every_fixture_key_is_a_key_of_its_apple_schema_and_the_message_type_matches() {
    for (name, schema, bytes) in SCHEMAS {
        let known: Vec<String> = top_level_keys(schema).into_iter().map(|(k, _)| k).collect();
        for key in dict(bytes).keys() {
            if key == "SomeKeyAppleAddsLater" {
                continue;
            }
            assert!(known.contains(key), "{name}: {key} is not in the schema");
        }
        assert!(
            schema.contains(&format!("  - {name}\n")),
            "{name}: not in the rangelist"
        );
        assert_eq!(CheckIn::parse(bytes).unwrap().message_type(), name);
    }
}

#[test]
fn every_key_the_readers_require_is_marked_required_in_the_schema() {
    // The keys a message needs for the server to act on it; the schema marks them `required`.
    let needs: [(&str, &[&str]); 6] = [
        ("Authenticate", &["MessageType", "Topic"]),
        (
            "TokenUpdate",
            &["MessageType", "Topic", "Token", "PushMagic"],
        ),
        ("CheckOut", &["MessageType", "Topic"]),
        ("DeclarativeManagement", &["MessageType", "Endpoint"]),
        ("GetBootstrapToken", &["MessageType"]),
        ("SetBootstrapToken", &["MessageType"]),
    ];
    for (name, keys) in needs {
        let (_, schema, _) = SCHEMAS.iter().find(|(n, _, _)| *n == name).unwrap();
        let schema_keys = top_level_keys(schema);
        for key in keys {
            let found = schema_keys.iter().find(|(k, _)| k == key);
            assert_eq!(
                found.map(|(_, p)| p.as_str()),
                Some("required"),
                "{name}.{key}"
            );
        }
    }
}

#[test]
fn authenticate_reads_the_macs_identity_and_ignores_keys_it_does_not_know() {
    let CheckIn::Authenticate(a) = CheckIn::parse(fixture!("checkin-authenticate")).unwrap() else {
        panic!("not Authenticate");
    };
    assert_eq!(a.id.key(), UDID);
    assert_eq!(a.id.enrollment_id, None);
    assert_eq!(
        a.topic,
        "com.apple.mgmt.External.00000000-0000-4000-8000-000000000000"
    );
    assert_eq!(a.serial_number.as_deref(), Some("TESTSERIAL01"));
    assert_eq!(a.device_name.as_deref(), Some("test-mac"));
    assert_eq!(a.model.as_deref(), Some("Mac00,0"));
    assert_eq!(a.model_name.as_deref(), Some("Mac Studio"));
    assert_eq!(a.os_version.as_deref(), Some("27.0.1"));
    assert_eq!(a.build_version.as_deref(), Some("00A000"));
    assert_eq!(a.product_name.as_deref(), Some("Mac00,0"));
}

#[test]
fn authenticate_is_refused_without_topic_device_identity_or_with_a_wrong_type() {
    let ok = fixture!("checkin-authenticate");
    assert_eq!(
        CheckIn::parse(&without(ok, "Topic")),
        Err(Error::MissingKey("Topic"))
    );
    assert_eq!(CheckIn::parse(&without(ok, "UDID")), Err(Error::NoDeviceId));
    assert_eq!(
        CheckIn::parse(&with(ok, "SerialNumber", Value::Integer(1.into()))),
        Err(Error::WrongType {
            key: "SerialNumber",
            expected: "a string"
        }),
    );
    assert_eq!(
        CheckIn::parse(&with(ok, "Topic", Value::String(String::new()))),
        Err(Error::Empty("Topic")),
    );
}

#[test]
fn a_user_enrollment_is_keyed_on_its_enrollment_id() {
    let by_enrollment = with(
        &without(fixture!("checkin-authenticate"), "UDID"),
        "EnrollmentID",
        "E-1".into(),
    );
    let CheckIn::Authenticate(a) = CheckIn::parse(&by_enrollment).unwrap() else {
        panic!("not Authenticate");
    };
    assert_eq!(
        a.id,
        DeviceId {
            udid: None,
            enrollment_id: Some("E-1".to_owned())
        }
    );
    assert_eq!(a.id.key(), "E-1");
}

#[test]
fn token_update_reads_the_raw_token_and_the_push_magic() {
    let CheckIn::TokenUpdate(t) = CheckIn::parse(fixture!("checkin-tokenupdate")).unwrap() else {
        panic!("not TokenUpdate");
    };
    assert_eq!(t.id.key(), UDID);
    assert_eq!(t.token, (0u8..32).collect::<Vec<u8>>());
    assert_eq!(t.push_magic, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    assert_eq!(t.unlock_token.as_deref(), Some(b"unlock".as_slice()));
    assert_eq!(t.awaiting_configuration, Some(false));
    assert_eq!(t.not_on_console, Some(false));
    assert_eq!(t.enrollment_user_id, None);
    assert!(t.topic.starts_with("com.apple.mgmt."));
}

#[test]
fn token_update_reads_the_user_id_of_a_user_channel() {
    let ok = fixture!("checkin-tokenupdate");
    let CheckIn::TokenUpdate(device) = CheckIn::parse(ok).unwrap() else {
        panic!("not TokenUpdate");
    };
    assert_eq!(device.user_id, None);
    let CheckIn::TokenUpdate(user) =
        CheckIn::parse(&with(ok, "UserID", "ABCD-1234".into())).unwrap()
    else {
        panic!("not TokenUpdate");
    };
    assert_eq!(user.user_id.as_deref(), Some("ABCD-1234"));
}

#[test]
fn token_update_is_refused_without_token_or_magic_and_with_an_empty_or_mistyped_token() {
    let ok = fixture!("checkin-tokenupdate");
    assert_eq!(
        CheckIn::parse(&without(ok, "Token")),
        Err(Error::MissingKey("Token"))
    );
    assert_eq!(
        CheckIn::parse(&without(ok, "PushMagic")),
        Err(Error::MissingKey("PushMagic"))
    );
    assert_eq!(
        CheckIn::parse(&with(ok, "Token", Value::Data(Vec::new()))),
        Err(Error::Empty("Token")),
    );
    assert_eq!(
        CheckIn::parse(&with(ok, "Token", "abc".into())),
        Err(Error::WrongType {
            key: "Token",
            expected: "data"
        }),
    );
    assert_eq!(
        CheckIn::parse(&with(ok, "NotOnConsole", "no".into())),
        Err(Error::WrongType {
            key: "NotOnConsole",
            expected: "a boolean"
        }),
    );
}

#[test]
fn check_out_names_the_device_and_is_refused_without_one() {
    let CheckIn::CheckOut(c) = CheckIn::parse(fixture!("checkin-checkout")).unwrap() else {
        panic!("not CheckOut");
    };
    assert_eq!(c.id.key(), UDID);
    assert!(c.topic.starts_with("com.apple.mgmt."));
    let ok = fixture!("checkin-checkout");
    assert_eq!(CheckIn::parse(&without(ok, "UDID")), Err(Error::NoDeviceId));
    assert_eq!(
        CheckIn::parse(&without(ok, "Topic")),
        Err(Error::MissingKey("Topic"))
    );
}

#[test]
fn declarative_management_reads_endpoint_and_body() {
    let CheckIn::DeclarativeManagement(d) =
        CheckIn::parse(fixture!("checkin-declarativemanagement")).unwrap()
    else {
        panic!("not DeclarativeManagement");
    };
    assert_eq!(d.id.key(), UDID);
    assert_eq!(d.endpoint, "declaration-items");
    assert_eq!(d.data.as_deref(), Some(b"{}".as_slice()));
    let no_data = without(fixture!("checkin-declarativemanagement"), "Data");
    let CheckIn::DeclarativeManagement(d) = CheckIn::parse(&no_data).unwrap() else {
        panic!("not DeclarativeManagement");
    };
    assert_eq!(d.data, None);
}

#[test]
fn declarative_management_is_refused_without_endpoint_or_device() {
    let ok = fixture!("checkin-declarativemanagement");
    assert_eq!(
        CheckIn::parse(&without(ok, "Endpoint")),
        Err(Error::MissingKey("Endpoint"))
    );
    assert_eq!(CheckIn::parse(&without(ok, "UDID")), Err(Error::NoDeviceId));
    assert_eq!(
        CheckIn::parse(&with(ok, "Data", "text".into())),
        Err(Error::WrongType {
            key: "Data",
            expected: "data"
        }),
    );
}

#[test]
fn bootstrap_token_messages_read_their_optional_keys() {
    let CheckIn::GetBootstrapToken(g) =
        CheckIn::parse(fixture!("checkin-getbootstraptoken")).unwrap()
    else {
        panic!("not GetBootstrapToken");
    };
    assert_eq!(g.awaiting_configuration, Some(true));
    let CheckIn::SetBootstrapToken(s) =
        CheckIn::parse(fixture!("checkin-setbootstraptoken")).unwrap()
    else {
        panic!("not SetBootstrapToken");
    };
    assert_eq!(s.bootstrap_token.as_deref(), Some(b"bootstrap".as_slice()));
    assert_eq!(s.awaiting_configuration, Some(false));
    let cleared = without(fixture!("checkin-setbootstraptoken"), "BootstrapToken");
    let CheckIn::SetBootstrapToken(s) = CheckIn::parse(&cleared).unwrap() else {
        panic!("not SetBootstrapToken");
    };
    assert_eq!(s.bootstrap_token, None);
}

#[test]
fn bootstrap_token_messages_are_refused_with_a_wrong_type() {
    assert_eq!(
        CheckIn::parse(&with(
            fixture!("checkin-getbootstraptoken"),
            "AwaitingConfiguration",
            1.into()
        )),
        Err(Error::WrongType {
            key: "AwaitingConfiguration",
            expected: "a boolean"
        }),
    );
    assert_eq!(
        CheckIn::parse(&with(
            fixture!("checkin-setbootstraptoken"),
            "BootstrapToken",
            "x".into()
        )),
        Err(Error::WrongType {
            key: "BootstrapToken",
            expected: "data"
        }),
    );
}

#[test]
fn a_check_in_is_refused_for_bytes_that_are_no_dictionary_and_for_unknown_or_missing_types() {
    assert!(matches!(
        CheckIn::parse(b"not a plist"),
        Err(Error::NotPlist(_))
    ));
    let mut array = Vec::new();
    Value::Array(vec![]).to_writer_xml(&mut array).unwrap();
    assert_eq!(CheckIn::parse(&array), Err(Error::NotDictionary));
    assert_eq!(
        CheckIn::parse(&with(
            fixture!("checkin-checkout"),
            "MessageType",
            "GetToken".into()
        )),
        Err(Error::UnknownMessageType("GetToken".to_owned())),
    );
    assert_eq!(
        CheckIn::parse(&without(fixture!("checkin-checkout"), "MessageType")),
        Err(Error::MissingKey("MessageType")),
    );
}

#[test]
fn a_binary_property_list_reads_as_the_xml_one_does() {
    for (_, _, bytes) in SCHEMAS {
        assert_eq!(CheckIn::parse(&binary(bytes)), CheckIn::parse(bytes));
    }
    assert_eq!(
        CommandResult::parse(&binary(fixture!("result-error"))),
        CommandResult::parse(fixture!("result-error"))
    );
}

#[test]
fn idle_is_a_result_with_no_command() {
    let r = CommandResult::parse(fixture!("result-idle")).unwrap();
    assert_eq!(r.status, Status::Idle);
    assert_eq!(r.device, UDID);
    assert_eq!(r.command_uuid, None);
}

#[test]
fn acknowledged_carries_the_command_and_its_answer_keys() {
    let r = CommandResult::parse(fixture!("result-acknowledged")).unwrap();
    assert_eq!(r.status, Status::Acknowledged);
    assert_eq!(r.command_uuid.as_deref(), Some(COMMAND));
    assert!(matches!(r.raw.get("ProfileList"), Some(Value::Array(a)) if a.len() == 1));
}

#[test]
fn error_carries_its_chain() {
    let r = CommandResult::parse(fixture!("result-error")).unwrap();
    assert_eq!(
        r.status,
        Status::Error(vec![ErrorEntry {
            code: 12021,
            domain: "MCProfileErrorDomain".to_owned(),
            localized_description: Some("The profile could not be installed.".to_owned()),
            us_english_description: Some("The profile could not be installed.".to_owned()),
        }]),
    );
    let bare = without(fixture!("result-error"), "ErrorChain");
    assert_eq!(
        CommandResult::parse(&bare).unwrap().status,
        Status::Error(Vec::new())
    );
}

#[test]
fn not_now_and_command_format_error_are_their_own_statuses() {
    let r = CommandResult::parse(fixture!("result-notnow")).unwrap();
    assert_eq!(
        (r.status, r.command_uuid.as_deref()),
        (Status::NotNow, Some(COMMAND))
    );
    let r = CommandResult::parse(fixture!("result-commandformaterror")).unwrap();
    assert_eq!(
        r.status,
        Status::CommandFormatError(vec![ErrorEntry {
            code: 1,
            domain: "MCMDMErrorDomain".to_owned(),
            localized_description: None,
            us_english_description: None,
        }]),
    );
}

#[test]
fn a_result_is_refused_for_each_defect() {
    let ok = fixture!("result-acknowledged");
    assert_eq!(
        CommandResult::parse(&without(ok, "Status")),
        Err(Error::MissingKey("Status"))
    );
    assert_eq!(
        CommandResult::parse(&with(ok, "Status", "Done".into())),
        Err(Error::UnknownStatus("Done".to_owned())),
    );
    assert_eq!(
        CommandResult::parse(&without(ok, "UDID")),
        Err(Error::NoDeviceId)
    );
    assert_eq!(
        CommandResult::parse(&without(ok, "CommandUUID")),
        Err(Error::MissingKey("CommandUUID"))
    );
    assert_eq!(
        CommandResult::parse(&without(fixture!("result-notnow"), "CommandUUID")),
        Err(Error::MissingKey("CommandUUID")),
    );
    assert!(matches!(
        CommandResult::parse(b"x"),
        Err(Error::NotPlist(_))
    ));
}

#[test]
fn a_result_for_a_user_enrollment_is_keyed_on_its_enrollment_id() {
    let by_enrollment = with(
        &without(fixture!("result-idle"), "UDID"),
        "EnrollmentID",
        "E-1".into(),
    );
    assert_eq!(CommandResult::parse(&by_enrollment).unwrap().device, "E-1");
}

#[test]
fn a_malformed_error_chain_is_refused() {
    let ok = fixture!("result-error");
    let wrong = Error::WrongType {
        key: "ErrorChain",
        expected: "an array of dictionaries",
    };
    assert_eq!(
        CommandResult::parse(&with(ok, "ErrorChain", "oops".into())),
        Err(wrong.clone())
    );
    assert_eq!(
        CommandResult::parse(&with(ok, "ErrorChain", Value::Array(vec!["x".into()]))),
        Err(wrong),
    );
    let mut entry = Dictionary::new();
    entry.insert("ErrorDomain".to_owned(), "D".into());
    assert_eq!(
        CommandResult::parse(&with(
            ok,
            "ErrorChain",
            Value::Array(vec![Value::Dictionary(entry)])
        )),
        Err(Error::WrongType {
            key: "ErrorCode",
            expected: "an integer"
        }),
    );
    let mut entry = Dictionary::new();
    entry.insert("ErrorCode".to_owned(), 5.into());
    assert_eq!(
        CommandResult::parse(&with(
            ok,
            "ErrorChain",
            Value::Array(vec![Value::Dictionary(entry)])
        )),
        Err(Error::MissingKey("ErrorDomain")),
    );
}

const COMMAND_SCHEMAS: [(&str, &str); 6] = [
    (
        "DeviceInformation",
        include_str!("fixtures/apple-schema/command-information.device.yaml"),
    ),
    (
        "SecurityInfo",
        include_str!("fixtures/apple-schema/command-information.security.yaml"),
    ),
    (
        "InstallProfile",
        include_str!("fixtures/apple-schema/command-profile.install.yaml"),
    ),
    (
        "RemoveProfile",
        include_str!("fixtures/apple-schema/command-profile.remove.yaml"),
    ),
    (
        "InstallEnterpriseApplication",
        include_str!("fixtures/apple-schema/command-application.install.enterprise.yaml"),
    ),
    (
        "ProfileList",
        include_str!("fixtures/apple-schema/command-profile.list.yaml"),
    ),
];

#[test]
fn the_request_types_the_server_sends_are_the_request_types_of_apples_command_schemas() {
    for (name, schema) in COMMAND_SCHEMAS {
        assert_eq!(request_type(schema), name);
    }
}

#[test]
fn a_command_renders_as_the_dictionary_a_mac_reads_and_reads_back() {
    let mut fields = Dictionary::new();
    fields.insert("ManagedOnly".to_owned(), Value::Boolean(false));
    let command = CommandRequest::new(COMMAND, "ProfileList", fields);
    let bytes = command.to_plist_xml().unwrap();
    let d = dict(&bytes);
    assert_eq!(
        d.get("CommandUUID").and_then(Value::as_string),
        Some(COMMAND)
    );
    let inner = d.get("Command").and_then(Value::as_dictionary).unwrap();
    assert_eq!(
        inner.get("RequestType").and_then(Value::as_string),
        Some("ProfileList")
    );
    assert_eq!(
        inner.get("ManagedOnly").and_then(Value::as_boolean),
        Some(false)
    );
    assert_eq!(CommandRequest::parse(&bytes).unwrap(), command);
    // ManagedOnly is a key of the schema's ProfileList command.
    let keys = top_level_keys(COMMAND_SCHEMAS[5].1);
    assert!(keys.iter().any(|(k, _)| k == "ManagedOnly"));
}

#[test]
fn a_command_with_no_uuid_or_type_is_not_rendered() {
    assert_eq!(
        CommandRequest::new("", "ProfileList", Dictionary::new()).to_plist_xml(),
        Err(Error::Empty("CommandUUID")),
    );
    assert_eq!(
        CommandRequest::new(COMMAND, "", Dictionary::new()).to_plist_xml(),
        Err(Error::Empty("RequestType")),
    );
}

#[test]
fn a_command_plist_is_refused_for_each_defect() {
    let ok = CommandRequest::new(COMMAND, "SecurityInfo", Dictionary::new())
        .to_plist_xml()
        .unwrap();
    assert_eq!(
        CommandRequest::parse(&without(&ok, "CommandUUID")),
        Err(Error::MissingKey("CommandUUID"))
    );
    assert_eq!(
        CommandRequest::parse(&without(&ok, "Command")),
        Err(Error::MissingKey("Command"))
    );
    assert_eq!(
        CommandRequest::parse(&with(&ok, "Command", "x".into())),
        Err(Error::WrongType {
            key: "Command",
            expected: "a dictionary"
        }),
    );
    assert_eq!(
        CommandRequest::parse(&with(&ok, "Command", Value::Dictionary(Dictionary::new()))),
        Err(Error::MissingKey("RequestType")),
    );
}

#[test]
fn the_error_texts_name_the_key_and_never_a_value() {
    assert_eq!(
        Error::MissingKey("Topic").to_string(),
        "key Topic is missing"
    );
    assert_eq!(
        Error::WrongType {
            key: "Token",
            expected: "data"
        }
        .to_string(),
        "key Token is not data",
    );
    assert_eq!(Error::Empty("Token").to_string(), "key Token is empty");
    assert_eq!(
        Error::NoDeviceId.to_string(),
        "neither UDID nor EnrollmentID is present"
    );
    assert_eq!(
        Error::NotDictionary.to_string(),
        "the property list is not a dictionary"
    );
    assert_eq!(
        Error::UnknownMessageType("X".to_owned()).to_string(),
        "unknown check-in message type \"X\"",
    );
    assert_eq!(
        Error::UnknownStatus("X".to_owned()).to_string(),
        "unknown command status \"X\""
    );
    assert_eq!(
        Error::Write("e".to_owned()).to_string(),
        "cannot write the property list: e"
    );
    assert_eq!(
        Error::NotPlist("e".to_owned()).to_string(),
        "not a property list: e"
    );
}

#[test]
fn a_command_with_a_value_xml_cannot_hold_is_not_rendered() {
    let mut fields = Dictionary::new();
    fields.insert("Odd".to_owned(), Value::Uid(plist::Uid::new(1)));
    let command = CommandRequest::new(COMMAND, "ProfileList", fields);
    assert!(matches!(command.to_plist_xml(), Err(Error::Write(_))));
}

#[test]
fn optional_booleans_may_be_absent() {
    let bare = without(
        without(fixture!("checkin-tokenupdate"), "NotOnConsole").as_slice(),
        "AwaitingConfiguration",
    );
    let CheckIn::TokenUpdate(t) = CheckIn::parse(&bare).unwrap() else {
        panic!("not TokenUpdate");
    };
    assert_eq!((t.not_on_console, t.awaiting_configuration), (None, None));
}

fn s(v: &str) -> Value {
    Value::String(v.to_owned())
}

#[test]
fn debug_of_a_token_update_prints_no_secret_byte() {
    let msg = CheckIn::parse(fixture!("checkin-tokenupdate")).unwrap();
    let shown = format!("{msg:?}");
    assert!(shown.contains("token_len: 32"), "{shown}");
    assert!(shown.contains("unlock_token_len: Some(6)"), "{shown}");
    assert!(!shown.contains("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"));
    assert!(!shown.contains("117, 110"), "{shown}");
    assert!(!shown.contains("[0, 1, 2"));
}

#[test]
fn debug_of_a_set_bootstrap_token_prints_no_secret_byte() {
    let bytes = with(
        fixture!("checkin-setbootstraptoken"),
        "BootstrapToken",
        Value::Data(b"secret-bootstrap".to_vec()),
    );
    let shown = format!("{:?}", CheckIn::parse(&bytes).unwrap());
    assert!(shown.contains("bootstrap_token_len: Some(16)"), "{shown}");
    assert!(!shown.contains("secret"));
    assert!(!shown.contains("115, 101"));
}

#[test]
fn debug_of_a_result_prints_key_names_and_no_answer_value() {
    let bytes = with(
        fixture!("result-acknowledged"),
        "SecretAnswer",
        s("hunter2-value"),
    );
    let shown = format!("{:?}", CommandResult::parse(&bytes).unwrap());
    assert!(shown.contains("SecretAnswer"), "{shown}");
    assert!(!shown.contains("hunter2-value"), "{shown}");
}

#[test]
fn an_empty_identity_is_refused_and_does_not_hide_the_other() {
    let co = fixture!("checkin-checkout");
    assert_eq!(
        CheckIn::parse(&with(co, "UDID", s(""))),
        Err(Error::Empty("UDID"))
    );
    let both = with(co, "EnrollmentID", s("ENROLL-1"));
    assert_eq!(
        CheckIn::parse(&with(&both, "UDID", s(""))),
        Err(Error::Empty("UDID"))
    );
    assert_eq!(
        CheckIn::parse(&with(&without(co, "UDID"), "EnrollmentID", s(""))),
        Err(Error::Empty("EnrollmentID"))
    );
}

#[test]
fn a_result_with_an_empty_identity_or_idle_command_uuid_is_refused() {
    let ack = fixture!("result-acknowledged");
    assert_eq!(
        CommandResult::parse(&with(ack, "UDID", s(""))),
        Err(Error::Empty("UDID"))
    );
    assert_eq!(
        CommandResult::parse(&with(&without(ack, "UDID"), "EnrollmentID", s(""))),
        Err(Error::Empty("EnrollmentID"))
    );
    assert_eq!(
        CommandResult::parse(&with(fixture!("result-idle"), "CommandUUID", s(""))),
        Err(Error::Empty("CommandUUID"))
    );
}

#[test]
fn bootstrap_token_messages_carry_the_device_identity_when_sent() {
    for kind in ["checkin-getbootstraptoken", "checkin-setbootstraptoken"] {
        let plain = match CheckIn::parse(&without_none(kind)).unwrap() {
            CheckIn::GetBootstrapToken(g) => g.id,
            CheckIn::SetBootstrapToken(t) => t.id,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(plain, None);
        let named = with(&without_none(kind), "UDID", s(UDID));
        let id = match CheckIn::parse(&named).unwrap() {
            CheckIn::GetBootstrapToken(g) => g.id,
            CheckIn::SetBootstrapToken(t) => t.id,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(id.unwrap().key(), UDID);
        assert_eq!(
            CheckIn::parse(&with(&without_none(kind), "UDID", s(""))),
            Err(Error::Empty("UDID"))
        );
    }
}

fn without_none(kind: &str) -> Vec<u8> {
    match kind {
        "checkin-getbootstraptoken" => fixture!("checkin-getbootstraptoken").to_vec(),
        _ => fixture!("checkin-setbootstraptoken").to_vec(),
    }
}

#[test]
fn an_error_quotes_at_most_max_quoted_characters_of_what_a_mac_sent() {
    let long = "A".repeat(1_000_000);
    let e =
        CheckIn::parse(&with(fixture!("checkin-checkout"), "MessageType", s(&long))).unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "unknown check-in message type {:?}",
            "A".repeat(hostkeeper_mdm_server::MAX_QUOTED)
        )
    );
    let e = CommandResult::parse(&with(fixture!("result-acknowledged"), "Status", s(&long)))
        .unwrap_err();
    assert!(e.to_string().len() < 100, "{e}");
    let e = CheckIn::parse(&[b'<'; 100_000]).unwrap_err();
    assert!(e.to_string().chars().count() < 100, "{e}");
}
