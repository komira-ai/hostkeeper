//! The messages of the MDM policy gate's operator API, added since the committed baseline
//! (`proto/compat/baseline.binpb`): each reads back as it was written, an absent optional stays
//! absent, and a reader built from the baseline, the release before, keeps what it does not
//! know and writes it back unchanged.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// What `ListSecrets` shows of a secret, every field set.
fn secret_info() -> op::SecretInfo {
    op::SecretInfo {
        kind: "push_key".to_owned(),
        version: 2,
        fingerprint_sha256: "ab".repeat(32),
        not_after_unix: Some(1_900_000_000),
        created_unix: 1_800_000_000,
        superseded_unix: Some(1_800_000_500),
    }
}

/// A device with every field set.
fn device() -> op::MdmDevice {
    op::MdmDevice {
        enrollment_id: "66ADE930-5FDF-5EC4-8429-15640684C489".to_owned(),
        udid: "66ADE930-5FDF-5EC4-8429-15640684C489".to_owned(),
        serial: "C02XK1ABJG5J".to_owned(),
        authenticate_serial: "C02XK1ABJG5J".to_owned(),
        phase: op::MdmPhase::Enrolled.into(),
        enrollments: 2,
        supervised: Some(true),
        enrolled_via_dep: Some(true),
        user_approved: Some(false),
        enrolled_since_unix: Some(1_800_000_000),
        last_checkin_unix: 1_800_000_100,
        checked_out_unix: Some(1_800_000_200),
        device_information_unix: Some(1_800_000_010),
        security_info_unix: Some(1_800_000_020),
        unanswered: vec![op::MdmCommand {
            request_type: "SecurityInfo".to_owned(),
            command_uuid: "A1A1A1A1-2222-4333-8444-555555555555".to_owned(),
            sent_unix: 1_800_000_030,
        }],
        last_error: "NanoHUB did not queue the command".to_owned(),
        install_sends: 2,
        install_last_sent_unix: Some(1_800_000_040),
        install_sha256: "ab".repeat(32),
    }
}

/// One message of each type the MDM API added, every field set, by full name.
fn mdm_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.operator.v1.ListDevicesRequest",
            round_trip(&op::ListDevicesRequest {
                limit: 20,
                page_token: "66ADE930-5FDF-5EC4-8429-15640684C489".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListDevicesResponse",
            round_trip(&op::ListDevicesResponse {
                devices: vec![device()],
                next_page_token: "66ADE930-5FDF-5EC4-8429-15640684C489".to_owned(),
                gate: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.MdmCommand",
            round_trip(&device().unanswered[0]),
        ),
        ("hostkeeper.operator.v1.MdmDevice", round_trip(&device())),
        (
            "hostkeeper.operator.v1.PutSecretRequest",
            round_trip(&op::PutSecretRequest {
                kind: "abm_token".to_owned(),
                material: vec![0, 1, 2, 255],
                not_after_unix: Some(1_900_000_000),
            }),
        ),
        (
            "hostkeeper.operator.v1.SecretInfo",
            round_trip(&secret_info()),
        ),
        (
            "hostkeeper.operator.v1.PutSecretResponse",
            round_trip(&op::PutSecretResponse {
                secret: Some(secret_info()),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutSettingRequest",
            round_trip(&op::PutSettingRequest {
                name: "push_alert_days".to_owned(),
                value: "60,14".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutSettingResponse",
            round_trip(&op::PutSettingResponse {
                name: "push_alert_days".to_owned(),
                value: "60,14".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ListSecretsResponse",
            round_trip(&op::ListSecretsResponse {
                secrets: vec![secret_info()],
                sealing: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.MdmSetting",
            round_trip(&op::MdmSetting {
                name: "tls_renew_days".to_owned(),
                value: "30".to_owned(),
                is_default: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.ListSettingsResponse",
            round_trip(&op::ListSettingsResponse {
                settings: vec![op::MdmSetting {
                    name: "tls_renew_days".to_owned(),
                    value: "30".to_owned(),
                    is_default: false,
                }],
            }),
        ),
        (
            "hostkeeper.operator.v1.CreatePushRequestRequest",
            round_trip(&op::CreatePushRequestRequest {
                email: "ops@example.org".to_owned(),
                submit: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.CreatePushRequestResponse",
            round_trip(&op::CreatePushRequestResponse {
                csr_pem: "-----BEGIN CERTIFICATE REQUEST-----".to_owned(),
                encryption_certificate_pem: "-----BEGIN CERTIFICATE-----".to_owned(),
                submitted: true,
            }),
        ),
        (
            "hostkeeper.operator.v1.PutPushReplyRequest",
            round_trip(&op::PutPushReplyRequest {
                reply: vec![0x30, 0x80, 0xff],
            }),
        ),
        (
            "hostkeeper.operator.v1.PutPushReplyResponse",
            round_trip(&op::PutPushReplyResponse {
                signed_request: b"<plist/>".to_vec(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutPushCertificateRequest",
            round_trip(&op::PutPushCertificateRequest {
                certificate_pem: b"-----BEGIN CERTIFICATE-----".to_vec(),
                key_pem: b"-----BEGIN RSA PRIVATE KEY-----".to_vec(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutPushCertificateResponse",
            round_trip(&op::PutPushCertificateResponse {
                topic: "com.apple.mgmt.External.d1801ae1".to_owned(),
                not_before_unix: 1_800_000_000,
                not_after_unix: 1_900_000_000,
                secret: Some(secret_info()),
                renewal: true,
            }),
        ),
    ]
}

/// Catches: a message of the MDM API that does not read back as written, or that a reader of the
/// release before drops: it knows none of them, and a message it reads with no field keeps every
/// field of the new ones unknown and writes them back unchanged.
#[test]
fn the_mdm_messages_round_trip_and_an_older_reader_keeps_unknown_fields() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("released");
    assert_eq!(blind.fields().count(), 0);
    let messages = mdm_messages();
    assert_eq!(messages.len(), 18);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(!bytes.is_empty(), "{name} sets its fields");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    for name in [
        "hostkeeper.operator.v1.MdmRefusal",
        "hostkeeper.operator.v1.MdmPhase",
    ] {
        assert!(older.get_enum_by_name(name).is_none(), "{name} is new");
    }
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Mdm")
            .is_none()
    );
}

/// Catches: an optional field of a device that reads as zero or false when the device has not
/// said it: a device with none of them writes none, reads each as absent, and one set to false
/// or 0 is written and read back as set, so that "not supervised" and "not answered yet" differ.
#[test]
fn an_absent_optional_of_a_device_stays_absent_and_a_zero_is_written() {
    let bare = op::MdmDevice {
        enrollment_id: "AAAA".to_owned(),
        ..op::MdmDevice::default()
    };
    let read = op::MdmDevice::decode(round_trip(&bare).as_slice()).expect("decodes");
    assert_eq!(
        (
            read.supervised,
            read.enrolled_via_dep,
            read.user_approved,
            read.enrolled_since_unix,
            read.checked_out_unix,
            read.device_information_unix,
            read.security_info_unix,
            read.install_last_sent_unix,
        ),
        (None, None, None, None, None, None, None, None)
    );
    let zero = op::MdmDevice {
        supervised: Some(false),
        enrolled_via_dep: Some(false),
        user_approved: Some(false),
        enrolled_since_unix: Some(0),
        checked_out_unix: Some(0),
        device_information_unix: Some(0),
        security_info_unix: Some(0),
        install_last_sent_unix: Some(0),
        ..bare.clone()
    };
    let bytes = round_trip(&zero);
    assert!(bytes.len() > round_trip(&bare).len());
    let read = op::MdmDevice::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read, zero);
    assert_eq!(read.supervised, Some(false));
    assert_eq!(read.checked_out_unix, Some(0));
}

/// Catches: a secret's `not_after_unix` or `superseded_unix` that reads as 0 when absent, which a
/// status would show as expired in 1970 or replaced at once: a version in use writes neither and
/// reads both absent, and a zero is written and read back as set.
#[test]
fn an_absent_time_of_a_secret_stays_absent_and_a_zero_is_written() {
    let bare = op::SecretInfo {
        kind: "tls_key".to_owned(),
        version: 1,
        ..op::SecretInfo::default()
    };
    let read = op::SecretInfo::decode(round_trip(&bare).as_slice()).expect("decodes");
    assert_eq!((read.not_after_unix, read.superseded_unix), (None, None));
    let zero = op::SecretInfo {
        not_after_unix: Some(0),
        superseded_unix: Some(0),
        ..bare.clone()
    };
    let bytes = round_trip(&zero);
    assert!(bytes.len() > round_trip(&bare).len());
    let read = op::SecretInfo::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(
        (read.not_after_unix, read.superseded_unix),
        (Some(0), Some(0))
    );
    let request = op::PutSecretRequest::decode(
        round_trip(&op::PutSecretRequest {
            kind: "tls_key".to_owned(),
            material: vec![1],
            not_after_unix: None,
        })
        .as_slice(),
    )
    .expect("decodes");
    assert_eq!(request.not_after_unix, None);
}
