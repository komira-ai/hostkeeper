//! The serial console baseline's fake ([`FakeSerialConsole`]), beside the other fakes of
//! [`crate::testing`].

use std::sync::Mutex;

use crate::ports::SerialConsoleSource;
use crate::serial_console::SerialConsoleReport;

/// The baseline, as a test sets it: none until [`FakeSerialConsole::set`].
#[derive(Default)]
pub struct FakeSerialConsole {
    report: Mutex<Option<SerialConsoleReport>>,
}

impl FakeSerialConsole {
    pub fn set(&self, report: Option<SerialConsoleReport>) {
        *super::lock(&self.report) = report;
    }
}

impl SerialConsoleSource for FakeSerialConsole {
    fn report(&self) -> Option<SerialConsoleReport> {
        super::lock(&self.report).clone()
    }
}
