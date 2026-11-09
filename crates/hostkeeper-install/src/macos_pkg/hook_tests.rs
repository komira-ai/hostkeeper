//! Tests of the hook at its fixed path.

use super::*;

const SERVICE: &str = "svc";

fn meta(kind: Kind, mode: u32) -> Meta {
    Meta {
        kind,
        mode,
        uid: 0,
        gid: 0,
    }
}

/// The directories of `svc`'s payload, with `extra` entries.
fn payload(extra: &[(&str, Meta)]) -> BTreeMap<String, Meta> {
    let mut found: BTreeMap<String, Meta> = [
        "/Library",
        "/Library/Application Support",
        "/Library/Application Support/svc",
    ]
    .into_iter()
    .map(|dir| (dir.to_owned(), meta(Kind::Dir, 0o755)))
    .collect();
    for (path, entry) in extra {
        found.insert((*path).to_owned(), *entry);
    }
    found
}

const HOOK: &str = "/Library/Application Support/svc/hostkeeper-hook";

#[test]
fn the_hook_is_the_fixed_file_of_the_services_directory() {
    assert_eq!(hook_path(SERVICE), HOOK);
}

/// Catches: a package without a hook refused, or one with an executable hook not reported.
#[test]
fn a_package_with_no_hook_or_a_regular_executable_one_passes() {
    assert_eq!(check(&payload(&[]), SERVICE, false), Ok(false));
    for mode in [0o755, 0o700, 0o500] {
        let found = payload(&[(HOOK, meta(Kind::File, mode))]);
        assert_eq!(check(&found, SERVICE, false), Ok(true), "{mode:o}");
        assert_eq!(check(&found, SERVICE, true), Ok(true), "{mode:o}");
    }
}

/// Catches: a hook path that is a link, a directory, a device or a file its owner cannot run
/// being taken for a hook.
#[test]
fn a_hook_that_is_a_link_a_directory_not_a_file_or_not_executable_is_refused() {
    let cases = [
        (meta(Kind::Link, 0o777), HookError::Link(HOOK.into())),
        (meta(Kind::Dir, 0o755), HookError::Directory(HOOK.into())),
        (meta(Kind::Other, 0o644), HookError::NotFile(HOOK.into())),
        (
            meta(Kind::File, 0o644),
            HookError::NotExecutable(HOOK.into(), 0o644),
        ),
        (
            meta(Kind::File, 0o055),
            HookError::NotExecutable(HOOK.into(), 0o055),
        ),
    ];
    for (entry, want) in cases {
        assert_eq!(check(&payload(&[(HOOK, entry)]), SERVICE, false), Err(want));
    }
}

/// Catches: a hook under a linked directory, whose real place the payload does not show, being
/// taken as absent or as the hook.
#[test]
fn a_hook_reached_through_a_link_is_refused_present_or_not() {
    for linked in [
        "/Library/Application Support/svc",
        "/Library/Application Support",
    ] {
        let mut found = payload(&[]);
        found.insert(linked.to_owned(), meta(Kind::Link, 0o777));
        let want = HookError::ThroughLink(HOOK.into(), linked.into());
        assert_eq!(check(&found, SERVICE, false), Err(want.clone()));
        found.insert(HOOK.to_owned(), meta(Kind::File, 0o755));
        assert_eq!(check(&found, SERVICE, false), Err(want));
    }
}

/// Catches: `hook.boot` accepted for a package that has no hook to run at boot.
#[test]
fn hook_boot_with_no_hook_is_refused() {
    assert_eq!(
        check(&payload(&[]), SERVICE, true),
        Err(HookError::BootWithoutHook(HOOK.into()))
    );
    assert_eq!(
        HookError::BootWithoutHook(HOOK.into()).to_string(),
        "the entry's hook.boot is set, but the package has no hook at \
         /Library/Application Support/svc/hostkeeper-hook"
    );
}
