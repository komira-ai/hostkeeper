//! The version directory of a package on the host (`hostkeeper_install::version`): the publish of
//! a staging directory an installer filled, the faults and crashes of its port, and the formats it
//! writes. The modules are in `version/`.

#[path = "version/faults.rs"]
mod faults;
#[path = "version/fill.rs"]
mod fill;
#[path = "version/formats.rs"]
mod formats;
#[path = "version/support.rs"]
mod support;
