//! What the channel verbs know of the product they publish, as data: the product's pool
//! directory in the shared repository, the prefix of its role sessions, its Linux packages and
//! its macOS installers. Tierstore's verbs had these as constants of one package; here the
//! packages' names, their file names and the rules that differ between one package and another
//! are the values below, and the verbs read them from [`HOSTKEEPER`], the one product of this
//! repository.
//!
//! New in this repository; the rest of [`super`] is the copy of komira-ai/tierstore.
//!
//! What the install checks hold an installed package to ([`Package::files`], the user, whether
//! the units are enabled after the install, the packages to install first, what the program says
//! for `--version`) and the stages the promotion decision reads ([`Product::stages`]) are data
//! here too, and so are the names and the place of the macOS installers
//! ([`Product::pkg_files`], [`Product::pkg_dir`]).
//!
//! A release of this repository publishes the daemon's and the coordinator's packages, and the
//! native packages of the services it releases (docs/design/packages-native.md, item A21):
//! hello's `.deb` for both architectures and mdm's for amd64 alone (neither has an rpm), and
//! beside hostkeeper's own installer hello's `.pkg`. The index, write and promote verbs take every
//! package of the product in one pass ([`Product::deb_files`], [`Product::rpm_files`]), so a
//! release is listed in the repository whole or not at all. Hello's failing build is not one of
//! them: its `.deb`s are listed in the suite `testing-fail` alone ([`Product::fail_ready_debs`]).

use crate::release::contract::{FAIL_READY_DEB_SUFFIX, FAIL_READY_SUFFIX};

/// A Linux architecture, in the two names the package formats give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arch {
    /// The Debian name (`amd64`, `arm64`).
    pub(crate) deb: &'static str,
    /// The RPM name (`x86_64`, `aarch64`).
    pub(crate) rpm: &'static str,
}

/// The x86-64 architecture.
pub(crate) const AMD64: Arch = Arch {
    deb: "amd64",
    rpm: "x86_64",
};

/// The 64-bit Arm architecture.
pub(crate) const ARM64: Arch = Arch {
    deb: "arm64",
    rpm: "aarch64",
};

/// The architectures of the repository: every package is built for one or both.
pub(crate) const ARCHES: [Arch; 2] = [AMD64, ARM64];

/// The release number of every rpm: `nfpm` writes `1` and the release assets name it.
pub(crate) const RPM_RELEASE: &str = "1";

/// One Linux package of the product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Package {
    /// The package name: the deb's `Package`, the rpm's name, and the start of both file names.
    pub(crate) name: &'static str,
    /// The architectures the package is built for: mdm's `.deb` is built for amd64 alone (its
    /// pinned tools are amd64 builds).
    pub(crate) arches: &'static [Arch],
    /// Whether the package is also an rpm. The services' native packages are `.deb`s alone
    /// (docs/design/packages-native.md: `dnf` is later).
    pub(crate) rpm: bool,
    /// Whether the rpm has a `%postun` script. `nfpm` writes `%preun` (from `preremove`) and no
    /// `%postun` for either package, so neither requires one; the signing reads it only where
    /// the spec says it is there.
    pub(crate) postun_required: bool,
    /// The program the package installs: its absolute path. The file name is the program's
    /// name, and `<name> --version` prints `<name> <version>`, followed by `+<12 digits of the
    /// commit>` when [`Package::stamp_commit`] says so.
    pub(crate) binary: &'static str,
    /// Whether the program's `--version` carries the commit: the shipped binaries' does
    /// (`hostkeeper X.Y.Z+<commit>`), hello's says `hello X.Y.Z` alone.
    pub(crate) stamp_commit: bool,
    /// The systemd units of the package, files of `/usr/lib/systemd/system`: the check holds
    /// each enabled or not as [`Package::enabled_after_install`] says.
    pub(crate) units: &'static [&'static str],
    /// The system user the install creates, if it creates one.
    pub(crate) user: Option<&'static str>,
    /// Whether the install enables the units. The daemon's postinstall enables its unit on a
    /// first install; the coordinator's enables nothing, because it cannot start unconfigured;
    /// a service's native package has no script at all, its units enabled by hostkeeper's
    /// installer alone. No package starts a unit.
    pub(crate) enabled_after_install: bool,
    /// The packages the check installs before the package: the daemon's deb declares no
    /// dependency, but its postinstall runs `systemd-tmpfiles` and `systemctl`.
    pub(crate) prerequisites: &'static [&'static str],
    /// Every file the install puts on the system, as `nfpm.yaml` lists them: the check holds each
    /// to be a file (the issue link of the daemon points at a file that exists on a running host
    /// only, so it is not listed).
    pub(crate) files: &'static [&'static str],
}

impl Package {
    /// The name of the program, the file name of [`Package::binary`].
    pub(crate) fn program(&self) -> &'static str {
        self.binary.rsplit('/').next().unwrap_or(self.binary)
    }

    /// The name the kernel gives a process of the program (`comm`): its first 15 bytes.
    pub(crate) fn comm(&self) -> &'static str {
        let program = self.program();
        &program[..program.len().min(15)]
    }

    /// The file name of the deb of `version` for `arch`: `<name>_<version>_<arch>.deb`.
    pub(crate) fn deb_file(&self, version: &str, arch: &Arch) -> String {
        format!("{}_{version}_{}.deb", self.name, arch.deb)
    }

    /// The file name of the rpm of `version` for `arch`: `<name>-<version>-1.<arch>.rpm`.
    pub(crate) fn rpm_file(&self, version: &str, arch: &Arch) -> String {
        format!("{}-{version}-{RPM_RELEASE}.{}.rpm", self.name, arch.rpm)
    }
}

/// A product published into the repository the organisation's products share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Product {
    /// The product's name.
    pub(crate) name: &'static str,
    /// The directory of its packages below `apt/` and `rpm/` in the bucket. The product's role
    /// may write exactly the objects below `apt/<pool_dir>/` and `rpm/<pool_dir>/` with
    /// `If-None-Match`.
    pub(crate) pool_dir: &'static str,
    /// What every session name of the product's roles starts with.
    pub(crate) session_prefix: &'static str,
    /// The directory of the macOS installer's versions below `pkg/` in the bucket.
    pub(crate) pkg_dir: &'static str,
    /// Its packages, in the order the verbs list them.
    pub(crate) packages: &'static [Package],
    /// The names of the macOS installers a release ships, each `<name>-<version>-macos-arm64.pkg`
    /// in the version's directory: the product's own and the native packages of its services.
    pub(crate) installers: &'static [&'static str],
    /// The name of the package whose failing build (the same program started with `--fail-ready`)
    /// the suite `testing-fail` lists alone.
    pub(crate) fail_ready: &'static str,
    /// The jobs `channels decide` reads, in the order it names them.
    pub(crate) stages: &'static [&'static str],
    /// The stages that may be `skipped`; every other stage must be `success`.
    pub(crate) may_skip: &'static [&'static str],
}

impl Product {
    /// The package called `name`.
    pub(crate) fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// The file names of the macOS installers of `version`, `<installer>-<version>-macos-arm64.pkg`
    /// for each of [`Product::installers`], sorted. Each is signed by the release that built it
    /// and not notarized; nothing here signs them.
    pub(crate) fn pkg_files(&self, version: &str) -> Vec<String> {
        let mut files: Vec<String> = self
            .installers
            .iter()
            .map(|name| format!("{name}-{version}-macos-arm64.pkg"))
            .collect();
        files.sort();
        files
    }

    /// The key prefix of the installer's version, `pkg/<product>/<version>/`: the `testing`
    /// channel of the installer.
    pub(crate) fn pkg_prefix(&self, version: &str) -> String {
        format!("{}/{version}/", self.pkg_dir)
    }

    /// The key of the pointer to the promoted installer, the `stable` channel of it.
    pub(crate) fn pkg_stable_key(&self) -> String {
        format!("{}/stable.json", self.pkg_dir)
    }

    /// The file names of the debs of `version`, every package for each of its architectures,
    /// sorted.
    pub(crate) fn deb_files(&self, version: &str) -> Vec<String> {
        self.entries(version, |_| true, Package::deb_file)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    /// The debs of the failing build of `version`, for each architecture of its package, as the
    /// file name the pool and the index give it (`<name>_<version>~fail.ready_<arch>.deb`) and the
    /// file name of the release asset (`<name>_<version>-fail-ready_<arch>.deb`: an asset name
    /// has no `~`), sorted.
    pub(crate) fn fail_ready_debs(&self, version: &str) -> Vec<(String, String)> {
        let pool = format!("{version}{FAIL_READY_DEB_SUFFIX}");
        let asset = format!("{version}{FAIL_READY_SUFFIX}");
        let mut files: Vec<(String, String)> = self
            .package(self.fail_ready)
            .into_iter()
            .flat_map(|p| p.arches.iter().map(move |a| (p, a)))
            .map(|(p, a)| (p.deb_file(&pool, a), p.deb_file(&asset, a)))
            .collect();
        files.sort();
        files
    }

    /// The file names of the rpms of `version`, every package that is an rpm for each of its
    /// architectures, sorted.
    pub(crate) fn rpm_files(&self, version: &str) -> Vec<String> {
        self.rpm_entries(version)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    /// The rpms of `version`, every package that is an rpm for each of its architectures, as the
    /// package and the file name, sorted by file name.
    pub(crate) fn rpm_entries(&self, version: &str) -> Vec<(&Package, String)> {
        self.entries(version, |p| p.rpm, Package::rpm_file)
    }

    fn entries(
        &self,
        version: &str,
        keep: impl Fn(&Package) -> bool,
        name: impl Fn(&Package, &str, &Arch) -> String,
    ) -> Vec<(&Package, String)> {
        let mut entries: Vec<(&Package, String)> = self
            .packages
            .iter()
            .filter(|p| keep(p))
            .flat_map(|p| p.arches.iter().map(move |a| (p, a)))
            .map(|(p, a)| (p, name(p, version, a)))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        entries
    }
}

/// hostkeeper: the daemon's package and the coordinator's, hello's and mdm's native packages, and
/// the installers of hostkeeper and of hello.
pub(crate) const HOSTKEEPER: Product = Product {
    name: "hostkeeper",
    pool_dir: "pool/hostkeeper",
    session_prefix: "hostkeeper-",
    pkg_dir: "pkg/hostkeeper",
    fail_ready: "hello",
    packages: &[
        Package {
            name: "hostkeeper",
            arches: &ARCHES,
            rpm: true,
            postun_required: false,
            binary: "/usr/bin/hostkeeper",
            stamp_commit: true,
            units: &["hostkeeper.service"],
            user: None,
            enabled_after_install: true,
            prerequisites: &["systemd"],
            files: &[
                "/usr/bin/hostkeeper",
                "/usr/bin/hostkeeper-pxe",
                "/usr/lib/systemd/system/hostkeeper.service",
                "/usr/lib/tmpfiles.d/hostkeeper.conf",
                "/usr/share/hostkeeper/hostkeeper.toml.template",
                "/usr/lib/hostkeeper/gh",
                "/usr/lib/hostkeeper/trusted_root.json",
            ],
        },
        Package {
            name: "hostkeeper-coordinator",
            arches: &ARCHES,
            rpm: true,
            postun_required: false,
            binary: "/usr/bin/hostkeeper-coordinator",
            stamp_commit: true,
            units: &["hostkeeper-coordinator.service"],
            user: Some("hostkeeper-coordinator"),
            enabled_after_install: false,
            prerequisites: &[],
            files: &[
                "/usr/bin/hostkeeper-coordinator",
                "/usr/lib/systemd/system/hostkeeper-coordinator.service",
                "/usr/lib/sysusers.d/hostkeeper-coordinator.conf",
                "/usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
                "/usr/share/hostkeeper-coordinator/serve.toml.template",
                "/usr/share/hostkeeper-coordinator/ui/index.html",
            ],
        },
        Package {
            name: "hello",
            arches: &ARCHES,
            rpm: false,
            postun_required: false,
            binary: "/usr/lib/hello/hello",
            stamp_commit: false,
            units: &["hello@a.service", "hello@b.service"],
            user: None,
            enabled_after_install: false,
            prerequisites: &[],
            files: &[
                "/usr/lib/hello/hello",
                "/usr/lib/systemd/system/hello@a.service",
                "/usr/lib/systemd/system/hello@b.service",
            ],
        },
        Package {
            name: "mdm",
            arches: &[AMD64],
            rpm: false,
            postun_required: false,
            binary: "/usr/lib/mdm/mdm",
            stamp_commit: true,
            units: &MDM_UNITS,
            user: None,
            enabled_after_install: false,
            prerequisites: &[],
            files: &[
                "/usr/lib/mdm/mdm",
                "/usr/lib/systemd/system/mdm-cert-renew.service",
                "/usr/lib/systemd/system/mdm-cert.service",
                "/usr/lib/systemd/system/mdm-cert.timer",
                "/usr/lib/systemd/system/mdm-depserver.service",
                "/usr/lib/systemd/system/mdm-depsyncer.service",
                "/usr/lib/systemd/system/mdm-nanohub.service",
                "/usr/lib/systemd/system/mdm-proxy.service",
                "/usr/lib/systemd/system/mdm-scep.service",
            ],
        },
    ],
    installers: &["hello", "hostkeeper"],
    stages: &[
        "testing-apt-write",
        "testing-apt-write-2",
        "testing-apt-write-3",
        "testing-rpm-write",
        "testing-rpm-write-2",
        "testing-rpm-write-3",
        "testing-pkg-upload",
        "check-apt",
        "check-rpm",
        "check-pkg",
    ],
    may_skip: &[
        "testing-apt-write-2",
        "testing-apt-write-3",
        "testing-rpm-write-2",
        "testing-rpm-write-3",
        "check-apt",
        "check-rpm",
        "check-pkg",
    ],
};

/// mdm's units, as its `.deb` installs them (`hostkeeper_mdm_core::package::UNITS`).
const MDM_UNITS: [&str; 8] = [
    "mdm-cert-renew.service",
    "mdm-cert.service",
    "mdm-cert.timer",
    "mdm-depserver.service",
    "mdm-depsyncer.service",
    "mdm-nanohub.service",
    "mdm-proxy.service",
    "mdm-scep.service",
];

/// The architecture whose RPM name is `rpm`.
pub(crate) fn arch_of_rpm(rpm: &str) -> Option<&'static Arch> {
    ARCHES.iter().find(|a| a.rpm == rpm)
}

/// The architecture whose Debian name is `deb`.
pub(crate) fn arch_of_deb(deb: &str) -> Option<&'static Arch> {
    ARCHES.iter().find(|a| a.deb == deb)
}

#[cfg(test)]
mod tests;
