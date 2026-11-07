//! The platforms network install has a boot set for: `linux-x86_64` and `linux-arm64`
//! (docs/design/network-install.md, section 3). A Mac never boots from the network.

use std::fmt;
use std::str::FromStr;

/// A Linux platform with a boot set. Ordered by spelling, so the platforms of a run are fetched,
/// sealed and listed in the order of their names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BootPlatform {
    /// `linux-arm64`.
    LinuxArm64,
    /// `linux-x86_64`.
    LinuxX86_64,
}

impl BootPlatform {
    /// Both, in the order of their spellings.
    pub const ALL: [BootPlatform; 2] = [BootPlatform::LinuxArm64, BootPlatform::LinuxX86_64];

    /// The spelling, as `netboot.json` keys its boot sets: `linux-arm64`, `linux-x86_64`.
    pub const fn as_str(self) -> &'static str {
        match self {
            BootPlatform::LinuxArm64 => "linux-arm64",
            BootPlatform::LinuxX86_64 => "linux-x86_64",
        }
    }

    /// The architecture as a deb names it: `arm64`, `amd64`.
    pub const fn deb_arch(self) -> &'static str {
        match self {
            BootPlatform::LinuxArm64 => "arm64",
            BootPlatform::LinuxX86_64 => "amd64",
        }
    }

    /// The architecture as a UEFI boot file names it: `aa64`, `x64`.
    pub const fn efi_arch(self) -> &'static str {
        match self {
            BootPlatform::LinuxArm64 => "aa64",
            BootPlatform::LinuxX86_64 => "x64",
        }
    }
}

impl fmt::Display for BootPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A spelling that is neither Linux platform.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a platform with a boot set: linux-x86_64 or linux-arm64")]
pub struct NoBootSet(pub String);

impl FromStr for BootPlatform {
    type Err = NoBootSet;

    fn from_str(text: &str) -> Result<BootPlatform, NoBootSet> {
        BootPlatform::ALL
            .into_iter()
            .find(|platform| platform.as_str() == text)
            .ok_or_else(|| NoBootSet(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each platform reads back from its spelling, and carries the deb and UEFI architecture
    /// names Ubuntu's files use; every other spelling, a Mac's included, is refused by name.
    ///
    /// Catches an arm64 machine given the amd64 deb or the x64 shim, and a Mac taken as a
    /// platform that boots from the network.
    #[test]
    fn each_platform_has_its_spelling_and_architecture_names() {
        let names: Vec<(String, &str, &str)> = BootPlatform::ALL
            .iter()
            .map(|p| (p.to_string(), p.deb_arch(), p.efi_arch()))
            .collect();
        assert_eq!(
            names,
            [
                ("linux-arm64".to_owned(), "arm64", "aa64"),
                ("linux-x86_64".to_owned(), "amd64", "x64"),
            ]
        );
        for platform in BootPlatform::ALL {
            assert_eq!(platform.as_str().parse(), Ok(platform));
        }
        assert_eq!(
            "macos-arm64".parse::<BootPlatform>(),
            Err(NoBootSet("macos-arm64".to_owned()))
        );
        assert_eq!(
            NoBootSet("x".to_owned()).to_string(),
            "\"x\" is not a platform with a boot set: linux-x86_64 or linux-arm64"
        );
    }
}
