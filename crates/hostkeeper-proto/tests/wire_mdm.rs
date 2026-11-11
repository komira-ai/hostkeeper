//! The names on the wire of the MDM policy gate's operator API, pinned: the `Mdm` service's RPC,
//! its refusal tokens, the phases of a device, and the fields of its messages by name and number.
//! The rest of the pinned names are in `wire.rs` and `wire_plan.rs`.

use prost_reflect::DescriptorPool;

/// The tokens of an enum: each value's name without the enum's prefix, in lower case.
fn tokens(pool: &DescriptorPool, name: &str, prefix: &str) -> Vec<(String, i32)> {
    pool.get_enum_by_name(name)
        .expect("the enum")
        .values()
        .map(|v| {
            let token = v
                .name()
                .strip_prefix(prefix)
                .expect("the prefix")
                .to_ascii_lowercase();
            (token, v.number())
        })
        .collect()
}

/// Each field of a message, as its name, number and whether it tracks presence (`optional`),
/// in the file's order.
fn fields_of(pool: &DescriptorPool, message: &str) -> Vec<(String, u32, bool)> {
    pool.get_message_by_name(message)
        .expect("the message")
        .fields()
        .map(|f| (f.name().to_owned(), f.number(), f.supports_presence()))
        .collect()
}

/// A field's name, number and presence: `true` is a field whose absence differs from its zero.
type Field = (&'static str, u32, bool);

/// The fields of the messages of `Mdm`, pinned by name, number and presence.
const FIELDS: [(&str, &[Field]); 4] = [
    (
        "hostkeeper.operator.v1.ListDevicesRequest",
        &[("limit", 1, false), ("page_token", 2, false)],
    ),
    (
        "hostkeeper.operator.v1.ListDevicesResponse",
        &[
            ("devices", 1, false),
            ("next_page_token", 2, false),
            ("gate", 3, false),
        ],
    ),
    (
        "hostkeeper.operator.v1.MdmCommand",
        &[
            ("request_type", 1, false),
            ("command_uuid", 2, false),
            ("sent_unix", 3, false),
        ],
    ),
    (
        "hostkeeper.operator.v1.MdmDevice",
        &[
            ("enrollment_id", 1, false),
            ("udid", 2, false),
            ("serial", 3, false),
            ("authenticate_serial", 4, false),
            ("phase", 5, false),
            ("enrollments", 6, false),
            ("supervised", 7, true),
            ("enrolled_via_dep", 8, true),
            ("user_approved", 9, true),
            ("enrolled_since_unix", 10, true),
            ("last_checkin_unix", 11, false),
            ("checked_out_unix", 12, true),
            ("device_information_unix", 13, true),
            ("security_info_unix", 14, true),
            ("unanswered", 15, false),
            ("last_error", 16, false),
            ("install_sends", 17, false),
            ("install_last_sent_unix", 18, true),
            ("install_sha256", 19, false),
            ("baseline_state", 20, false),
            ("baseline_reason", 21, false),
        ],
    ),
];

/// Catches: a name of the MDM gate's API renamed or renumbered without this test changing with
/// it: the `Mdm` RPC with its messages, the refusal tokens and their numbers, the phases, and
/// the fields of the device with the ones that can be absent. Callers branch on each, and the
/// operator client and the coordinator are built against them apart.
#[test]
fn wire_names_of_the_mdm_api_are_pinned() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");

    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Mdm")
        .expect("the service");
    let rpcs: Vec<String> = service
        .methods()
        .map(|m| format!("{}({}) {}", m.name(), m.input().name(), m.output().name()))
        .collect();
    assert_eq!(
        rpcs,
        ["ListDevices(ListDevicesRequest) ListDevicesResponse"]
    );

    let owned = |pairs: &[(&str, i32)]| -> Vec<(String, i32)> {
        pairs.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect()
    };
    assert_eq!(
        tokens(&pool, "hostkeeper.operator.v1.MdmRefusal", "MDM_REFUSAL_"),
        owned(&[
            ("unspecified", 0),
            ("unauthorized", 1),
            ("forbidden", 2),
            ("invalid_request", 3),
        ])
    );
    assert_eq!(
        tokens(&pool, "hostkeeper.operator.v1.MdmPhase", "MDM_PHASE_"),
        owned(&[
            ("unspecified", 0),
            ("authenticated", 1),
            ("enrolled", 2),
            ("checked_out", 3),
        ])
    );
    assert_eq!(
        tokens(
            &pool,
            "hostkeeper.operator.v1.MdmBaselineState",
            "MDM_BASELINE_STATE_"
        ),
        owned(&[
            ("unspecified", 0),
            ("pending", 1),
            ("applied", 2),
            ("failed", 3),
        ])
    );

    for (message, want) in FIELDS {
        let want: Vec<(String, u32, bool)> = want
            .iter()
            .map(|(name, number, presence)| ((*name).to_owned(), *number, *presence))
            .collect();
        assert_eq!(fields_of(&pool, message), want, "{message}");
    }
}
