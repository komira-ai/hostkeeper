//! The platforms a package is built for: one per operating system and CPU family
//! (docs/design/hostkeeper-design.md, section 5.5).
//!
//! The set is closed. The daemon knows its own platform from the target it was compiled for
//! ([`Platform::current`]), never from a config file or from the machine at run time. Which
//! instruction-set level runs inside a package is the build system's business, not this type's.

use std::fmt;
use std::str::FromStr;

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
compile_error!("hostkeeper runs only on linux-x86_64, linux-arm64 and macos-arm64");

/// One operating system and CPU family. Ordered by spelling, so a sorted list of platforms is
/// sorted the same way as their spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    // Declared in the order of the spellings, which the derived order follows.
    /// `linux-arm64`.
    LinuxArm64,
    /// `linux-x86_64`.
    LinuxX86_64,
    /// `macos-arm64`.
    MacosArm64,
}

/// A spelling that is none of the three platforms.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a platform: one of linux-x86_64, linux-arm64, macos-arm64")]
pub struct UnknownPlatform(pub String);

impl Platform {
    /// Every platform, in the order of their spellings.
    pub const ALL: [Platform; 3] = [
        Platform::LinuxArm64,
        Platform::LinuxX86_64,
        Platform::MacosArm64,
    ];

    /// The spelling: `linux-x86_64`, `linux-arm64` or `macos-arm64`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::MacosArm64 => "macos-arm64",
        }
    }

    /// The platform of the target this code was compiled for.
    pub const fn current() -> Platform {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let platform = Platform::LinuxX86_64;
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let platform = Platform::LinuxArm64;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let platform = Platform::MacosArm64;
        platform
    }
}

impl FromStr for Platform {
    type Err = UnknownPlatform;

    /// The platform of exactly one of the three spellings; no other case or spelling.
    fn from_str(text: &str) -> Result<Platform, UnknownPlatform> {
        Platform::ALL
            .into_iter()
            .find(|platform| platform.as_str() == text)
            .ok_or_else(|| UnknownPlatform(text.to_owned()))
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
