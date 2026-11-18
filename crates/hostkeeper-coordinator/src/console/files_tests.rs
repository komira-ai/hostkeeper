//! The console's files: read at start, read again on the reload tick, a refused one kept out.

use hostkeeper_console::operators::{Denied, Need};

use super::{ConsoleFiles, ConsoleFilesError};
use crate::console::support::{OPERATORS, write_files};
use crate::testutil::scratch;

/// Proves: with no files nobody has access, the reload does nothing; with them, the operators
/// file grants as it says.
///
/// Catches: a coordinator with no operators file that lets an operator in.
#[test]
fn no_operators_file_means_nobody() {
    let none = ConsoleFiles::default();
    none.reload();
    assert_eq!(
        none.grant("alice", Need::Console).err(),
        Some(Denied::NoConsoleAccess)
    );
    assert_eq!(format!("{none:?}"), "ConsoleFiles { paths: None, .. }");
    let dir = scratch("console-files-grant");
    let files = ConsoleFiles::read(write_files(&dir, OPERATORS)).expect("read");
    assert!(files.grant("alice", Need::Power).is_ok());
    assert_eq!(
        files.grant("bob", Need::Power).err(),
        Some(Denied::PowerNotPermitted)
    );
    assert_eq!(
        files.grant("carol", Need::Console).err(),
        Some(Denied::NoConsoleAccess)
    );
}

/// Proves: a change to each file is in use after a reload; a file broken since is logged and the
/// one in use stays, for each of the three.
///
/// Catches: an operator revoked from the file keeping access until a restart, and a broken edit
/// that locks every operator out.
#[test]
fn a_reload_takes_a_changed_file_and_keeps_the_last_good_one() {
    let dir = scratch("console-files-reload");
    let paths = write_files(&dir, OPERATORS);
    let files = ConsoleFiles::read(paths.clone()).expect("read");
    files.reload();
    assert!(files.grant("alice", Need::Power).is_ok());
    write_files(&dir, "[[operator]]\nname = \"carol\"\n");
    std::fs::write(
        &paths.targets,
        "[[target]]\nmac = \"0c:c4:7a:00:00:01\"\naddress = \"192.0.2.99\"\n",
    )
    .expect("written");
    files.reload();
    assert_eq!(
        files.grant("alice", Need::Console).err(),
        Some(Denied::NoConsoleAccess)
    );
    let grant = files.grant("carol", Need::Power).expect("carol");
    let bmc = hostkeeper_enrollment::names::MacAddress::parse("0c:c4:7a:00:00:01").expect("a MAC");
    assert_eq!(
        grant.targets.address_of(&bmc).map(ToString::to_string),
        Some("192.0.2.99".to_owned())
    );
    for path in [&paths.operators, &paths.targets, &paths.credential] {
        std::fs::write(path, "not toml [").expect("broken");
    }
    files.reload();
    let grant = files.grant("carol", Need::Power).expect("still carol");
    assert_eq!(grant.credential.user(), "ADMIN");
    assert_eq!(
        grant.targets.address_of(&bmc).map(ToString::to_string),
        Some("192.0.2.99".to_owned())
    );
}

/// Proves: at start a broken file of each kind refuses the console, its error naming its flag.
///
/// Catches: a coordinator that starts with a console file it cannot read and grants nobody.
#[test]
fn a_broken_file_refuses_the_start_naming_its_flag() {
    let dir = scratch("console-files-start");
    let paths = write_files(&dir, OPERATORS);
    let flags = [
        (&paths.operators, "--console-operators-file"),
        (&paths.targets, "--console-targets-file"),
        (&paths.credential, "--console-bmc-credential-file"),
    ];
    for (path, flag) in flags {
        let good = std::fs::read(path).expect("read");
        std::fs::write(path, "not toml [").expect("broken");
        let refused: ConsoleFilesError = ConsoleFiles::read(paths.clone()).expect_err("refused");
        assert!(refused.to_string().starts_with(flag), "{refused}");
        std::fs::write(path, good).expect("restored");
    }
}
