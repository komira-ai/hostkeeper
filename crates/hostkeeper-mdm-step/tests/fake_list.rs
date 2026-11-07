//! The fake host's listing, which the tests of every verb that copies a directory rely on.

use hostkeeper_mdm_step::Host;
use hostkeeper_mdm_step::fake::FakeHost;

/// Catches a fake listing that reaches below the directory, lists a sibling that shares the
/// name's prefix, lists a file as a directory, or ignores an unreadable directory: only the
/// direct entries come back, sorted, a file is an error, nothing there is `None`, and a denied
/// read is "permission denied" as on the machine.
#[test]
fn the_fake_lists_only_the_direct_entries_of_a_directory() {
    let host = FakeHost::new(0);
    host.put_dir("/d", 0o700, (0, 0));
    host.put_file("/d/b", 0o600, (0, 0), b"x");
    host.put_dir("/d/a", 0o700, (0, 0));
    host.put_file("/d/a/deep", 0o600, (0, 0), b"x");
    host.put_file("/dd", 0o600, (0, 0), b"x");
    host.put_file("/d-other/x", 0o600, (0, 0), b"x");
    assert_eq!(
        host.list("/d").unwrap(),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(host.list("/d/a").unwrap(), Some(vec!["deep".to_owned()]));
    assert_eq!(host.list("/none").unwrap(), None);
    assert_eq!(
        host.list("/d/b").unwrap_err().to_string(),
        "fake host: not a directory"
    );
    host.deny_read("/d");
    assert_eq!(
        host.list("/d").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}
