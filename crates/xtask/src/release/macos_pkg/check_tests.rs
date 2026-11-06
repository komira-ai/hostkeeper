//! The read-back of an expanded pkg: its parts, its `PackageInfo` and its payload, checked
//! against the binary, `gh` and trusted root it must hold.

use super::tests::*;
use super::*;

#[test]
fn a_good_expanded_pkg_passes_the_check() {
    // Catches: a check that refuses the pkg pkgbuild makes of the tree build lays out.
    let expanded = Expanded::good("macos-pkg-check-good");
    assert_eq!(
        expanded.check(),
        Ok(vec![format!(
            "check-macos-pkg: {} installs exactly /usr/local/bin/hostkeeper (755), \
             /Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist (644), \
             /private/etc/newsyslog.d/hostkeeper.conf (644), /usr/local/lib/hostkeeper/gh \
             (755), /usr/local/lib/hostkeeper/trusted_root.json (644) and the directory \
             /private/var/log/hostkeeper (755), owned by root and wheel, with the label \
             dev.hostkeeper.hostkeeper, version 0.1.5 and the one script postinstall",
            expanded.pkg().display()
        )])
    );
}

#[test]
fn a_pkg_with_another_file_or_without_a_part_is_refused() {
    // Catches: a pkg with a stray file, or a missing bill of materials, payload, info or scripts
    // (a pkg without its postinstall leaves the daemon's job nothing to start on a first install).
    let expanded = Expanded::good("macos-pkg-check-parts");
    expanded.write("Distribution", b"<installer-gui-script/>");
    std::fs::remove_file(expanded.pkg().join("Bom")).expect("remove");
    std::fs::remove_file(expanded.pkg().join("Scripts")).expect("remove");
    let only = "an expanded pkg of hostkeeper holds only the files Bom, PackageInfo, Payload and \
                Scripts";
    assert_eq!(
        expanded.problems(),
        [
            format!("Distribution: {only}"),
            "missing Bom".to_owned(),
            "missing Scripts".to_owned(),
        ]
    );
    // A part that is a directory is not that part, and is not read: `pkgutil --expand` leaves
    // `Scripts` an archive, as it leaves `Payload` (`--expand-full` would make directories).
    let expanded = Expanded::good("macos-pkg-check-payload-dir");
    for part in ["Payload", "Scripts"] {
        std::fs::remove_file(expanded.pkg().join(part)).expect("remove");
        std::fs::create_dir(expanded.pkg().join(part)).expect("a directory");
    }
    std::fs::write(
        expanded.pkg().join("Scripts/postinstall"),
        postinstall_text(),
    )
    .expect("write");
    std::fs::remove_file(expanded.pkg().join("PackageInfo")).expect("remove");
    assert_eq!(
        expanded.problems(),
        [
            format!("Payload: {only}"),
            format!("Scripts: {only}"),
            "missing PackageInfo".to_owned(),
            "missing Payload".to_owned(),
            "missing Scripts".to_owned(),
        ]
    );
}

#[test]
fn a_scripts_archive_that_is_not_the_one_postinstall_is_refused() {
    // Catches: a pkg that runs another script at install, runs a postinstall with other words
    // than the seed (another state directory, another binary, a command that is not the seed),
    // holds the postinstall twice or not at all, or holds one the installer cannot run.
    let text = postinstall_text();
    let script = |mode: u32, data: &[u8]| odc_entry("./postinstall", mode, 501, 20, data);
    let top = odc_entry(".", 0o040_755, 501, 20, b"");
    let other = text.replace(
        "--state-dir '/var/lib/updater/state'",
        "--state-dir '/tmp/state'",
    );
    let not_ours = "Scripts: ./postinstall is not the postinstall release macos-pkg writes";
    let cases: [(Vec<Vec<u8>>, Vec<String>); 7] = [
        (
            vec![top.clone(), script(0o100_755, other.as_bytes())],
            vec![not_ours.to_owned()],
        ),
        (
            vec![top.clone(), script(0o100_644, text.as_bytes())],
            vec!["Scripts: ./postinstall has mode 644, which the installer cannot run".to_owned()],
        ),
        (
            vec![top.clone(), script(0o040_755, b"")],
            vec![
                "Scripts: ./postinstall is of type 40000, not a regular file".to_owned(),
                not_ours.to_owned(),
            ],
        ),
        (
            vec![
                top.clone(),
                script(0o100_755, text.as_bytes()),
                odc_entry("./preinstall", 0o100_755, 501, 20, b"#!/bin/sh\n"),
            ],
            vec![
                "Scripts holds \"./preinstall\": the pkg's one script is ./postinstall".to_owned(),
            ],
        ),
        (
            vec![top.clone()],
            vec!["Scripts holds ./postinstall 0 times, not once".to_owned()],
        ),
        (
            vec![
                script(0o100_755, text.as_bytes()),
                script(0o100_755, text.as_bytes()),
            ],
            vec!["Scripts holds ./postinstall 2 times, not once".to_owned()],
        ),
        // The top directory is not needed: an archive of the postinstall alone is the script.
        (vec![script(0o100_700, text.as_bytes())], vec![]),
    ];
    for (n, (entries, want)) in cases.into_iter().enumerate() {
        let expanded = Expanded::good(&format!("macos-pkg-check-scripts-{n}"));
        expanded.write("Scripts", &gzip(&odc(&entries)));
        let got = match expanded.check() {
            Ok(_) => Vec::new(),
            Err(Error::Refused(lines)) => lines,
            Err(other) => panic!("case {n}: {other:?}"),
        };
        assert_eq!(got, want, "case {n}");
    }
    let expanded = Expanded::good("macos-pkg-check-scripts-not-gzip");
    expanded.write("Scripts", b"#!/bin/sh\n");
    let problems = expanded.problems();
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].starts_with("Scripts is not gzip data: "),
        "{problems:?}"
    );
    expanded.write("Scripts", &gzip(b"not cpio"));
    assert_eq!(
        expanded.problems(),
        ["Scripts: the archive ends at byte 8 without its trailer"]
    );
}

#[test]
fn the_postinstall_runs_the_installed_binarys_seed_of_the_daemons_state_directory() {
    // Catches: a postinstall that is not one line running the seed (the counterpart of the Linux
    // unit's ExecStartPre=), or that runs it from another binary, of another binary, or for
    // another state directory than the daemon's, which the plist starts `current` of.
    let text = postinstall_text();
    assert_eq!(
        text.lines().last(),
        Some(
            "exec '/usr/local/bin/hostkeeper' self-update-seed --state-dir \
             '/var/lib/updater/state' --binary '/usr/local/bin/hostkeeper'"
        )
    );
    assert!(text.starts_with("#!/bin/sh\n"));
    let code: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|line| !line.starts_with('#'))
        .collect();
    assert_eq!(code.len(), 1, "one command: {code:?}");
}

#[test]
fn package_info_of_another_identifier_version_or_location_or_with_a_script_is_refused() {
    // Catches: the pkg of another package or version, one installed elsewhere than `/`, or one
    // that runs a script or asks for a restart after install.
    let cases: [(String, Vec<&str>); 8] = [
        (
            package_info_with(
                "identifier=\"dev.hostkeeper.other\" version=\"0.1.4\" install-location=\"/opt\"",
            ),
            vec![
                "PackageInfo: identifier is \"dev.hostkeeper.other\", not \"dev.hostkeeper.hostkeeper\"",
                "PackageInfo: version is \"0.1.4\", not \"0.1.5\"",
                "PackageInfo: install-location is \"/opt\", not \"/\"",
            ],
        ),
        (
            package_info_with("version=\"0.1.5\""),
            vec![
                "PackageInfo has no identifier",
                "PackageInfo has no install-location",
            ],
        ),
        (
            package_info().replace(
                "postinstall-action=\"none\"",
                "postinstall-action=\"restart\"",
            ),
            vec!["PackageInfo: postinstall-action is \"restart\", not \"none\""],
        ),
        (
            package_info().replace("<postinstall file=\"./postinstall\"/>", ""),
            vec![
                "PackageInfo names no postinstall script (<postinstall file=\"./postinstall\"/>): \
                 nothing would give the daemon's job a binary to start",
            ],
        ),
        (
            package_info().replace(
                "<relocate/>",
                "<scripts><preinstall file=\"./preinstall\"/></scripts>",
            ),
            vec!["PackageInfo names a preinstall script: the pkg's one script is its postinstall"],
        ),
        (
            "<installer-script/>".to_owned(),
            vec!["PackageInfo has no readable <pkg-info> element"],
        ),
        (
            "<pkg-info identifier=\"dev.hostkeeper.hostkeeper\" version=0.1.5>".to_owned(),
            vec!["PackageInfo has no readable <pkg-info> element"],
        ),
        (
            "<pkg-info a b=\"c\">".to_owned(),
            vec!["PackageInfo has no readable <pkg-info> element"],
        ),
    ];
    for (n, (text, want)) in cases.into_iter().enumerate() {
        let expanded = Expanded::good(&format!("macos-pkg-check-info-{n}"));
        expanded.write("PackageInfo", text.as_bytes());
        assert_eq!(expanded.problems(), want, "{text}");
    }
    // A self-closing element with the three attributes is read; one without its end is not.
    assert_eq!(
        pkg_info_attributes("<pkg-info identifier=\"i\" version=\"v\"/>").map(|a| a.len()),
        Some(2)
    );
    assert_eq!(pkg_info_attributes("<pkg-info identifier=\"i\""), None);
    // A value whose closing quote never comes is not read as running to the end.
    assert_eq!(pkg_info_attributes("<pkg-info identifier=\"i>"), None);
}

#[test]
fn a_payload_that_is_not_a_gzip_cpio_is_refused() {
    // Catches: a payload in another format (pbzx, xz, newc cpio) passing as empty or read
    // loosely.
    let expanded = Expanded::good("macos-pkg-check-not-gzip");
    expanded.write("Payload", b"pbzx not gzip");
    let problems = expanded.problems();
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].starts_with("Payload is not gzip data: "),
        "{problems:?}"
    );
    let expanded = Expanded::good("macos-pkg-check-newc");
    expanded.write("Payload", &gzip(b"070701 a newc header"));
    assert_eq!(
        expanded.problems(),
        ["Payload: the archive ends at byte 20 without its trailer"]
    );
}

#[test]
fn a_payload_with_another_entry_owner_kind_mode_or_bytes_is_refused_naming_each() {
    // Catches: a payload with a file beyond its own, one of its files missing (gh or the
    // trusted root among them), an entry not owned by root and wheel, a writable or setuid mode,
    // a gh that cannot run, a link or directory in place of a file (or the reverse), or other
    // bytes than the released binary and plist and the pinned gh and trusted root; the log
    // directory and the newsyslog file have their own cases in `logs`.
    let good = good_entries(BINARY);
    let plist = plist_text().expect("the plist");
    let with = |n: usize, entry: Vec<u8>| {
        let mut entries = good.clone();
        entries[n] = entry;
        entries
    };
    let cases: Vec<(Vec<Vec<u8>>, Vec<String>)> = vec![
        (
            [
                good.clone(),
                vec![odc_entry("./etc/hostkeeper.toml", 0o100_644, 0, 0, b"x")],
            ]
            .concat(),
            vec![
                "/etc/hostkeeper.toml (\"./etc/hostkeeper.toml\") is in the payload, which holds \
                 only the binary, the plist, the newsyslog file, the log directory, gh, the trusted root and their \
                 directories, each once"
                    .to_owned(),
            ],
        ),
        (
            [&good[..BIN_AT], &good[BIN_AT + 1..]].concat(),
            vec!["missing /usr/local/bin/hostkeeper in the payload".to_owned()],
        ),
        (
            [&good[..3], &good[4..]].concat(),
            vec![
                "missing /Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist in the payload"
                    .to_owned(),
            ],
        ),
        (
            with(
                BIN_AT,
                odc_entry(&format!("./{BIN}"), 0o100_755, 501, 20, BINARY),
            ),
            vec![
                "/usr/local/bin/hostkeeper: owned by uid 501 and gid 20, not root and wheel (0 \
                 and 0)"
                    .to_owned(),
            ],
        ),
        (
            with(
                BIN_AT,
                odc_entry(&format!("./{BIN}"), 0o104_755, 0, 0, BINARY),
            ),
            vec!["/usr/local/bin/hostkeeper: mode 4755, not 755".to_owned()],
        ),
        (
            with(
                3,
                odc_entry(&format!("./{PLIST}"), 0o100_664, 0, 0, plist.as_bytes()),
            ),
            vec![
                "/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist: mode 664, not 644"
                    .to_owned(),
            ],
        ),
        (
            with(1, odc_entry("./Library", 0o040_775, 0, 0, b"")),
            vec!["/Library: mode 775, not 755".to_owned()],
        ),
        (
            with(
                BIN_AT,
                odc_entry(&format!("./{BIN}"), 0o120_755, 0, 0, b"/elsewhere"),
            ),
            vec!["/usr/local/bin/hostkeeper: of type 120000, not a regular file".to_owned()],
        ),
        (
            with(
                BIN_AT - 1,
                odc_entry("./usr/local/bin", 0o100_755, 0, 0, b""),
            ),
            vec!["/usr/local/bin: of type 100000, not a directory".to_owned()],
        ),
        (
            with(
                BIN_AT,
                odc_entry(&format!("./{BIN}"), 0o100_755, 0, 0, b"another binary"),
            ),
            vec![format!(
                "/usr/local/bin/hostkeeper: not the bytes of {{binary}}"
            )],
        ),
        (
            with(
                3,
                odc_entry(
                    &format!("./{PLIST}"),
                    0o100_644,
                    0,
                    0,
                    plist.replace("<true/>", "<false/>").as_bytes(),
                ),
            ),
            vec![
                "/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist: not the daemon plist \
                 hostkeeper-launchd writes"
                    .to_owned(),
            ],
        ),
        (
            with(
                3,
                odc_entry(
                    &format!("./{PLIST}"),
                    0o100_644,
                    0,
                    0,
                    plist
                        .replace(">dev.hostkeeper.hostkeeper<", ">dev.hostkeeper.x<")
                        .as_bytes(),
                ),
            ),
            vec![
                "/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist: the label is \
                 Some(\"dev.hostkeeper.x\"), not \"dev.hostkeeper.hostkeeper\""
                    .to_owned(),
                "/Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist: not the daemon plist \
                 hostkeeper-launchd writes"
                    .to_owned(),
            ],
        ),
        (
            [good.clone(), vec![good[BIN_AT].clone()]].concat(),
            vec![
                "/usr/local/bin/hostkeeper (\"./usr/local/bin/hostkeeper\") is in the payload, \
                 which holds only the binary, the plist, the newsyslog file, the log directory, gh, the trusted root and \
                 their directories, each once"
                    .to_owned(),
            ],
        ),
        (
            good[..GH_AT].to_vec(),
            vec![
                "missing /usr/local/lib/hostkeeper/gh in the payload".to_owned(),
                "missing /usr/local/lib/hostkeeper/trusted_root.json in the payload".to_owned(),
            ],
        ),
        (
            [&good[..GH_AT], &good[ROOT_AT..]].concat(),
            vec!["missing /usr/local/lib/hostkeeper/gh in the payload".to_owned()],
        ),
        (
            good[..ROOT_AT].to_vec(),
            vec!["missing /usr/local/lib/hostkeeper/trusted_root.json in the payload".to_owned()],
        ),
        (
            with(GH_AT, odc_entry(&format!("./{GH}"), 0o100_644, 0, 0, GH_BYTES)),
            vec!["/usr/local/lib/hostkeeper/gh: mode 644, not 755".to_owned()],
        ),
        (
            with(
                ROOT_AT,
                odc_entry(&format!("./{ROOT}"), 0o100_666, 0, 0, ROOT_BYTES),
            ),
            vec!["/usr/local/lib/hostkeeper/trusted_root.json: mode 666, not 644".to_owned()],
        ),
        (
            with(
                GH_AT,
                odc_entry(&format!("./{GH}"), 0o100_755, 0, 0, b"another gh"),
            ),
            vec!["/usr/local/lib/hostkeeper/gh: not the bytes of {gh}".to_owned()],
        ),
        (
            with(
                ROOT_AT,
                odc_entry(&format!("./{ROOT}"), 0o100_644, 0, 0, b"{\"another\":1}\n"),
            ),
            vec![
                "/usr/local/lib/hostkeeper/trusted_root.json: not the bytes of {trusted_root}"
                    .to_owned(),
            ],
        ),
        (
            with(
                GH_AT - 1,
                odc_entry("./usr/local/lib/hostkeeper", 0o040_700, 0, 0, b""),
            ),
            vec!["/usr/local/lib/hostkeeper: mode 700, not 755".to_owned()],
        ),
        // A name without `./` is the same path: pkgbuild writes `./`, other tools may not.
        (
            with(BIN_AT, odc_entry(BIN, 0o100_755, 0, 0, BINARY)),
            vec![],
        ),
    ];
    for (n, (entries, want)) in cases.into_iter().enumerate() {
        let expanded = Expanded::new(&format!("macos-pkg-check-payload-{n}"), &entries);
        let want: Vec<String> = want
            .iter()
            .map(|w| {
                let sources = expanded.sources();
                w.replace("{binary}", &sources.binary.display().to_string())
                    .replace("{gh}", &sources.gh.display().to_string())
                    .replace(
                        "{trusted_root}",
                        &sources.trusted_root.display().to_string(),
                    )
            })
            .collect();
        match expanded.check() {
            Ok(_) => assert!(want.is_empty(), "case {n} passed, wanted {want:?}"),
            Err(Error::Refused(got)) => assert_eq!(got, want, "case {n}"),
            Err(other) => panic!("case {n}: {other:?}"),
        }
    }
}

#[test]
fn a_name_without_the_dot_slash_prefix_or_with_another_one_is_not_a_payload_path() {
    // Catches: a payload path read loosely, so that `./x` and `x` and `../x` name one entry.
    assert_eq!(payload_path("."), "");
    assert_eq!(payload_path("./usr"), "usr");
    assert_eq!(payload_path("usr"), "usr");
    assert_eq!(payload_path("../usr"), "../usr");
    assert_eq!(payload_path("/usr"), "/usr");
    let mut entries = good_entries(BINARY);
    entries[BIN_AT - 3] = dir_entry("../usr");
    let expanded = Expanded::new("macos-pkg-check-dotdot", &entries);
    assert_eq!(
        expanded.problems(),
        [
            "/../usr (\"../usr\") is in the payload, which holds only the binary, the plist, the \
             newsyslog file, the log directory, gh, the trusted root and their directories, \
             each once",
            "missing /usr in the payload",
        ]
    );
}

#[test]
fn an_unreadable_directory_binary_gh_or_trusted_root_is_an_io_error_naming_it() {
    // Catches: a check that passes because it could read nothing, or that checks the payload's
    // gh or trusted root against nothing.
    let expanded = Expanded::good("macos-pkg-check-io");
    let absent = expanded.dir.join("absent");
    assert!(matches!(
        check(&absent, &version(), &expanded.sources()),
        Err(Error::Io { what, .. }) if what == format!("read {}", absent.display())
    ));
    for missing in 0..3 {
        let mut sources = expanded.sources();
        let source = [
            &mut sources.binary,
            &mut sources.gh,
            &mut sources.trusted_root,
        ];
        *source.into_iter().nth(missing).expect("a source") = absent.clone();
        assert!(matches!(
            check(&expanded.pkg(), &version(), &sources),
            Err(Error::Io { what, .. }) if what == format!("read {}", absent.display())
        ));
    }
}
