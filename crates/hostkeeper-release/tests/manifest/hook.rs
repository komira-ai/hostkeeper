//! The hook's program, boot step and bounds, and the installer's bounds: every bound at 0 and at
//! its limit.

use hostkeeper_config::names::PathError;
use hostkeeper_proto::hostkeeper::release::v1::{Hook, InstallerBounds, Manifest, StepBounds};
use hostkeeper_release::manifest::{self, ManifestError};
use hostkeeper_release::{Installer, MAX_INSTALLER_SECS};

use crate::support::{LINUX, bounds, hook, manifest, web};

fn with_hook(hook: Hook) -> Manifest {
    Manifest {
        hook: Some(hook),
        ..manifest(Installer::Tarball)
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
/// passing; a plain relative program refused.
#[test]
fn a_hook_program_is_a_relative_path_of_plain_names() {
    let program = |program: &str| Hook {
        command: vec![program.to_owned(), "update-hook".to_owned()],
        ..hook()
    };
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
            check_hook(program(bad)),
            Err(ManifestError::HookProgram {
                program: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check_hook(program("..")).expect_err("refused").to_string(),
        "hook.command names the program \"..\", which has a `.` or `..` component; it is a path relative to the tree"
    );
    assert_eq!(check_hook(program("usr/bin/web")), Ok(()));
    // The arguments are the hook's own; only the program is a path.
    let args = Hook {
        command: vec![
            "bin/web".to_owned(),
            "../anything".to_owned(),
            "/x".to_owned(),
        ],
        ..hook()
    };
    assert_eq!(check_hook(args), Ok(()));
}

/// Catches: an empty command (the built-in steps, or a native package's program at its fixed
/// path) refused, or a boot step passing whose name is not an additive step's. Whether a boot
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
