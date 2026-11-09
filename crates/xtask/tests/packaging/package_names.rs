//! The file names of the deb and the rpm `nfpm.yaml` makes for each Linux platform.

use crate::nfpm::Config;
use crate::{NFPM, altered, read};

/// The platforms of hostkeeper's packages: the closed list of the release index (one package
/// per OS and CPU family). Held here until hostkeeper-config's shared `Platform` replaces it.
#[derive(Clone, Copy, Debug)]
enum Platform {
    LinuxX86_64,
    LinuxArm64,
    MacosArm64,
}

impl Platform {
    const ALL: [Platform; 3] = [
        Platform::LinuxX86_64,
        Platform::LinuxArm64,
        Platform::MacosArm64,
    ];

    /// The `ARCH` nfpm builds the platform's deb and rpm with; none for macOS, whose package
    /// is a pkg, not built here.
    fn nfpm_arch(self) -> Option<&'static str> {
        match self {
            Platform::LinuxX86_64 => Some("amd64"),
            Platform::LinuxArm64 => Some("arm64"),
            Platform::MacosArm64 => None,
        }
    }
}

/// The file names of the deb and then the rpm of every Linux platform,
/// for version 1.2.3.
fn file_names(text: &str) -> Result<Vec<String>, String> {
    let config = Config::parse(text)?;
    let mut out = Vec::new();
    for packager in ["deb", "rpm"] {
        for arch in Platform::ALL.iter().filter_map(|p| p.nfpm_arch()) {
            out.push(config.file_name(packager, "1.2.3", arch)?);
        }
    }
    Ok(out)
}

/// Catches a package name, version or architecture that does not come out as
/// `hostkeeper_<version>_<arch>.deb` and `hostkeeper-<version>-1.<arch>.rpm` for both
/// architectures, or an Arch package still named after its withdrawal, or a value read from any other variable of the environment.
#[test]
fn package_file_names_follow_the_pattern() {
    let text = read(NFPM);
    let wanted = [
        "hostkeeper_1.2.3_amd64.deb",
        "hostkeeper_1.2.3_arm64.deb",
        "hostkeeper-1.2.3-1.x86_64.rpm",
        "hostkeeper-1.2.3-1.aarch64.rpm",
    ];
    assert_eq!(file_names(&text), Ok(wanted.map(str::to_owned).to_vec()));
    assert_eq!(
        Config::parse(&text).and_then(|c| c.file_name("archlinux", "1.2.3", "amd64")),
        Err("no packager \"archlinux\"".to_owned())
    );
    let names = |t: &str| file_names(t).map(|names| names.join(" "));
    // A fixed architecture names the arm64 packages amd64.
    let fixed = names(&altered(&text, "arch: \"${ARCH}\"", "arch: amd64"));
    assert_eq!(
        fixed,
        Ok("hostkeeper_1.2.3_amd64.deb hostkeeper_1.2.3_amd64.deb hostkeeper-1.2.3-1.x86_64.rpm hostkeeper-1.2.3-1.x86_64.rpm".to_owned())
    );
    let typo = names(&altered(&text, "name: hostkeeper\n", "name: hostkeper\n"));
    assert!(
        typo.as_ref().is_ok_and(|n| !n.contains("hostkeeper_")),
        "{typo:?}"
    );
    for (from, to, words) in [
        (
            "version: \"${VERSION}\"",
            "version: \"${VERSION}-${GIT_SHA}\"",
            "version is",
        ),
        (
            "arch: \"${ARCH}\"",
            "arch: \"${HOSTKEEPER_ARCH}\"",
            "arch is",
        ),
        ("name: hostkeeper\n", "", "nfpm.yaml has no name"),
    ] {
        let refused = names(&altered(&text, from, to)).expect_err(to);
        assert!(refused.contains(words), "{refused:?} lacks {words:?}");
    }
    // Only the inputs nfpm.yaml documents: a version nfpm would rewrite, or an architecture
    // that is not one of the Linux platforms', is refused, not given a name nfpm would not use.
    let config = Config::parse(&text).expect("nfpm.yaml parses");
    for (version, arch, refusal) in [
        (
            "v1.2.3",
            "amd64",
            "VERSION \"v1.2.3\" is not MAJOR.MINOR.PATCH in digits",
        ),
        (
            "1.2.3-rc.1",
            "amd64",
            "VERSION \"1.2.3-rc.1\" is not MAJOR.MINOR.PATCH in digits",
        ),
        (
            "1.2.3+build.5",
            "amd64",
            "VERSION \"1.2.3+build.5\" is not MAJOR.MINOR.PATCH in digits",
        ),
        (
            "1.2",
            "amd64",
            "VERSION \"1.2\" is not MAJOR.MINOR.PATCH in digits",
        ),
        (
            "",
            "amd64",
            "VERSION \"\" is not MAJOR.MINOR.PATCH in digits",
        ),
        ("1.2.3", "", "ARCH \"\" is not amd64 or arm64"),
        ("1.2.3", "x86_64", "ARCH \"x86_64\" is not amd64 or arm64"),
    ] {
        for packager in ["deb", "rpm"] {
            assert_eq!(
                config.file_name(packager, version, arch),
                Err(refusal.to_owned()),
                "{packager} {version:?} {arch:?}"
            );
        }
    }
}
