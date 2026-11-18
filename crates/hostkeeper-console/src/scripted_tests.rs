//! The scripted double itself: what it answers and records.

use super::{Scripted, Seen};
use crate::credential::BmcCredential;
use crate::ipmi::{Invocation, Settings, ShellError, Tool};
use crate::targets::Address;

/// Proves: a held session's open waits until released, and reports that it began; its commands
/// are answered in turn and recorded with the open and the close, and an open with nothing
/// scripted is a tool that cannot start.
///
/// Catches: a double that lets a held open through, which would make the coordinator's test of
/// commands to one BMC pass whether or not they wait for each other.
#[test]
fn a_held_session_opens_only_when_released() {
    let tool = Scripted::new();
    let credential =
        BmcCredential::parse("user = \"u\"\npassword = \"p\"\n").expect("a credential");
    let address = Address::Ip("192.0.2.1".parse().expect("an address"));
    let held = tool.held_session(&["first\n"]);
    let opener = tool.clone();
    let opening = std::thread::spawn(move || {
        let invocation = Invocation::new(&address, &credential, &Settings::default(), false);
        let mut shell = opener.open(&invocation).map_err(|_| "refused")?;
        shell.ask("one").map_err(|_| "ended")
    });
    held.entered.recv().expect("the open began");
    assert_eq!(tool.seen().len(), 1);
    held.release.send(()).expect("released");
    assert_eq!(opening.join().expect("joined"), Ok("first\n".to_owned()));
    assert_eq!(
        tool.seen()[1..],
        [Seen::Ask("one".to_owned()), Seen::Closed]
    );

    let credential =
        BmcCredential::parse("user = \"u\"\npassword = \"p\"\n").expect("a credential");
    let address = Address::Ip("192.0.2.1".parse().expect("an address"));
    let invocation = Invocation::new(&address, &credential, &Settings::default(), false);
    assert_eq!(tool.open(&invocation).err(), Some(ShellError::Start));
}
