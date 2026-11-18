//! The names on the wire of service identity (`docs/design/service-identity.md`), pinned: the
//! RPCs of the coordinator's `Coordinator` service in declaration order, the `HostIdentity`
//! service of the operator API, their refusal tokens, and the fields of the new messages and the
//! new fields of old ones by name, number and presence. The rest of the pinned names are in
//! `wire.rs`.

use prost_reflect::DescriptorPool;

/// The descriptor set this build serves.
fn pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// The tokens of an enum: each value's name without the enum's prefix, in lower case, with its
/// number.
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

/// The methods of a service in declaration order, as `Name(Input) Output`.
fn rpcs(pool: &DescriptorPool, service: &str) -> Vec<String> {
    pool.get_service_by_name(service)
        .expect("the service")
        .methods()
        .map(|m| format!("{}({}) {}", m.name(), m.input().name(), m.output().name()))
        .collect()
}

/// Each field of a message, as its name, number and whether it tracks presence (`optional`) or
/// repeats, in the file's order.
fn fields_of(pool: &DescriptorPool, message: &str) -> Vec<(String, u32, &'static str)> {
    pool.get_message_by_name(message)
        .expect("the message")
        .fields()
        .map(|f| {
            let label = if f.is_list() {
                "repeated"
            } else if f.supports_presence() {
                "optional"
            } else {
                "-"
            };
            (f.name().to_owned(), f.number(), label)
        })
        .collect()
}

/// A field's name, number and label.
type Field = (&'static str, u32, &'static str);

/// Proves the coordinator's service has these RPCs in this order, the two of service identity
/// last, and that `HostIdentity` has `Get` and `Approve`.
/// Catches: an RPC renamed, retyped or reordered.
#[test]
fn the_rpcs_of_service_identity_are_pinned() {
    let pool = pool();
    assert_eq!(
        rpcs(&pool, "hostkeeper.agent.v1.Coordinator"),
        [
            "Heartbeat(HeartbeatRequest) HeartbeatResponse",
            "Enroll(EnrollRequest) EnrollResponse",
            "Poll(PollRequest) PollResponse",
            "Renew(RenewRequest) RenewResponse",
            "IssueServiceCertificate(IssueServiceCertificateRequest) IssueServiceCertificateResponse",
            "ListDeny(ListDenyRequest) ListDenyResponse",
        ]
    );
    assert_eq!(
        rpcs(&pool, "hostkeeper.operator.v1.HostIdentity"),
        [
            "Get(GetHostIdentityRequest) GetHostIdentityResponse",
            "Approve(ApproveHostIdentityRequest) ApproveHostIdentityResponse",
        ]
    );
}

/// Proves the new refusal tokens of the coordinator and the tokens of `HostIdentityRefusal` and
/// the values of `HostAssurance` are these, with these numbers.
/// Catches: a token renamed or renumbered, which a peer built apart would read as another.
#[test]
fn the_refusals_and_assurances_of_service_identity_are_pinned() {
    let pool = pool();
    let coordinator = tokens(
        &pool,
        "hostkeeper.agent.v1.CoordinatorRefusal",
        "COORDINATOR_REFUSAL_",
    );
    assert_eq!(
        coordinator[10..],
        [
            ("service_not_assigned".to_owned(), 10),
            ("identity_not_approved".to_owned(), 11),
            ("invalid_service".to_owned(), 12),
            ("store_failed".to_owned(), 13),
        ]
    );
    let want: Vec<(String, i32)> = [
        "unspecified",
        "unauthorized",
        "forbidden",
        "invalid_request",
        "unknown_host",
        "key_mismatch",
        "denied",
        "store_failed",
        "audit_unavailable",
    ]
    .iter()
    .zip(0..)
    .map(|(token, n)| ((*token).to_owned(), n))
    .collect();
    assert_eq!(
        tokens(
            &pool,
            "hostkeeper.operator.v1.HostIdentityRefusal",
            "HOST_IDENTITY_REFUSAL_"
        ),
        want
    );
    assert_eq!(
        tokens(
            &pool,
            "hostkeeper.operator.v1.HostAssurance",
            "HOST_ASSURANCE_"
        ),
        [
            ("unspecified".to_owned(), 0),
            ("operator".to_owned(), 1),
            ("auto".to_owned(), 2),
        ]
    );
}

/// Proves the new messages and the new fields of old ones have these names, numbers and labels.
/// Catches: a field renumbered, renamed or given another label than the schema says.
#[test]
fn the_fields_of_service_identity_are_pinned() {
    let pool = pool();
    let cases: [(&str, &[Field]); 11] = [
        (
            "hostkeeper.agent.v1.ServiceIdentityReport",
            &[
                ("service", 1, "-"),
                ("not_after_unix", 2, "optional"),
                ("last_error", 3, "optional"),
            ],
        ),
        (
            "hostkeeper.agent.v1.IssueServiceCertificateRequest",
            &[("service", 1, "-"), ("csr_der", 2, "-")],
        ),
        (
            "hostkeeper.agent.v1.IssueServiceCertificateResponse",
            &[("certificate_der", 1, "-"), ("ca_bundle_pem", 2, "-")],
        ),
        ("hostkeeper.agent.v1.ListDenyRequest", &[]),
        (
            "hostkeeper.agent.v1.ListDenyResponse",
            &[("host_ids", 1, "repeated")],
        ),
        (
            "hostkeeper.operator.v1.GetHostIdentityRequest",
            &[("node_id", 1, "-")],
        ),
        (
            "hostkeeper.operator.v1.GetHostIdentityResponse",
            &[
                ("node_id", 1, "-"),
                ("assurance", 2, "-"),
                ("key_sha256", 3, "-"),
                ("allowed", 4, "-"),
                ("approval", 5, "optional"),
                ("denied", 6, "-"),
            ],
        ),
        (
            "hostkeeper.operator.v1.IdentityApprovalInfo",
            &[("approver", 1, "-"), ("approved_unix", 2, "-")],
        ),
        (
            "hostkeeper.operator.v1.ApproveHostIdentityRequest",
            &[
                ("node_id", 1, "-"),
                ("key_sha256", 2, "-"),
                ("reason", 3, "-"),
            ],
        ),
        (
            "hostkeeper.operator.v1.ApproveHostIdentityResponse",
            &[("node_id", 1, "-"), ("approval", 2, "optional")],
        ),
        (
            "hostkeeper.release.v1.Manifest",
            &[
                ("identity", 10, "optional"),
                ("identity_account", 11, "optional"),
            ],
        ),
    ];
    for (name, want) in cases {
        let got = fields_of(&pool, name);
        let tail: Vec<(String, u32, &str)> = got
            .iter()
            .skip(got.len().saturating_sub(want.len()))
            .cloned()
            .collect();
        let want: Vec<(String, u32, &str)> = want
            .iter()
            .map(|(n, k, l)| ((*n).to_owned(), *k, *l))
            .collect();
        assert_eq!(tail, want, "{name}");
    }
    let beat = fields_of(&pool, "hostkeeper.agent.v1.HeartbeatRequest");
    assert_eq!(
        beat.last(),
        Some(&("service_identities".to_owned(), 17, "repeated"))
    );
}
