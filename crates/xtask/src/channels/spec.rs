//! What the channel verbs know of the product they publish, as data: the product's pool
//! directory in the shared repository, the prefix of its role sessions, and its two Linux
//! packages. Tierstore's verbs had these as constants of one package; here the package's name,
//! its file names and the rules that differ between the daemon's package and the coordinator's
//! are the values below, and no verb names `hostkeeper` or `hostkeeper-coordinator` itself.
//!
//! New in this repository; the rest of [`super`] is the copy of komira-ai/tierstore.
//!
//! What the install checks hold an installed package to ([`Package::files`], the user, whether
//! the unit is enabled after the install, the packages to install first) and the stages the
//! promotion decision reads ([`Product::stages`]) are data here too, and so are the names and the
//! place of the macOS installer ([`Product::pkg_file`], [`Product::pkg_dir`]).
//!
//! The index, write and promote verbs take every package of the product in one pass
//! ([`Product::deb_files`], [`Product::rpm_files`]): the daemon's and the coordinator's, each for
//! both architectures, so a release is listed in the repository whole or not at all.

/// A Linux architecture, in the two names the package formats give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Arch {
    /// The Debian name (`amd64`, `arm64`).
    pub(crate) deb: &'static str,
    /// The RPM name (`x86_64`, `aarch64`).
    pub(crate) rpm: &'static str,
}

/// The architectures every package is built for.
pub(crate) const ARCHES: [Arch; 2] = [
    Arch {
        deb: "amd64",
        rpm: "x86_64",
    },
    Arch {
        deb: "arm64",
        rpm: "aarch64",
    },
];

/// The release number of every rpm: `nfpm` writes `1` and the release assets name it.
pub(crate) const RPM_RELEASE: &str = "1";

/// One Linux package of the product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Package {
    /// The package name: the deb's `Package`, the rpm's name, and the start of both file names.
    pub(crate) name: &'static str,
    /// Whether the rpm has a `%postun` script. `nfpm` writes `%preun` (from `preremove`) and no
    /// `%postun` for either package, so neither requires one; the signing reads it only where
    /// the spec says it is there.
    pub(crate) postun_required: bool,
    /// The program the package installs: its absolute path. The file name is the program's
    /// name, and `<name> --version` prints `<name> <version>+<12 digits of the commit>`.
    pub(crate) binary: &'static str,
    /// The systemd unit of the package, a file of `/usr/lib/systemd/system`.
    pub(crate) unit: &'static str,
    /// The system user the install creates, if it creates one.
    pub(crate) user: Option<&'static str>,
    /// Whether the install enables the unit. The daemon's postinstall enables it on a first
    /// install; the coordinator's enables nothing, because it cannot start unconfigured. Neither
    /// package starts its unit.
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
    pub(crate) packages: [Package; 2],
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

    /// The file name of the macOS installer of `version`: `<name>-<version>-macos-arm64.pkg`. It
    /// is unsigned and unnotarized until the owner has a Developer ID; nothing here signs it.
    pub(crate) fn pkg_file(&self, version: &str) -> String {
        format!("{}-{version}-macos-arm64.pkg", self.name)
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

    /// The file names of the debs of `version`, every package for every architecture, sorted.
    pub(crate) fn deb_files(&self, version: &str) -> Vec<String> {
        self.entries(version, Package::deb_file)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    /// The file names of the rpms of `version`, every package for every architecture, sorted.
    pub(crate) fn rpm_files(&self, version: &str) -> Vec<String> {
        self.rpm_entries(version)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    /// The rpms of `version`, every package for every architecture, as the package and the file
    /// name, sorted by file name.
    pub(crate) fn rpm_entries(&self, version: &str) -> Vec<(&Package, String)> {
        self.entries(version, Package::rpm_file)
    }

    fn entries(
        &self,
        version: &str,
        name: impl Fn(&Package, &str, &Arch) -> String,
    ) -> Vec<(&Package, String)> {
        let mut entries: Vec<(&Package, String)> = self
            .packages
            .iter()
            .flat_map(|p| ARCHES.iter().map(move |a| (p, a)))
            .map(|(p, a)| (p, name(p, version, a)))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        entries
    }
}

/// hostkeeper: the daemon's package and the coordinator's.
pub(crate) const HOSTKEEPER: Product = Product {
    name: "hostkeeper",
    pool_dir: "pool/hostkeeper",
    session_prefix: "hostkeeper-",
    pkg_dir: "pkg/hostkeeper",
    packages: [
        Package {
            name: "hostkeeper",
            postun_required: false,
            binary: "/usr/bin/hostkeeper",
            unit: "hostkeeper.service",
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
            postun_required: false,
            binary: "/usr/bin/hostkeeper-coordinator",
            unit: "hostkeeper-coordinator.service",
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
    ],
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
