//! A kbf defined by a rollout file end to end: what convergence and the offers do with the
//! definition, the rules for the file that carries it, and the spelling of the hold reasons in
//! the status.

use std::os::unix::fs::PermissionsExt;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat, ServiceReply};
use serde_json::json;

use crate::converge::State;
use crate::decide::{Gate, Skip, Why};
use crate::identity::Identity;
use crate::services::{Services, ServicesError};
use crate::testutil::{OPERATOR, Rig, beat, ci_token, id, role, scratch, version};

fn who(name: &str, role: RoleName) -> Identity {
    Identity {
        host_id: id(name),
        role,
    }
}

/// `top` that also runs `service` at `running`.
fn announcing_other(mut top: Heartbeat, service: &str, running: &str) -> Heartbeat {
    top.services = Some(vec![ServiceBeat {
        name: service.to_owned(),
        version: version(running),
        health: Health::Healthy,
        last_update: None,
        updating: None,
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
        record_try: None,
    }]);
    top
}

fn refusal(text: &str) -> String {
    Services::parse(text).expect_err("refused").to_string()
}

/// A rig whose kbf is replaced by `text`.
fn replaced(name: &str, text: &str) -> Rig {
    let mut rig = Rig::new(name);
    rig.coordinator =
        Rig::start(&rig.dir, &rig.clock).with_services(Services::parse(text).expect("services"));
    rig
}

/// Catches: a replaced kbf ignored by convergence. kbf is replaced by a definition with
/// storage and daemon only; the desired state asks for a frontend to stay up (as deploy-farm's
/// body always does) and a stray frontend host, which no order can reach, runs the old
/// version. The farm of the replacement is at the desired version, so the convergence read
/// and the status both say `converged`, over the roles of the replacement and no other.
#[tokio::test]
async fn a_replaced_kbf_converges_over_its_own_roles() {
    let rig = replaced(
        "kbf-replaced-converge",
        "[[service]]\nname = \"kbf\"\nroles = [\"storage\", \"daemon\"]\n",
    );
    rig.set(r#"{"version":"2.0","min_up":{"storage":1,"frontend":1}}"#)
        .await;
    rig.at(2_000);
    let c = &rig.coordinator;
    for (name, kind, running) in [
        ("s1", "storage", "2.0"),
        ("d1", "daemon", "2.0"),
        ("f1", "frontend", "1.0"),
    ] {
        c.heartbeat(who(name, role(kind)), beat(running))
            .expect("answered");
    }
    let read = c
        .convergence(Some(&ci_token()), "kbf")
        .await
        .expect("convergence");
    assert_eq!(read.state, State::Converged);
    let roles: Vec<&str> = read.roles.iter().map(|r| r.role.as_str()).collect();
    assert_eq!(roles, ["storage", "daemon"]);
    let status = serde_json::to_value(c.status(Some(OPERATOR)).expect("status")).expect("JSON");
    let entry = &status["per_service"]["kbf"];
    assert_eq!(entry["roles"], entry["convergence"]["roles"]);
    assert_eq!(entry["convergence"]["state"], "converged");
}

/// Behaviour: the priority of a replaced kbf orders the offers. With kbf at -1 and `other` at
/// 0, a host behind on both is offered `other`, named; with kbf's priority left at 0 it is
/// offered kbf, unnamed. Catches a priority the replacement cannot set, or one that is read
/// and not used for kbf.
#[tokio::test]
async fn the_priority_of_a_replaced_kbf_orders_the_offers() {
    let other = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";
    for (priority, want_service) in [(-1, Some("other")), (0, None)] {
        let text = format!(
            "[[service]]\nname = \"kbf\"\npriority = {priority}\nroles = [\"daemon\"]\n{other}"
        );
        let mut rig = replaced("kbf-replaced-priority", &text);
        let list = hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
            + "services = [\"kbf\", \"other\"]\n";
        rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
            .with_services(Services::parse(&text).expect("services"));
        for body in [
            r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#,
            r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#,
        ] {
            rig.coordinator
                .set_desired(Some(&ci_token()), body.as_bytes())
                .await
                .expect("set");
        }
        rig.at(2_000);
        let reply = rig
            .coordinator
            .heartbeat(
                who("d1", role("daemon")),
                announcing_other(beat("1.0"), "other", "1.0"),
            )
            .expect("answered");
        assert_eq!(
            reply,
            ServiceReply::Update {
                service: want_service.map(str::to_owned),
                version: version(if want_service.is_some() { "3.0" } else { "2.0" }),
            },
            "priority {priority}"
        );
    }
}

/// Behaviour: a kbf defined by the file names roles by the same rule as any service: a name
/// that is no role name is refused, and says which; a well-formed one the old four did not
/// include is a role.
#[test]
fn a_kbf_with_a_malformed_role_is_refused_and_an_open_role_is_not() {
    assert!(
        refusal("[[service]]\nname = \"kbf\"\nroles = [\"Cook\"]\n")
            .contains("Cook is not a role name")
    );
    Services::parse("[[service]]\nname = \"kbf\"\nroles = [\"cook\"]\n").expect("roles are open");
}

/// Behaviour: kbf has no place of its own in the file: after another service it is defined
/// like any, and a second definition of it is refused with the text for any repeated name.
#[test]
fn kbf_after_another_service_is_defined_and_kbf_twice_is_refused() {
    let daemon = "roles = [\"daemon\"]\n";
    let after_other =
        format!("[[service]]\nname = \"a\"\n{daemon}[[service]]\nname = \"kbf\"\n{daemon}");
    let services = Services::parse(&after_other).expect("parsed");
    assert_eq!(services.names().collect::<Vec<_>>(), ["a", "kbf"]);
    let twice =
        format!("[[service]]\nname = \"kbf\"\n{daemon}[[service]]\nname = \"kbf\"\n{daemon}");
    assert_eq!(refusal(&twice), "services: kbf is defined twice");
}

/// Behaviour: a rollout file is read under the rule of the operator's token file: a regular
/// file owned by the coordinator's user, mode 0600 or 0400. A file others can write could
/// switch off every limit of kbf's rollout, so mode 0644 and 0660 are refused with the
/// mode named, and 0600 and 0400 are read.
#[test]
fn a_rollout_file_must_be_owner_only() {
    let dir = scratch("rollout-file-mode");
    let path = dir.path().join("services.toml");
    std::fs::write(&path, "[[service]]\nname = \"a\"\nroles = [\"daemon\"]\n").expect("written");
    for (mode, ok) in [(0o644, false), (0o660, false), (0o600, true), (0o400, true)] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        let read = Services::from_file(&path);
        match (ok, read) {
            (true, Ok(services)) => assert!(services.get("a").is_some(), "{mode:o}"),
            (false, Err(ServicesError::Read(why))) => {
                assert!(why.contains("mode must be 0600 or 0400"), "{mode:o}: {why}");
            }
            (_, other) => panic!("mode {mode:o}: {other:?}"),
        }
    }
    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&path, &link).expect("link");
    assert!(matches!(
        Services::from_file(&link),
        Err(ServicesError::Read(_))
    ));
}

/// Behaviour: an owner-only rollout file that is not text, or is over a mebibyte, is refused
/// with the reason, not read as an empty file.
#[test]
fn a_rollout_file_must_be_text_and_small() {
    let dir = scratch("rollout-file-shape");
    let read = |name: &str, bytes: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        Services::from_file(&path).expect_err("refused").to_string()
    };
    assert!(read("binary.toml", &[0xff, 0xfe]).contains("not UTF-8"));
    let big = vec![b'#'; (1 << 20) + 1];
    assert!(read("big.toml", &big).contains("over 1048576 bytes"));
}

/// Pins: the strings the status gives for each hold are public: scripts match on them. The
/// matches have no wildcard, so a new variant fails to compile here until it is listed.
///
/// Catches: a renamed hold reason, which would break the scripts that match on the status.
#[test]
fn the_hold_strings_are_the_ones_the_status_documents() {
    let gate = |gate: Gate| -> &'static str {
        match gate {
            Gate::Halted => "halted",
            Gate::Held => "held",
            Gate::Bad => "bad",
            Gate::BelowMinVersion => "below_min_version",
            Gate::Updating => "updating",
            Gate::BusyElsewhere => "busy_elsewhere",
            Gate::NoSlot => "no_slot",
            Gate::Quiet => "quiet",
            Gate::CanaryWait => "canary_wait",
            Gate::Baking => "baking",
            Gate::Paused => "paused",
            Gate::AdvanceWait => "advance_wait",
            Gate::StageFailed => "stage_failed",
            Gate::StageWait => "stage_wait",
            Gate::BelowMinUp => "below_min_up",
            Gate::RolloutElsewhere => "rollout_elsewhere",
        }
    };
    let skip = |skip: Skip| -> &'static str {
        match skip {
            Skip::NoDesired => "no_desired",
            Skip::NotAMember => "not_a_member",
            Skip::UpToDate => "up_to_date",
            Skip::FailedHere => "failed_here",
            Skip::Deferred => "deferred",
            Skip::Unsupported => "unsupported",
            Skip::NoDigest => "no_digest",
            Skip::NoPackageForPlatform => "no_package_for_platform",
            Skip::NoTarget => "no_target",
            Skip::NoBuildForHost => "no_build_for_host",
            Skip::InstallerDropped => "installer_dropped",
        }
    };
    for g in [
        Gate::Halted,
        Gate::Held,
        Gate::Bad,
        Gate::BelowMinVersion,
        Gate::Updating,
        Gate::BusyElsewhere,
        Gate::NoSlot,
        Gate::Quiet,
        Gate::CanaryWait,
        Gate::Baking,
        Gate::Paused,
        Gate::AdvanceWait,
        Gate::StageFailed,
        Gate::StageWait,
        Gate::BelowMinUp,
        Gate::RolloutElsewhere,
    ] {
        assert_eq!(json!(g), json!(gate(g)));
        assert_eq!(json!(Why::Gate(g)), json!(gate(g)));
    }
    for s in [
        Skip::NoDesired,
        Skip::NotAMember,
        Skip::UpToDate,
        Skip::FailedHere,
        Skip::Deferred,
        Skip::Unsupported,
        Skip::NoDigest,
        Skip::NoPackageForPlatform,
        Skip::NoTarget,
        Skip::NoBuildForHost,
        Skip::InstallerDropped,
    ] {
        assert_eq!(json!(s), json!(skip(s)));
        assert_eq!(json!(Why::Skip(s)), json!(skip(s)));
    }
}
