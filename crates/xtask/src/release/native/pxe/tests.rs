use super::{BOOT_DIR, DEB_PROGRAM, NAME, SETTINGS_FILE, UNIT, deb_files};

/// The committed unit of the package, read from the file.
const GOLDEN: &str = include_str!("../../../../../../packaging/pxe/deb/pxe.service");

#[test]
fn the_deb_holds_the_program_and_the_committed_unit_and_nothing_else() {
    // Proves: the package is the program at /usr/lib/pxe/pxe (0755, the given bytes) and the
    // unit in /usr/lib/systemd/system (0644, the golden's bytes), with no other file.
    // Catches: a file added (a wrapper, a /usr/bin entry), a mode changed, or a unit that
    // differs from its golden.
    let files = deb_files(b"BIN".to_vec());
    let listed: Vec<(&str, u32)> = files.iter().map(|f| (f.path.as_str(), f.mode)).collect();
    assert_eq!(
        listed,
        [
            ("/usr/lib/pxe/pxe", 0o755),
            ("/usr/lib/systemd/system/pxe.service", 0o644),
        ]
    );
    assert_eq!(files[0].data, b"BIN");
    assert_eq!(files[1].data, GOLDEN.as_bytes());
}

#[test]
fn the_unit_starts_the_installed_program_with_the_settings_and_boot_directory() {
    // Proves: the golden's ExecStart runs the program the package installs, on the settings file
    // and the boot directory the manifest's params name.
    // Catches: a unit still starting a program from the archive's current, or a params constant
    // that no longer matches the unit.
    let exec = GOLDEN
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .expect("an ExecStart");
    assert_eq!(
        exec,
        format!(
            "{DEB_PROGRAM} serve --notify-systemd --settings {SETTINGS_FILE} --dir {BOOT_DIR}/current"
        )
    );
    assert_eq!(UNIT, format!("{NAME}.service"));
}
