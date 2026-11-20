//! The hook's program, boot step and bounds, and the installer's bounds: every bound at 0 and at
//! its limit.

use hostkeeper_config::names::PathError;
use hostkeeper_proto::hostkeeper::release::v1::{Hook, InstallerBounds, Manifest, StepBounds};
use hostkeeper_release::manifest::{self, ManifestError};
use hostkeeper_release::{Installer, MAX_INSTALLER_SECS};

use crate::support::{LINUX, bounds, hook, manifest, placed_manifest, web};

fn with_hook(hook: Hook) -> Manifest {
    Manifest {
        hook: Some(hook),
        ..placed_manifest()
    }
}

fn check_hook(hook: Hook) -> Result<(), ManifestError> {
    manifest::check(&web(), LINUX, &with_hook(hook))
}

/// A change to one bound of a fixture.
type SetStep = fn(&mut StepBounds);
/// Sets one installer bound.
type SetBound = fn(&mut InstallerBounds, u64);

fn steps(change: impl FnOnce(&mut StepBounds)) -> Hook {
    let mut hook = hook();
    let bounds = hook.timeout_secs.as_mut().expect("bounds");
    change(bounds);
    hook
}

/// Catches: a hook program that leaves the tree, names an absolute path or a dot segment,
/// passing for an entry that is not native; a plain relative program refused; or the rule
/// applied by the rules every manifest keeps, which do not know the installer.
#[test]
fn a_hook_program_is_a_relative_path_of_plain_names() {
    let program = |program: &str| Hook {
        command: vec![program.to_owned(), "update-hook".to_owned()],
        ..hook()
    };
    let check_program = |hook: Hook| manifest::check_hook_of(false, &with_hook(hook));
    for (bad, why) in [
        ("..", PathError::DotComponent),
        ("bin/../../etc/shadow", PathError::DotComponent),
        (".", PathError::DotComponent),
        ("./bin/web", PathError::DotComponent),
        ("/bin/sh", PathError::EmptyComponent),
        ("bin//web", PathError::EmptyComponent),
        ("", PathError::Empty),
        ("bin/web hook", PathError::BadChar(' ')),
    ] {
        assert_eq!(
            check_program(program(bad)),
            Err(ManifestError::HookProgram {
                program: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check_program(program(".."))
            .expect_err("refused")
            .to_string(),
        "hook.command names the program \"..\", which has a `.` or `..` component; it is a path relative to the tree"
    );
    assert_eq!(check_program(program("usr/bin/web")), Ok(()));
    assert_eq!(check_hook(program("/bin/sh")), Ok(()));
    // The arguments are the hook's own; only the program is a path.
    let args = Hook {
        command: vec![
            "bin/web".to_owned(),
            "../anything".to_owned(),
            "/x".to_owned(),
        ],
        ..hook()
    };
    assert_eq!(check_program(args), Ok(()));
}

/// Catches: an empty command (the built-in steps) refused, or a boot step passing whose name is not an additive step's. Whether a boot
/// step needs a command is the installer's rule (`native.rs`): the rules every manifest keeps
/// take one without.
#[test]
fn an_empty_command_passes_and_a_boot_step_is_named_as_an_additive_step() {
    let builtin = Hook {
        command: Vec::new(),
        boot: None,
        ..hook()
    };
    assert_eq!(check_hook(builtin.clone()), Ok(()));
    assert_eq!(
        check_hook(Hook {
            boot: Some("restore".to_owned()),
            ..builtin
        }),
        Ok(())
    );
    let long = "a".repeat(65);
    for bad in [
        "prepare",
        "uninstall",
        "Restore",
        "re-store",
        "",
        long.as_str(),
    ] {
        assert_eq!(
            check_hook(Hook {
                boot: Some(bad.to_owned()),
                ..hook()
            }),
            Err(ManifestError::Boot(bad.to_owned())),
            "{bad}"
        );
    }
    for good in ["restore", "warm_cache_2", "a".repeat(64).as_str()] {
        assert_eq!(
            check_hook(Hook {
                boot: Some(good.to_owned()),
                ..hook()
            }),
            Ok(()),
            "{good}"
        );
    }
}

/// Catches: a step bound of 0 passing for any step but `prepare`, `prepare` 0 refused, or an
/// absent `uninstall` refused.
#[test]
fn every_step_bound_but_prepare_is_above_0() {
    assert_eq!(check_hook(steps(|b| b.prepare = 0)), Ok(()));
    assert_eq!(check_hook(steps(|b| b.uninstall = None)), Ok(()));
    let cases: [(&str, SetStep); 5] = [
        ("cutover", |b| b.cutover = 0),
        ("verify", |b| b.verify = 0),
        ("rollback", |b| b.rollback = 0),
        ("cleanup", |b| b.cleanup = 0),
        ("uninstall", |b| b.uninstall = Some(0)),
    ];
    for (step, zero) in cases {
        assert_eq!(
            check_hook(steps(zero)),
            Err(ManifestError::ZeroBound(step)),
            "{step}"
        );
    }
    assert_eq!(
        ManifestError::ZeroBound("cutover").to_string(),
        "hook.timeout_secs.cutover is 0; only prepare may be unbounded"
    );
    // Each at 1, the least it may be, with the deadline still verify plus rollback.
    let least = Hook {
        timeout_secs: Some(StepBounds {
            prepare: 1,
            cutover: 1,
            verify: 1,
            rollback: 1,
            cleanup: 1,
            uninstall: Some(1),
        }),
        verify_deadline_secs: 2,
        ..hook()
    };
    assert_eq!(check_hook(least), Ok(()));
    assert_eq!(
        check_hook(Hook {
            timeout_secs: None,
            ..hook()
        }),
        Err(ManifestError::NoStepBounds)
    );
    assert_eq!(
        ManifestError::NoStepBounds.to_string(),
        "hook.timeout_secs is missing"
    );
}

/// Catches: a verify deadline shorter than verify and rollback passing, the exact sum refused,
/// or a sum that overflows passing.
#[test]
fn the_verify_deadline_is_at_least_verify_and_rollback_together() {
    // The fixture's deadline is exactly verify (20) plus rollback (30).
    assert_eq!(check_hook(hook()), Ok(()));
    let short = Hook {
        verify_deadline_secs: 49,
        ..hook()
    };
    assert_eq!(
        check_hook(short),
        Err(ManifestError::DeadlineTooShort {
            deadline: 49,
            verify: 20,
            rollback: 30
        })
    );
    assert_eq!(
        check_hook(Hook {
            verify_deadline_secs: 49,
            ..hook()
        })
        .expect_err("refused")
        .to_string(),
        "hook.verify_deadline_secs 49 is less than the verify (20) and rollback (30) bounds together"
    );
    let overflow = Hook {
        verify_deadline_secs: u64::MAX,
        ..steps(|b| {
            b.verify = u64::MAX;
            b.rollback = 1;
        })
    };
    assert!(matches!(
        check_hook(overflow),
        Err(ManifestError::DeadlineTooShort { .. })
    ));
    let largest = Hook {
        verify_deadline_secs: u64::MAX,
        ..steps(|b| {
            b.verify = u64::MAX - 1;
            b.rollback = 1;
        })
    };
    assert_eq!(check_hook(largest), Ok(()));
}

/// Catches: a promotion ceiling of 0 passing, or an absent one refused.
#[test]
fn a_promotion_ceiling_is_absent_or_above_0() {
    assert_eq!(
        check_hook(Hook {
            promotion_ceiling_secs: Some(0),
            ..hook()
        }),
        Err(ManifestError::ZeroPromotionCeiling)
    );
    assert_eq!(
        ManifestError::ZeroPromotionCeiling.to_string(),
        "hook.promotion_ceiling_secs is 0; leave it out for no ceiling"
    );
    for ceiling in [None, Some(1)] {
        assert_eq!(
            check_hook(Hook {
                promotion_ceiling_secs: ceiling,
                cleanup_retries: None,
                ..hook()
            }),
            Ok(())
        );
    }
}

/// Catches: an installer bound of 0 or over 3600 passing, or 1 or 3600 refused, for any of the
/// three.
#[test]
fn each_installer_bound_is_1_to_3600() {
    let with = |b: InstallerBounds| Manifest {
        installer_bounds: Some(b),
        ..manifest(Installer::Apt)
    };
    let cases: [(&str, SetBound); 3] = [
        ("stage_secs", |b, s| b.stage_secs = s),
        ("activate_secs", |b, s| b.activate_secs = s),
        ("remove_secs", |b, s| b.remove_secs = s),
    ];
    for (field, set) in cases {
        for (secs, ok) in [
            (0, false),
            (1, true),
            (MAX_INSTALLER_SECS, true),
            (MAX_INSTALLER_SECS + 1, false),
        ] {
            let mut b = bounds();
            set(&mut b, secs);
            let got = manifest::check(&web(), LINUX, &with(b));
            let want = if ok {
                Ok(())
            } else {
                Err(ManifestError::InstallerBound { field, secs })
            };
            assert_eq!(got, want, "{field} {secs}");
        }
    }
    assert_eq!(
        ManifestError::InstallerBound {
            field: "stage_secs",
            secs: 3601
        }
        .to_string(),
        "installer_bounds.stage_secs is 3601, not 1 to 3600"
    );
}

fn pair(drain: Option<&str>, resume: Option<&str>) -> Hook {
    Hook {
        drain: drain.map(str::to_owned),
        resume: resume.map(str::to_owned),
        ..hook()
    }
}

/// Catches: a hook naming `drain` and `resume` (their own words, or free names of the additive
/// grammar) refused by the rules every manifest keeps or by the rules of the installer.
#[test]
fn a_drain_and_resume_pair_passes_under_their_own_words_or_free_names() {
    for (drain, resume) in [("drain", "resume"), ("quiesce", "unquiesce")] {
        let given = pair(Some(drain), Some(resume));
        assert_eq!(check_hook(given.clone()), Ok(()), "{drain} {resume}");
        assert_eq!(
            manifest::check_hook_of(false, &with_hook(given.clone())),
            Ok(()),
            "{drain} {resume}"
        );
        let mut apt = manifest(Installer::Apt);
        let native = apt.hook.as_mut().expect("a hook");
        native.drain = given.drain;
        native.resume = given.resume;
        assert_eq!(
            manifest::check_for(&web(), LINUX, Installer::Apt, &apt),
            Ok(()),
            "{drain} {resume}"
        );
    }
}

/// Catches: `drain` or `resume` accepted as the name of the boot step, which would run the
/// maintenance step at every daemon start.
#[test]
fn a_boot_step_cannot_be_named_drain_or_resume() {
    for bad in ["drain", "resume"] {
        assert_eq!(
            check_hook(Hook {
                boot: Some(bad.to_owned()),
                ..hook()
            }),
            Err(ManifestError::Boot(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        ManifestError::Boot("drain".to_owned()).to_string(),
        "hook.boot \"drain\" is not the name of an additive step: 1 to 64 of a-z, 0-9 and `_`, and none of prepare, cutover, verify, rollback, cleanup, uninstall, drain, resume"
    );
}

/// Catches: one of `drain` and `resume` passing without the other, which would take a service out
/// of service with no hook to put it back.
#[test]
fn drain_and_resume_come_together() {
    assert_eq!(
        check_hook(pair(Some("drain"), None)),
        Err(ManifestError::MaintenancePair {
            given: "drain",
            missing: "resume",
            name: "drain".to_owned()
        })
    );
    assert_eq!(
        check_hook(pair(None, Some("quiesce"))),
        Err(ManifestError::MaintenancePair {
            given: "resume",
            missing: "drain",
            name: "quiesce".to_owned()
        })
    );
    assert_eq!(
        check_hook(pair(Some("drain"), None))
            .expect_err("refused")
            .to_string(),
        "hook.drain \"drain\" needs a hook.resume; drain and resume go together"
    );
    assert_eq!(check_hook(pair(None, None)), Ok(()));
}

/// Catches: a name outside the drain or resume grammar passing: another step's word, the other
/// maintenance step's word, a capital, or an empty name.
#[test]
fn a_drain_or_resume_name_is_its_own_word_or_a_free_name() {
    let cases = [
        ("drain", "resume", pair(Some("resume"), Some("resume"))),
        ("drain", "cleanup", pair(Some("cleanup"), Some("resume"))),
        ("drain", "Drain", pair(Some("Drain"), Some("resume"))),
        ("resume", "drain", pair(Some("drain"), Some("drain"))),
        ("resume", "", pair(Some("drain"), Some(""))),
    ];
    for (field, name, hook) in cases {
        assert_eq!(
            check_hook(hook),
            Err(ManifestError::MaintenanceStep {
                field,
                name: name.to_owned()
            }),
            "{field} {name:?}"
        );
    }
    assert_eq!(
        check_hook(pair(Some("cleanup"), Some("resume")))
            .expect_err("refused")
            .to_string(),
        "hook.drain \"cleanup\" is not the name of the drain step: drain itself, or 1 to 64 of a-z, 0-9 and `_` and none of prepare, cutover, verify, rollback, cleanup, uninstall, drain, resume"
    );
}

/// Catches: two of `boot`, `drain` and `resume` naming one step passing, which would run it for
/// both.
#[test]
fn no_step_is_named_by_two_of_boot_drain_and_resume() {
    let cases = [
        (
            "boot",
            "drain",
            Hook {
                boot: Some("quiesce".to_owned()),
                ..pair(Some("quiesce"), Some("resume"))
            },
        ),
        (
            "boot",
            "resume",
            Hook {
                boot: Some("unquiesce".to_owned()),
                ..pair(Some("drain"), Some("unquiesce"))
            },
        ),
        (
            "drain",
            "resume",
            Hook {
                boot: Some("restore".to_owned()),
                ..pair(Some("x"), Some("x"))
            },
        ),
    ];
    for (first, second, hook) in cases {
        let name = match first {
            "boot" => hook.boot.clone(),
            _ => hook.drain.clone(),
        }
        .expect("named");
        assert_eq!(
            check_hook(hook),
            Err(ManifestError::StepNamedTwice {
                first,
                second,
                name
            }),
            "{first} {second}"
        );
    }
    assert_eq!(
        ManifestError::StepNamedTwice {
            first: "boot",
            second: "drain",
            name: "quiesce".to_owned()
        }
        .to_string(),
        "hook.boot and hook.drain both name the step \"quiesce\""
    );
}

/// Catches: `drain` or `resume` with no program passing for the installer's rule (the built-in
/// steps have neither), the boot step's refusal not coming first, or the field and name lost from
/// the message.
#[test]
fn drain_and_resume_need_a_command() {
    let without = |hook: Hook| {
        manifest::check_hook_of(
            false,
            &with_hook(Hook {
                command: Vec::new(),
                ..hook
            }),
        )
    };
    assert_eq!(
        without(Hook {
            boot: None,
            ..pair(Some("drain"), Some("resume"))
        }),
        Err(ManifestError::MaintenanceWithoutCommand {
            field: "drain",
            name: "drain".to_owned()
        })
    );
    assert_eq!(
        without(Hook {
            boot: None,
            ..pair(None, Some("resume"))
        }),
        Err(ManifestError::MaintenanceWithoutCommand {
            field: "resume",
            name: "resume".to_owned()
        })
    );
    assert_eq!(
        without(pair(Some("drain"), Some("resume"))),
        Err(ManifestError::BootWithoutCommand("restore".to_owned()))
    );
    assert_eq!(
        without(Hook {
            boot: None,
            ..pair(Some("drain"), Some("resume"))
        })
        .expect_err("refused")
        .to_string(),
        "hook.drain \"drain\" needs a hook.command; the built-in steps have no drain step"
    );
    assert_eq!(
        without(Hook {
            boot: None,
            ..pair(None, None)
        }),
        Ok(())
    );
}

/// Catches: the step bounds judged before the drain and resume names, so a manifest with both a
/// bad name and no bounds reports the bounds.
#[test]
fn a_bad_drain_name_is_reported_before_the_step_bounds() {
    assert_eq!(
        check_hook(Hook {
            timeout_secs: None,
            ..pair(Some("cleanup"), Some("resume"))
        }),
        Err(ManifestError::MaintenanceStep {
            field: "drain",
            name: "cleanup".to_owned()
        })
    );
    assert_eq!(
        check_hook(Hook {
            timeout_secs: None,
            ..pair(Some("drain"), Some("resume"))
        }),
        Err(ManifestError::NoStepBounds)
    );
}
