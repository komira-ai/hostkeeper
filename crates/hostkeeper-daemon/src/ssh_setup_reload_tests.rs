//! The reload rule of `ssh_setup` (design rule 4): when a record makes sshd read its files
//! again, over the scripted host of the parent module's tests.

use super::*;

/// A second setup over the rig's host and paths: the daemon after a restart, holding nothing.
fn restarted(rig: &Rig) -> SshSetup {
    SshSetup::new(
        Arc::clone(&rig.host) as Arc<dyn SshHost>,
        rig.paths.clone(),
        rustix::process::geteuid().as_raw(),
        rig.log.clone(),
    )
}

/// Proves the same generation again does nothing, and a new generation with the same content
/// checks and reads the banner but writes nothing, reloads nothing and builds no new list.
#[test]
fn a_record_again_changes_nothing() {
    let rig = Rig::new("ssh-again");
    rig.apply(&access(1));
    let before = rig.host.calls();
    let effective_calls = rig.host.lock().effective_calls;
    assert_eq!(rig.apply(&access(1)).applied_generation, 1);
    assert_eq!(rig.host.calls(), before);
    assert_eq!(
        rig.host.lock().effective_calls,
        effective_calls,
        "nothing was asked of sshd"
    );

    let report = rig.apply(&access(2));
    assert_eq!(
        (report.applied_generation, report.state.as_str()),
        (2, "applied")
    );
    assert_eq!(
        rig.host.calls()[before.len()..],
        ["start", "check", "banner 127.0.0.1:22"]
    );
}

/// Proves under socket activation a change asks the host to reload the socket's sshd before the
/// banner, as for a service: whether a socket has a listening sshd to reload is the host's to
/// say (`SystemHost::reload`), and the setup never takes it that one does not.
/// Catches: a setup that skips the reload for a socket unit, so that a listener the socket
/// started before the change keeps refusing every certificate.
#[test]
fn a_socket_is_reloaded_by_the_host_after_a_change() {
    let rig = Rig::new("ssh-socket");
    rig.host.lock().unit = Ok(SshdUnit::Socket);
    assert_eq!(rig.apply(&access(1)).state, "applied");
    assert_eq!(
        rig.host.calls(),
        [
            "start",
            "revocations",
            "check",
            "reload Socket",
            "banner 127.0.0.1:22"
        ]
        .map(str::to_owned)
    );
}

/// Proves the first record a daemon takes up after it starts reloads sshd even when every file
/// is already what the record asks for (an earlier start wrote them), and that the same
/// content at a later generation then reloads nothing.
/// Catches: a daemon upgraded from one that wrote the files and never had the listening sshd
/// read them, which would stay `applied` with every certificate refused.
#[test]
fn the_first_record_after_a_start_reloads_files_already_in_place() {
    let rig = Rig::new("ssh-restart");
    rig.apply(&access(1));
    let written = rig.files();
    let restarted = restarted(&rig);
    let before = rig.host.calls().len();
    restarted.apply(&access(1));
    assert_eq!(
        restarted.report().map(|report| report.state),
        Some("applied".to_owned())
    );
    assert_eq!(rig.files(), written);
    assert_eq!(
        rig.host.calls()[before..],
        [
            "start",
            "revocations",
            "check",
            "reload",
            "banner 127.0.0.1:22"
        ]
    );
    let again = rig.host.calls().len();
    restarted.apply(&access(2));
    assert_eq!(
        rig.host.calls()[again..],
        ["start", "check", "banner 127.0.0.1:22"]
    );
}

/// Proves a record refused before any file is written does not count as the first record: a
/// daemon that restarts over files already in place, whose first record is refused (`sshd -t`
/// fails), still reloads at the next record that applies, though no file changes then.
/// Catches: a listener that never reads the files because the reload was due only at the first
/// record after a start, and that record was refused.
#[test]
fn a_refused_first_record_leaves_the_reload_for_the_next_one_that_applies() {
    let rig = Rig::new("ssh-restart-refused");
    rig.apply(&access(1));
    let restarted = restarted(&rig);
    rig.host.lock().candidate_fails = true;
    restarted.apply(&access(2));
    assert_eq!(
        restarted.report().map(|report| report.state),
        Some("config_invalid".to_owned())
    );
    rig.host.lock().candidate_fails = false;
    let before = rig.host.calls().len();
    restarted.apply(&access(3));
    assert_eq!(
        restarted.report().map(|report| report.state),
        Some("applied".to_owned())
    );
    assert!(
        rig.host.calls()[before..].contains(&"reload".to_owned()),
        "{:?}",
        rig.host.calls()
    );
}
