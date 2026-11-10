//! `hostkeeper-pxe`, the library of the `pxe` program, the boot server of network install
//! (docs/design/network-install.md, sections 3 and 5; docs/design/two-programs.md, section 6.1).
//! `pxe serve` (item N9) serves what the `prepare` step of the `pxe` package's hook (item N8, the
//! fetch) sealed ([`serve`]).
//!
//! The fetch ([`fetch`]) makes a new directory and writes into it, for each Linux platform asked
//! for (both by default):
//!
//! - the boot set `netboot.json` pins: the Ubuntu live-server ISO, downloaded from the URL the
//!   pin names, and the kernel, the initrd, the shim and the network GRUB, taken out of that ISO
//!   (the shim and GRUB out of the debs in its package pool);
//! - the hostkeeper deb of the platform, from the same release as `netboot.json`.
//!
//! Each file's size and SHA-256 are measured in the one pass that writes it, and each boot file
//! must match its pin. `netboot.json` and each deb must be attested by the daemon config's trusted
//! signer, checked by the daemon's own verifier (`hostkeeper_package::gh`). The last file written
//! is the seal ([`seal`]), which names every file and its size and SHA-256. On any refusal the
//! directory is removed, so nothing of a refused fetch is kept.
//!
//! The modules, in reading order:
//!
//! - [`cli`]: the verbs of `pxe`: `serve` and the steps of the hook;
//! - `settings`: the settings file, expanded into the flags of `serve` and of the fetch;
//! - `hook`: the six steps of the package's update hook, over the boot directory and `systemctl`;
//! - [`fetch`]: the order of the steps of the fetch, the ports it runs on, and every refusal;
//! - [`netboot`]: `netboot.json`, read for the platforms asked for;
//! - `download`, `copy`: one HTTPS GET into a file, measured as it is written;
//! - `attest`: one asset's bundle, read and handed to the verifier within its deadline;
//! - `iso`: an ISO 9660 image with Rock Ridge names, read for one file's extent;
//! - `deb`: a deb, read for one file of its `data.tar` (zstd, xz, gzip or none);
//! - [`seal`]: what the seal holds;
//! - `repositories`: the repository files `serve --repositories` checks and reads;
//! - [`serve`]: the start of `serve` (the files read as root, the sockets, the privilege drop of
//!   [`privilege`], the seal check, the readiness notice), and the run: the answer set read from
//!   the coordinator for each answer, DHCP in proxy or router mode, TFTP and HTTP.

pub mod cli;
pub mod fetch;
pub mod netboot;
pub mod platform;
pub mod privilege;
pub mod seal;
pub mod serve;

mod attest;
mod copy;
mod deb;
mod download;
mod hook;
mod iso;
mod repositories;
mod settings;

#[cfg(test)]
mod testing;

pub use attest::AttestError;
pub use fetch::{FetchError, FileId, Part, Ports, Request, Taken, fetch};
pub use platform::BootPlatform;
pub use seal::Seal;
