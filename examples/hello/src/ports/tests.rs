use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use super::exit_code;

/// Catches a program that a signal ended being read as one that exited with a code (a killed
/// `systemctl` must not pass for a success), and a real exit code being lost.
#[test]
fn a_program_ended_by_a_signal_has_no_exit_code() {
    // The raw wait status: the exit code in the second byte, a signal in the low bits.
    assert_eq!(exit_code("p", ExitStatus::from_raw(0)), Ok(0));
    assert_eq!(exit_code("p", ExitStatus::from_raw(3 << 8)), Ok(3));
    let why = exit_code("systemctl", ExitStatus::from_raw(9)).expect_err("a signal has no code");
    assert!(
        why.starts_with("systemctl ended without an exit code: "),
        "{why}"
    );
}
