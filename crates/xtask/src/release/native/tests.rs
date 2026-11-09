use super::{EntryKind, deb_path_problem, hook_wrapper, unit_named_for};

#[test]
fn the_hook_runs_the_program_beside_it_with_update_hook_and_every_argument() {
    // Catches: a hook that runs an installed program (the old version before activate) instead
    // of the one of its own tree, drops or splits an argument, or forgets the verb.
    assert_eq!(
        hook_wrapper("hello"),
        "#!/bin/sh\nexec \"${0%/*}/hello\" update-hook \"$@\"\n"
    );
    assert_eq!(
        hook_wrapper("bin/hello"),
        "#!/bin/sh\nexec \"${0%/*}/bin/hello\" update-hook \"$@\"\n"
    );
    assert_eq!(hook_wrapper("x").lines().count(), 2);
}

#[test]
fn a_unit_is_named_for_the_service_by_its_name_a_dash_or_an_at_and_a_kind() {
    // Catches: a rule that lets a package install a unit of the host's (shadowing it), a drop-in,
    // a kind the daemon does not run, or a name that only starts with the service's letters.
    for name in [
        "mdm.service",
        "mdm-proxy.service",
        "mdm-cert.timer",
        "mdm@a.service",
        "mdm-x.socket",
    ] {
        assert!(unit_named_for("mdm", name), "{name}");
    }
    for name in [
        "mdmx.service",
        "ssh.service",
        "mdm-.service",
        "mdm@.service",
        "mdm.path",
        "mdm.target",
        "mdm.slice",
        "mdm-proxy.service.d",
        "mdm",
        "x/mdm.service",
        "mdm-a/b.service",
    ] {
        assert!(!unit_named_for("mdm", name), "{name}");
    }
}

#[test]
fn a_deb_installs_only_at_the_services_paths_and_its_units_as_plain_named_files() {
    // Catches: each path of section 10.2's rule taken out or widened: the service's own trees,
    // a program in /usr/bin, a named unit, and the directories above them pass; an enablement
    // link, a cron file, a sudoers file, a drop-in, another service's tree, a unit of another
    // name or a link in the unit directory, a file at a host directory, a nested /usr/bin
    // directory, a .. component, and a device are each refused.
    let ok = [
        ("/", EntryKind::Dir),
        ("/usr/", EntryKind::Dir),
        ("/usr/lib/systemd/system/", EntryKind::Dir),
        ("/usr/lib/hello/", EntryKind::Dir),
        ("/usr/lib/hello/hello", EntryKind::File),
        ("/usr/lib/hello/sub/x", EntryKind::Link),
        ("/usr/share/hello/x", EntryKind::File),
        ("/usr/share/doc/hello/copyright", EntryKind::File),
        ("/etc/hello/hello.conf", EntryKind::File),
        ("/usr/bin/hello", EntryKind::File),
        ("/usr/bin/hello-ctl", EntryKind::Link),
        ("/usr/lib/systemd/system/hello@a.service", EntryKind::File),
    ];
    for (path, kind) in ok {
        assert_eq!(deb_path_problem("hello", path, kind), None, "{path}");
    }
    let refused = [
        (
            "/etc/systemd/system/multi-user.target.wants/hello@a.service",
            EntryKind::Link,
            "outside",
        ),
        ("/etc/systemd/", EntryKind::Dir, "outside"),
        ("/etc/cron.d/hello", EntryKind::File, "outside"),
        ("/etc/sudoers.d/hello", EntryKind::File, "outside"),
        (
            "/usr/lib/systemd/system/hello@a.service.d/",
            EntryKind::Dir,
            "no drop-in",
        ),
        (
            "/usr/lib/systemd/system/ssh.service",
            EntryKind::File,
            "no drop-in",
        ),
        (
            "/usr/lib/systemd/system/hello.service",
            EntryKind::Link,
            "no drop-in",
        ),
        (
            "/usr/lib/systemd/system/hello.path",
            EntryKind::File,
            "no drop-in",
        ),
        (
            "/usr/lib/systemd/system-generators/hello",
            EntryKind::File,
            "outside",
        ),
        ("/usr/lib/mdm/mdm", EntryKind::File, "outside"),
        ("/etc/ld.so.conf.d/hello.conf", EntryKind::File, "outside"),
        ("/usr/bin/sub/", EntryKind::Dir, "outside"),
        ("/usr/lib", EntryKind::File, "a directory of the host"),
        ("/usr/lib/hello/../mdm/x", EntryKind::File, ".."),
        ("/usr/lib/hello/fifo", EntryKind::Other, "neither"),
    ];
    for (path, kind, words) in refused {
        let why = deb_path_problem("hello", path, kind).unwrap_or_else(|| panic!("{path}"));
        assert!(why.contains(words), "{path}: {why}");
    }
}
