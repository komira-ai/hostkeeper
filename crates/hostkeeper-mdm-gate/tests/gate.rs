//! The gate's device side against a fake NanoHUB, as design section 10 names its tests (task
//! M8): enrollment from the check-ins, supervision from `DeviceInformation` alone, a device
//! without a serial, a serial under two enrollments, the seven command types, and a restart.

mod support;

use hostkeeper_mdm_gate::{
    COMMAND_TYPES, Command, CommandUuid, DeviceStatus, EnqueueError, Phase, StoreError, Unavailable,
};
use plist::Dictionary;
use support::{
    OTHER_UDID, Rig, SERIAL, UDID, authenticate, check_out, device_information, id, recorded,
    recorded_device_information, security_info, token_update,
};

/// Enrolled and supervised, in an enrollment that began at `since`.
const fn enrolled_supervised(since: u64) -> DeviceStatus {
    DeviceStatus {
        enrolled: true,
        supervised: true,
        enrolled_since_unix: Some(since),
        enrolled_via_dep: None,
    }
}

/// Catches: a device counted enrolled before its `TokenUpdate`, or still enrolled after its
/// `CheckOut`; a re-enrollment that keeps what the device said in its last enrollment (its
/// serial and supervision), or that is not asked again.
#[tokio::test]
async fn a_device_is_enrolled_from_token_update_until_check_out() {
    let rig = Rig::new("enrolled").await;
    // The recorded `Authenticate`, with a serial: not enrolled, and nothing is sent yet.
    rig.post(&recorded("Authenticate.2")).await;
    let record = rig.record(UDID);
    assert_eq!(record.phase, Phase::Authenticated);
    assert_eq!(record.authenticate_serial.as_deref(), Some(SERIAL));
    assert!(rig.hub.commands().is_empty());
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Unknown));

    rig.clock.set(1_010);
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert_eq!(record.phase, Phase::Enrolled);
    assert_eq!(
        (record.enrollment, record.enrolled_since_unix),
        (1, Some(1_010))
    );
    let sent: Vec<String> = rig
        .hub
        .commands()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(sent, ["DeviceInformation", "SecurityInfo"]);

    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_010)));

    // A later `TokenUpdate` of the same enrollment changes nothing.
    rig.post(&token_update(UDID, 2)).await;
    assert_eq!(rig.record(UDID).enrollment, 1);
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_010)));

    rig.clock.set(1_020);
    rig.post(&check_out(UDID)).await;
    let record = rig.record(UDID);
    assert_eq!(
        (record.phase, record.checked_out_unix),
        (Phase::CheckedOut, Some(1_020))
    );
    assert!(
        record.sent.is_empty(),
        "a checked-out device's commands are dropped"
    );
    assert_eq!(
        rig.gate.status(SERIAL),
        Ok(DeviceStatus {
            enrolled: false,
            supervised: true,
            enrolled_since_unix: Some(1_010),
            enrolled_via_dep: None,
        })
    );

    // Enrolled again (an erase, then ADE): what it said before is forgotten until it answers.
    rig.post(&authenticate(UDID, Some(SERIAL))).await;
    let record = rig.record(UDID);
    assert_eq!((record.serial, record.supervised), (None, None));
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Unknown));
    rig.clock.set(1_030);
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert_eq!(
        (record.enrollment, record.enrolled_since_unix),
        (2, Some(1_030))
    );
    assert_eq!(rig.hub.commands().len(), 4, "both queries are asked again");
    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_030)));

    // A `TokenUpdate` NanoMDM counts first, whose `Authenticate` the gate did not hear, is a new
    // enrollment too.
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert_eq!((record.enrollment, record.serial), (3, None));
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Unknown));

    // A `TokenUpdate` after a `CheckOut`, with no `Authenticate` between, is a new enrollment.
    rig.post(&check_out(UDID)).await;
    rig.post(&token_update(UDID, 7)).await;
    assert_eq!(rig.record(UDID).enrollment, 4);
}

/// Catches: a device made supervised by a check-in, by its `SecurityInfo` (enrolled through
/// ADE), or by a `DeviceInformation` answer that says `IsSupervised` false or nothing.
#[tokio::test]
async fn supervised_comes_from_device_information_and_nothing_else() {
    let rig = Rig::new("supervised").await;
    rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "SecurityInfo");
    rig.post(&security_info(UDID, &uuid, true, true)).await;
    let record = rig.record(UDID);
    assert_eq!(
        (
            record.supervised,
            record.enrolled_via_dep,
            record.user_approved
        ),
        (None, Some(true), Some(true))
    );
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Unknown));

    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(false)))
        .await;
    let not_supervised = DeviceStatus {
        enrolled: true,
        supervised: false,
        enrolled_since_unix: Some(1_000),
        enrolled_via_dep: Some(true),
    };
    assert_eq!(rig.gate.status(SERIAL), Ok(not_supervised));

    // An answer with no `IsSupervised` is not supervised either.
    rig.post(&authenticate(OTHER_UDID, None)).await;
    rig.post(&token_update(OTHER_UDID, 1)).await;
    let uuid = rig.sent(OTHER_UDID, "DeviceInformation");
    rig.post(&device_information(
        OTHER_UDID,
        &uuid,
        Some("C02OTHER0001"),
        None,
    ))
    .await;
    // It has not answered `SecurityInfo`, so its ADE state is unknown.
    assert_eq!(
        rig.gate.status("C02OTHER0001"),
        Ok(DeviceStatus {
            enrolled_via_dep: None,
            ..not_supervised
        })
    );
}

/// Catches: a serial taken from the `Authenticate` alone, or a `DeviceInformation` answer with
/// no serial (the recorded one) matched to the serial the `Authenticate` gave.
#[tokio::test]
async fn a_device_without_a_serial_is_unavailable() {
    let rig = Rig::new("no-serial").await;
    rig.post(&recorded("Authenticate.2")).await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&recorded_device_information(&uuid)).await;
    let record = rig.record(UDID);
    assert_eq!(
        record.device_information_unix,
        Some(1_000),
        "the answer was read"
    );
    assert_eq!(record.serial, None);
    assert_eq!(record.authenticate_serial.as_deref(), Some(SERIAL));
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Unknown));

    // An answer whose serial is not a serial (not 1 to 64 letters and digits) is none either.
    rig.post(&authenticate(OTHER_UDID, None)).await;
    rig.post(&token_update(OTHER_UDID, 1)).await;
    let uuid = rig.sent(OTHER_UDID, "DeviceInformation");
    rig.post(&device_information(
        OTHER_UDID,
        &uuid,
        Some("C02 BAD/1"),
        Some(true),
    ))
    .await;
    assert_eq!(rig.record(OTHER_UDID).serial, None);
    assert_eq!(rig.gate.status("C02 BAD/1"), Err(Unavailable::Unknown));
}

/// Catches: a serial two live enrollments answered being given the status of either one; an
/// ambiguity that stays after the older enrollment checks out.
#[tokio::test]
async fn a_serial_under_two_enrollments_is_unavailable() {
    let rig = Rig::new("two-enrollments").await;
    rig.enroll(UDID, SERIAL, true).await;
    assert_eq!(
        rig.gate.status(SERIAL),
        Ok(DeviceStatus {
            enrolled_via_dep: Some(true),
            ..enrolled_supervised(1_000)
        })
    );
    rig.enroll(OTHER_UDID, SERIAL, false).await;
    assert_eq!(rig.gate.status(SERIAL), Err(Unavailable::Ambiguous));
    rig.post(&check_out(UDID)).await;
    assert_eq!(
        rig.gate.status(SERIAL),
        Ok(DeviceStatus {
            enrolled: true,
            supervised: false,
            enrolled_since_unix: Some(1_000),
            enrolled_via_dep: Some(true),
        }),
        "once the older checks out, the serial is the newer enrollment's"
    );
}

/// Catches: a ninth command type reaching NanoHUB, or the list of types changing without this
/// test. Every type the gate sends in a whole enrollment is on the list; each of the eight is
/// queued; `EraseDevice` (and any other type) is refused before NanoHUB is called.
#[tokio::test]
async fn the_gate_enqueues_only_its_eight_command_types() {
    assert_eq!(
        COMMAND_TYPES,
        [
            "DeviceInformation",
            "SecurityInfo",
            "InstallProfile",
            "RemoveProfile",
            "InstallEnterpriseApplication",
            "EnableRemoteDesktop",
            "ProfileList",
            "Settings"
        ]
    );
    let rig = Rig::new("eight-types").await;
    rig.enroll(UDID, SERIAL, true).await;
    let kinds: Vec<String> = rig
        .hub
        .commands()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(kinds, ["DeviceInformation", "SecurityInfo"]);

    let client = rig.hub.client();
    for (n, kind) in COMMAND_TYPES.iter().enumerate() {
        let uuid = CommandUuid::from_random([u8::try_from(n).expect("small"); 16]);
        let command = Command::new(kind, uuid, Dictionary::new());
        assert!(client.enqueue(&id(UDID), &command).await.is_ok(), "{kind}");
    }
    let heard = rig.hub.heard();
    assert_eq!(heard.len(), 10);
    for (kind, uuid) in ["EraseDevice", "DeviceLock", "deviceinformation", ""]
        .into_iter()
        .zip(10u8..)
    {
        let command = Command::new(
            kind,
            CommandUuid::from_random([uuid; 16]),
            Dictionary::new(),
        );
        assert_eq!(
            client.enqueue(&id(UDID), &command).await,
            Err(EnqueueError::NotAllowed(kind.to_owned()))
        );
    }
    assert_eq!(rig.hub.heard().len(), 10, "NanoHUB heard none of them");
    for (heard, kind) in heard.iter().skip(2).zip(COMMAND_TYPES) {
        assert_eq!(heard.command().0, kind);
        assert_eq!(heard.method, "PUT");
        assert_eq!(heard.path, format!("/api/v1/nanomdm/enqueue/{UDID}"));
    }
}

/// Catches: a record kept in memory only (lost at a restart), a result of a command sent before
/// the restart not matched after it, and a torn or changed record file read as a device (every
/// cut of the file, and one changed byte, refuse the start).
#[tokio::test]
async fn a_device_record_survives_a_coordinator_restart() {
    let mut rig = Rig::new("restart").await;
    rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.post(&token_update(UDID, 1)).await;
    let before = rig.gate.records();
    let uuid = rig.sent(UDID, "DeviceInformation");

    rig.restart().expect("the gate opens again");
    assert_eq!(rig.gate.records(), before);
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_000)));
    assert_eq!(rig.hub.commands().len(), 2, "nothing was sent again");
    rig.restart().expect("the gate opens again");
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_000)));

    let path = rig.dir().join(format!("{UDID}.pb"));
    let whole = std::fs::read(&path).expect("the record file");
    for cut in 0..whole.len() {
        std::fs::write(&path, &whole[..cut]).expect("cut the file");
        let refused = rig.restart().expect_err("a torn record is refused");
        let reason = match refused {
            StoreError::Corrupt { reason, .. } => reason,
            other => panic!("{other}"),
        };
        assert!(
            [
                "the file is not a record",
                "the file has no schema",
                "the record does not match its digest"
            ]
            .contains(&reason.as_str()),
            "cut at {cut}: {reason}"
        );
    }
    let mut changed = whole.clone();
    changed[10] ^= 0x01;
    std::fs::write(&path, &changed).expect("change the file");
    assert_eq!(
        rig.restart()
            .expect_err("a changed record is refused")
            .to_string(),
        format!(
            "{}: the record does not match its digest; the gate does not guess what a device's record said, so fix or remove the file to start",
            path.display()
        )
    );
    std::fs::write(&path, &whole).expect("put the file back");
    rig.restart().expect("the whole file opens");
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_000)));
}

/// Catches `enrolled_via_dep` taken from anything but the enrollment's own `SecurityInfo`
/// answer: it is `None` before the answer, `Some(true)` and `Some(false)` as the answer says, and
/// forgotten by a new enrollment of the device.
#[tokio::test]
async fn status_carries_enrolled_via_dep_from_security_info() {
    let rig = Rig::new("via-dep").await;
    rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    // No `SecurityInfo` answer yet: unknown, not false.
    assert_eq!(rig.gate.status(SERIAL), Ok(enrolled_supervised(1_000)));

    let uuid = rig.sent(UDID, "SecurityInfo");
    rig.post(&security_info(UDID, &uuid, true, false)).await;
    assert_eq!(
        rig.gate
            .status(SERIAL)
            .map(|status| status.enrolled_via_dep),
        Ok(Some(true))
    );

    // A new enrollment forgets it, and a manual one says false.
    rig.post(&authenticate(UDID, Some(SERIAL))).await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&device_information(UDID, &uuid, Some(SERIAL), Some(true)))
        .await;
    assert_eq!(
        rig.gate
            .status(SERIAL)
            .map(|status| status.enrolled_via_dep),
        Ok(None)
    );
    let uuid = rig.sent(UDID, "SecurityInfo");
    rig.post(&security_info(UDID, &uuid, false, true)).await;
    assert_eq!(
        rig.gate
            .status(SERIAL)
            .map(|status| status.enrolled_via_dep),
        Ok(Some(false))
    );
}
