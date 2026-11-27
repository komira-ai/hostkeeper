//! The log tokens of the alert kinds, pinned: each `AlertKind` value's name without its prefix,
//! in lower case, is the `alert=` field of the coordinator's log line of that kind, and the
//! operator API's JSON spells the kind the same way. Moved here from `wire.rs`, unchanged but for
//! host health's kinds (25 to 32), to keep that file under 1000 lines.

use prost_reflect::DescriptorPool;

/// Catches: a kind renamed, renumbered or inserted before the others, which would change a log
/// token operators search for.
#[test]
fn alert_kind_tokens_are_pinned() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let kinds = pool
        .get_enum_by_name("hostkeeper.operator.v1.AlertKind")
        .expect("the enum");
    let tokens: Vec<(String, i32)> = kinds
        .values()
        .filter(|v| v.number() != 0)
        .map(|v| {
            let token = v.name().strip_prefix("ALERT_KIND_").expect("the prefix");
            (token.to_ascii_lowercase(), v.number())
        })
        .collect();
    let want: Vec<(String, i32)> = [
        "halt",
        "waiting_for_operator",
        "duplicate",
        "duplicate_claim_pending",
        "failed_canary",
        "held_on_bad_version",
        "abandoned_while_updating",
        "silent_host",
        "cert_near_expiry",
        "store_unreachable",
        "trusted_root",
        "push_cert_near_expiry",
        "mdm_tls_renewal_failed",
        "mdm_dns_credential_rejected",
        "mdm_identity_near_expiry",
        "prerequisite_failed",
        "xcode_release_waiting",
        "xcode_version_taken",
        "updates_due",
        "failed_update",
        "host_halt",
        "maintenance_halted",
        "maintenance_not_back",
        "repeated_deferral",
        "hardware_fault",
        "capacity_regressed",
        "storage_failing",
        "unexpected_reboot",
        "service_unhealthy",
        "service_crash_loop",
        "host_not_monitored",
        "bmc_missing",
    ]
    .iter()
    .zip(1..)
    .map(|(token, number)| ((*token).to_owned(), number))
    .collect();
    assert_eq!(tokens, want);
}
