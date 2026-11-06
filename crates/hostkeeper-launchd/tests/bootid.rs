//! The reading of the Mac's boot id, `kern.bootsessionuuid`: the text the sysctl gives is a
//! boot id only when it is a UUID, never an empty one.

use hostkeeper_launchd::{BOOT_SESSION_UUID, parse_boot_session_uuid};
use hostkeeper_steps::BootIdError;

/// `sysctl -n kern.bootsessionuuid` as a Mac prints it: upper-case digits and a line end.
const SYSCTL: &str = include_str!("fixtures/sysctl-bootsessionuuid.txt");

/// Catches the line end of the sysctl's value kept in the id, or the id changed in case: the
/// value of a Mac gives its UUID, as the kernel wrote it.
#[test]
fn the_sysctl_value_of_a_mac_gives_its_uuid() {
    assert_eq!(BOOT_SESSION_UUID, "kern.bootsessionuuid");
    assert!(SYSCTL.ends_with('\n'));
    let id = parse_boot_session_uuid(SYSCTL).expect("a boot id");
    assert_eq!(id.as_str(), "6F1E9C2A-4B3D-4E8F-A1C7-0D5B9E2F3A64");
    assert_eq!(
        parse_boot_session_uuid(&SYSCTL.to_lowercase())
            .expect("lower case")
            .as_str(),
        "6f1e9c2a-4b3d-4e8f-a1c7-0d5b9e2f3a64"
    );
}

/// Catches an empty boot id slipping through, which would make every boot of the Mac the same
/// boot: an empty value and one of white space only are refused as empty.
#[test]
fn an_empty_sysctl_value_is_refused() {
    for text in ["", "\n", " \t\n"] {
        let error = parse_boot_session_uuid(text).expect_err("an empty id");
        assert!(
            matches!(
                error,
                BootIdError::Empty {
                    from: "kern.bootsessionuuid"
                }
            ),
            "{text:?}"
        );
        assert_eq!(
            error.to_string(),
            "the boot id read from kern.bootsessionuuid is empty"
        );
    }
}

/// Catches a value that is not a boot id taken as one: text that is not a UUID, a UUID cut short
/// and one with a stray character are refused with what was read.
#[test]
fn a_sysctl_value_that_is_not_a_uuid_is_refused() {
    for text in [
        "unknown\n",
        "6F1E9C2A-4B3D-4E8F-A1C7\n",
        "6F1E9C2A-4B3D-4E8F-A1C7-0D5B9E2F3A6Z\n",
    ] {
        let error = parse_boot_session_uuid(text).expect_err("not a UUID");
        assert_eq!(
            error.to_string(),
            format!(
                "the boot id read from kern.bootsessionuuid is not a UUID: {:?}",
                text.trim()
            )
        );
    }
}
