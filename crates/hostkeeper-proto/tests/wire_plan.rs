//! The names on the wire of the update plan, pinned: the `Rollouts` service's RPCs, the fields of
//! the plan, its messages and the two views that carry it, and the plan's enums and refusal
//! tokens. The rest of the pinned names are in `wire.rs`.

use prost_reflect::DescriptorPool;

/// The tokens of an enum: each value's name without the enum's prefix, in lower case.
fn tokens(pool: &DescriptorPool, name: &str, prefix: &str) -> Vec<String> {
    pool.get_enum_by_name(name)
        .expect("the enum")
        .values()
        .filter(|v| v.number() != 0)
        .map(|v| {
            v.name()
                .strip_prefix(prefix)
                .expect("the prefix")
                .to_ascii_lowercase()
        })
        .collect()
}

/// Each RPC of a service, as `Name(Request) Response`, in the file's order.
fn rpcs(pool: &DescriptorPool, service: &str) -> Vec<String> {
    pool.get_service_by_name(service)
        .expect("the service")
        .methods()
        .map(|m| format!("{}({}) {}", m.name(), m.input().name(), m.output().name()))
        .collect()
}

/// Each field of a message, as its name and number, in the file's order.
fn fields_of(pool: &DescriptorPool, message: &str) -> Vec<(String, u32)> {
    pool.get_message_by_name(message)
        .expect("the message")
        .fields()
        .map(|f| (f.name().to_owned(), f.number()))
        .collect()
}

/// The fields of the plan's messages and of the two views that carry it, pinned by name and
/// number.
const FIELDS: [(&str, &[(&str, u32)]); 13] = [
    (
        "hostkeeper.operator.v1.AdvanceRequest",
        &[("service", 1), ("reason", 2), ("host_group", 3)],
    ),
    (
        "hostkeeper.operator.v1.AdvanceResponse",
        &[("entry", 1), ("plan", 2)],
    ),
    (
        "hostkeeper.operator.v1.PauseRequest",
        &[("service", 1), ("reason", 2), ("host_group", 3)],
    ),
    (
        "hostkeeper.operator.v1.PauseResponse",
        &[("entry", 1), ("plan", 2)],
    ),
    (
        "hostkeeper.operator.v1.ResumeRequest",
        &[("service", 1), ("reason", 2), ("host_group", 3)],
    ),
    (
        "hostkeeper.operator.v1.ResumeResponse",
        &[("entry", 1), ("plan", 2)],
    ),
    (
        "hostkeeper.operator.v1.RolloutPlan",
        &[
            ("version", 1),
            ("stages", 2),
            ("current", 3),
            ("eligible", 4),
            ("waiting", 5),
            ("paused", 6),
            ("paused_by", 7),
            ("pause_reason", 8),
            ("finished", 9),
            ("waiting_on", 10),
            ("host_group", 11),
            ("waiting_on_why", 12),
        ],
    ),
    (
        "hostkeeper.operator.v1.LiveRollout",
        &[("service", 1), ("reason", 2), ("node_id", 3)],
    ),
    (
        "hostkeeper.operator.v1.PlanStage",
        &[
            ("number", 1),
            ("percent", 2),
            ("hosts", 3),
            ("advance", 4),
            ("members", 5),
        ],
    ),
    (
        "hostkeeper.operator.v1.PlanHost",
        &[("node_id", 1), ("role", 2), ("state", 3), ("deferring", 4)],
    ),
    (
        "hostkeeper.operator.v1.HostService",
        &[
            ("service", 1),
            ("up", 2),
            ("holds_slot", 3),
            ("gate_reason", 4),
            ("membership", 5),
            ("holds", 6),
            ("installed_version", 7),
            ("health", 8),
            ("last_update", 9),
            ("excluded_reason", 10),
            ("stage", 11),
            ("order", 12),
        ],
    ),
    (
        "hostkeeper.operator.v1.CurrentOrder",
        &[
            ("order_id", 1),
            ("order_try", 2),
            ("version", 3),
            ("uninstall", 4),
        ],
    ),
    (
        "hostkeeper.operator.v1.ServiceStatus",
        &[
            ("service", 1),
            ("desired", 2),
            ("convergence", 3),
            ("hosts", 4),
            ("up", 5),
            ("slots_held", 6),
            ("marks", 7),
            ("plan", 8),
            ("host_group", 9),
        ],
    ),
];

/// Catches: a name of the update plan renamed or renumbered without this test changing with it:
/// the `Rollouts` RPCs with their messages, the fields of the plan, the stage, the plan's host and
/// the `plan` and `stage` fields of the views, and the tokens of the plan's refusals and enums.
/// Callers branch on each, and the operator client and the coordinator are built against them
/// apart.
#[test]
fn wire_names_of_the_update_plan_are_pinned() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");

    let cases: [(&str, &str, &[&str]); 5] = [
        (
            "hostkeeper.operator.v1.RolloutsRefusal",
            "ROLLOUTS_REFUSAL_",
            &[
                "unauthorized",
                "forbidden",
                "invalid_request",
                "unknown_service",
                "no_desired",
                "not_waiting",
                "already_paused",
                "not_paused",
                "store_failed",
                "audit_unavailable",
                "ambiguous_host_group",
                "not_in_host_group",
            ],
        ),
        (
            "hostkeeper.operator.v1.StageAdvance",
            "STAGE_ADVANCE_",
            &["auto", "manual"],
        ),
        (
            "hostkeeper.operator.v1.PlanHostState",
            "PLAN_HOST_STATE_",
            &["waiting", "updating", "baking", "done", "failed"],
        ),
        (
            "hostkeeper.operator.v1.PlanWait",
            "PLAN_WAIT_",
            &[
                "none",
                "halted",
                "paused",
                "quiet",
                "rollout_elsewhere",
                "manual_advance",
                "failed_host",
                "below_min_up",
                "updating",
                "deferring",
                "baking",
            ],
        ),
        (
            "hostkeeper.operator.v1.LiveReason",
            "LIVE_REASON_",
            &[
                "updating",
                "promotion_pending",
                "ordered",
                "baking",
                "plan_under_way",
            ],
        ),
    ];
    for (name, prefix, want) in cases {
        assert_eq!(tokens(&pool, name, prefix), want, "{name}");
    }

    assert_eq!(
        rpcs(&pool, "hostkeeper.operator.v1.Rollouts"),
        [
            "Advance(AdvanceRequest) AdvanceResponse",
            "Pause(PauseRequest) PauseResponse",
            "Resume(ResumeRequest) ResumeResponse",
        ]
    );
    for (message, want) in FIELDS {
        let want: Vec<(String, u32)> = want.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect();
        assert_eq!(fields_of(&pool, message), want, "{message}");
    }
}
