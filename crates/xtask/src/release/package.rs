//! The Linux package tools of the release: [`nfpm_stage`] is `release nfpm-stage`: the pinned
//! `gh`, the Sigstore trusted root and the two static programs of the daemon (`hostkeeper`,
//! `hostkeeper-pxe`), checked and laid out for the deb and the rpm that
//! `packaging/linux/nfpm.yaml` builds; [`linux_check`] is `release check-linux-pkg`, which reads
//! a built deb, rpm or tarball back and refuses an ELF program that is not static and any
//! declared dependency, and with a staged directory checks that the package installs the staged
//! files; [`elf`] is the ELF reader they share.

mod elf;
pub(super) mod linux_check;
pub(super) mod nfpm_stage;

/// Why a package other than hello takes no `--fail-ready`: only hello has that flag
/// to give its units.
pub(super) const FAIL_READY_ONLY_HELLO: &str =
    "--fail-ready is an option of the hello package; hostkeeper has no such flag";
