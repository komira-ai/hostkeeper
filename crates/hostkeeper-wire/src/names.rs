//! The names the wire and the certificates carry: a role, a host and a platform.
//!
//! A [`RoleName`] is an open validated string: any well-formed name is a role, and this crate
//! knows no list of them. A [`HostId`] is a DNS label. A [`Platform`] is one of the platforms
//! a release is built for, spelled as it is on the wire.

use std::fmt;
use std::str::FromStr;

/// The longest role name, in bytes.
const ROLE_MAX_LEN: usize = 32;

/// What a host runs, which decides its turn in an update and how many hosts of its kind may
/// update at once.
///
/// A name is lower-case ASCII letters, digits and `-`: 1 to 32 bytes, starting with a
/// letter, with no trailing or doubled `-`. Any such name is a role; whether a service has the
/// role is for its definition to say. [`FromStr`] is the only way to make one from text, and
/// serde reads and writes it as a string that is validated on read.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct RoleName(String);

/// A string that is no role name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a role name: 1 to 32 bytes of a-z, 0-9 and -, starting with a letter")]
pub struct RoleNameError;

impl RoleName {
    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name as text: [`RoleName::as_str`].
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl FromStr for RoleName {
    type Err = RoleNameError;

    fn from_str(name: &str) -> Result<Self, RoleNameError> {
        let bytes = name.as_bytes();
        let well_formed = (1..=ROLE_MAX_LEN).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            && !name.ends_with('-')
            && !name.contains("--");
        if !well_formed {
            return Err(RoleNameError);
        }
        Ok(Self(name.to_owned()))
    }
}

impl TryFrom<String> for RoleName {
    type Error = RoleNameError;

    fn try_from(name: String) -> Result<Self, RoleNameError> {
        name.parse()
    }
}

impl From<RoleName> for String {
    fn from(role: RoleName) -> Self {
        role.0
    }
}

impl fmt::Display for RoleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a string is not a [`HostId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostIdError {
    /// The id is empty.
    #[error("a host id is empty")]
    Empty,
    /// The id is longer than 63 characters.
    #[error("a host id is longer than {} characters", HostId::MAX_LEN)]
    TooLong,
    /// The id has a character other than `a-z`, `0-9` and `-`.
    #[error("a host id has a character outside a-z, 0-9 and -")]
    Character,
    /// The id starts or ends with `-`.
    #[error("a host id starts or ends with -")]
    Edge,
}

/// A host's id: `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`. It is the certificate's one DNS name,
/// so it is a valid DNS label and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(String);

impl HostId {
    /// The longest id: a DNS label.
    pub const MAX_LEN: usize = 63;

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for HostId {
    type Err = HostIdError;

    fn from_str(text: &str) -> Result<Self, HostIdError> {
        if text.is_empty() {
            return Err(HostIdError::Empty);
        }
        if text.len() > Self::MAX_LEN {
            return Err(HostIdError::TooLong);
        }
        if !text
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(HostIdError::Character);
        }
        if text.starts_with('-') || text.ends_with('-') {
            return Err(HostIdError::Edge);
        }
        Ok(Self(text.to_owned()))
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A platform a release is built for: the `<platform>` of `<name>-<commit>-<platform>.tar.gz`.
/// A host's platform comes from the `os` and `arch` entries of its node report
/// ([`Platform::from_report`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    /// `linux-x86_64`.
    LinuxX86_64,
    /// `linux-arm64`.
    LinuxArm64,
    /// `darwin-arm64`.
    DarwinArm64,
}

impl Platform {
    /// Every platform, in the order [`Ord`] gives.
    pub const ALL: [Self; 3] = [Self::LinuxX86_64, Self::LinuxArm64, Self::DarwinArm64];

    /// The platform of a node that reports `os` (`linux` or `macos`) and `arch`
    /// (`x86_64` or `arm64`); `None` for any other pair, including `macos` on `x86_64`,
    /// for which no release is built.
    #[must_use]
    pub fn from_report(os: &str, arch: &str) -> Option<Self> {
        match (os, arch) {
            ("linux", "x86_64") => Some(Self::LinuxX86_64),
            ("linux", "arm64") => Some(Self::LinuxArm64),
            ("macos", "arm64") => Some(Self::DarwinArm64),
            _ => None,
        }
    }

    /// The name in file names and on the wire: `linux-x86_64`, `linux-arm64` or
    /// `darwin-arm64`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxArm64 => "linux-arm64",
            Self::DarwinArm64 => "darwin-arm64",
        }
    }

    /// The platform [`Platform::as_str`] names `name` for; `None` for any other text (the
    /// match is exact: no case folding, no trimming).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches: a role closed to a list: names no fixed list holds are roles, and the
    /// name reads back as written through every accessor.
    #[test]
    fn a_role_name_is_open() {
        for name in [
            "cache",
            "gpu-builder",
            "frontend",
            "mac-daemon",
            "a",
            "build-farm-2",
            "x1",
            &"a".repeat(32),
        ] {
            let role: RoleName = name.parse().unwrap_or_else(|e| panic!("{name:?}: {e}"));
            assert_eq!(role.as_str(), name);
            assert_eq!(role.name(), name);
            assert_eq!(role.to_string(), name);
            assert_eq!(String::from(role), name);
        }
    }

    /// Catches: a malformed role name reaching a certificate, a row or an order: case, spaces,
    /// lookalike letters, separators, the hyphen rules and the length bound.
    #[test]
    fn malformed_role_names_are_refused() {
        let too_long = "a".repeat(33);
        for bad in [
            "",
            "Frontend",
            "CACHE",
            "ca che",
            " cache",
            "cache ",
            "\tcache",
            "cache\n",
            "c\u{430}che",
            "\u{ff43}ache",
            "caché",
            "1abc",
            "-cache",
            "a-",
            "a--b",
            "ca_che",
            "ca.che",
            "ca/che",
            "ca\0che",
            "\0",
            too_long.as_str(),
        ] {
            assert_eq!(bad.parse::<RoleName>(), Err(RoleNameError), "{bad:?}");
        }
        assert_eq!(
            RoleNameError.to_string(),
            "not a role name: 1 to 32 bytes of a-z, 0-9 and -, starting with a letter"
        );
    }

    /// Catches: a role name that serde reads without the grammar, or writes as anything but a
    /// plain string.
    #[test]
    fn a_role_name_is_a_validated_string_in_serde() {
        let role: RoleName = serde_json::from_str("\"gpu-builder\"").expect("a role");
        assert_eq!(
            serde_json::to_string(&role).expect("json"),
            "\"gpu-builder\""
        );
        for bad in ["\"Frontend\"", "\"a--b\"", "\"\"", "7", "null"] {
            assert!(serde_json::from_str::<RoleName>(bad).is_err(), "{bad}");
        }
        assert_eq!(RoleName::try_from("cache".to_owned()), "cache".parse());
        assert_eq!(RoleName::try_from("Cache".to_owned()), Err(RoleNameError));
    }

    /// Catches: roles ordered or hashed by anything but their text.
    #[test]
    fn role_names_order_by_their_text() {
        let a: RoleName = "alpha".parse().expect("a role");
        let b: RoleName = "beta".parse().expect("a role");
        assert!(a < b);
        assert_eq!(a.clone(), a);
    }

    /// Catches: a host id the certificate's DNS name would disagree about; each refusal is the
    /// first rule it breaks.
    #[test]
    fn host_ids_are_dns_labels() {
        let longest = "a".repeat(63);
        let hyphens = format!("a{}b", "-".repeat(61));
        for ok in ["a", "node-02", "0", "a-b-c", "m1", &longest, &hyphens] {
            let id: HostId = ok.parse().expect("a host id");
            assert_eq!(id.as_str(), ok);
            assert_eq!(id.to_string(), ok);
        }
        let too_long = "a".repeat(64);
        let cases = [
            ("", HostIdError::Empty),
            (too_long.as_str(), HostIdError::TooLong),
            ("Node", HostIdError::Character),
            ("node_02", HostIdError::Character),
            ("node.02", HostIdError::Character),
            ("n\u{f6}de", HostIdError::Character),
            ("node 02", HostIdError::Character),
            ("-node", HostIdError::Edge),
            ("node-", HostIdError::Edge),
            ("-", HostIdError::Edge),
        ];
        for (text, want) in cases {
            assert_eq!(text.parse::<HostId>().err(), Some(want), "{text:?}");
        }
    }

    /// Catches: an error that reads as nothing, or still says "node id".
    #[test]
    fn every_host_id_error_says_what_is_wrong() {
        assert_eq!(HostIdError::Empty.to_string(), "a host id is empty");
        assert_eq!(
            HostIdError::TooLong.to_string(),
            "a host id is longer than 63 characters"
        );
        assert_eq!(
            HostIdError::Character.to_string(),
            "a host id has a character outside a-z, 0-9 and -"
        );
        assert_eq!(
            HostIdError::Edge.to_string(),
            "a host id starts or ends with -"
        );
    }

    /// Catches: host ids ordered by anything but their text.
    #[test]
    fn host_ids_order_by_their_text() {
        let a: HostId = "h1".parse().expect("a host id");
        let b: HostId = "h2".parse().expect("a host id");
        assert!(a < b);
        assert_eq!(a.clone(), a);
    }

    /// Catches: a pair mapped to another platform, `macos` on `x86_64` or an arch spelled
    /// `aarch64` accepted, an `os` of another case accepted, and any pair outside the table
    /// mapped to something.
    #[test]
    fn a_node_report_maps_to_its_platform() {
        let table = [
            ("linux", "x86_64", Some(Platform::LinuxX86_64)),
            ("linux", "arm64", Some(Platform::LinuxArm64)),
            ("linux", "aarch64", None),
            ("linux", "", None),
            ("macos", "arm64", Some(Platform::DarwinArm64)),
            ("macos", "x86_64", None),
            ("macos", "aarch64", None),
            ("macos", "", None),
            ("darwin", "arm64", None),
            ("Linux", "x86_64", None),
            ("windows", "x86_64", None),
            ("", "x86_64", None),
            ("", "", None),
        ];
        for (os, arch, want) in table {
            assert_eq!(Platform::from_report(os, arch), want, "{os}/{arch}");
        }
    }

    /// Catches: a name that does not read back as its platform, a platform listed twice or
    /// missing from `ALL`, text that is not exactly a name accepted, and a `Display` that
    /// differs from the name.
    #[test]
    fn platform_names_round_trip() {
        for platform in Platform::ALL {
            assert_eq!(Platform::parse(platform.as_str()), Some(platform));
            assert_eq!(platform.to_string(), platform.as_str());
        }
        let names: Vec<_> = Platform::ALL.map(Platform::as_str).to_vec();
        assert_eq!(names, ["linux-x86_64", "linux-arm64", "darwin-arm64"]);
        for bad in [
            "",
            "linux",
            "Linux-x86_64",
            "linux-x86_64 ",
            "darwin-x86_64",
            "macos-arm64",
        ] {
            assert_eq!(Platform::parse(bad), None, "{bad:?}");
        }
    }
}
