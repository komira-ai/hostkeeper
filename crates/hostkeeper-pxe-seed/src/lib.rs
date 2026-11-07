//! What a bare machine boots into when `hostkeeper-pxe` installs it
//! (docs/design/network-install.md, section 4; item N6 of its section 12): the fixed
//! `grub.cfg`, the install or local-disk menu of a MAC, and the machine's autoinstall
//! `user-data` and `meta-data`.
//!
//! - [`menu`]: [`menu::GRUB_CFG`], and [`menu::MenuRequest`], which reads the path GRUB asks
//!   for and chooses the menu of its MAC from the answer set, the record's platform and the
//!   [`menu::Mode`].
//! - [`seed`]: [`seed::seed`], the NoCloud seed of one machine.
//! - [`inputs`] and [`keys`]: the values both are made from, each checked once when it is made
//!   (the daemon config by the daemon's own parser, the SSH keys file with any private key
//!   refused), so neither generator can fail.
//! - [`layout`]: the TFTP and HTTP paths the menus and seeds name, which `hostkeeper-pxe serve`
//!   serves.
//!
//! The crate is pure: it reads no file, socket, clock or environment. Every output is a
//! function of its inputs alone, and goldens in `tests/golden` pin each one byte for byte.

pub mod inputs;
pub mod keys;
pub mod layout;
pub mod menu;
pub mod seed;
