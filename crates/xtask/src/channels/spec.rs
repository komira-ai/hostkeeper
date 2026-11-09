//! What the channel verbs know of the product they publish, as data: the product's pool
//! directory in the shared repository, the prefix of its role sessions, and its two Linux
//! packages. Tierstore's verbs had these as constants of one package; here the package's name,
//! its file names and the rules that differ between the daemon's package and the coordinator's
//! are the values below, and no verb names `hostkeeper` or `hostkeeper-coordinator` itself.
//!
//! New in this repository; the rest of [`super`] is the copy of komira-ai/tierstore.
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
}

impl Package {
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
    /// Its packages, in the order the verbs list them.
    pub(crate) packages: [Package; 2],
}

impl Product {
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
    packages: [
        Package {
            name: "hostkeeper",
            postun_required: false,
        },
        Package {
            name: "hostkeeper-coordinator",
            postun_required: false,
        },
    ],
};

#[cfg(test)]
mod tests;
