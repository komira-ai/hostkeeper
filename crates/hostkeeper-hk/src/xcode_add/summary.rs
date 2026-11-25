//! The seam where the summary of the Macs that will install the release runs
//! (docs/design/xcode-one-action.md section 2.5): after `stored:` and after
//! `already in the store`. It prints nothing yet; the summary lands here.

use crate::World;
use crate::args::Globals;

/// Prints which Macs will install the release `version` (`build`), and when.
pub(crate) fn macs(_w: &mut World<'_>, _globals: &Globals, _version: &str, _build: &str) {}
