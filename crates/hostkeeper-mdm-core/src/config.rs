//! The kit's settings: `KEY=VALUE` lines over [`KEYS`], checked into [`MdmConfig`]
//! before anything reads the host.
//!
//! The keys that name a site (the DNS name, the LAN, the user and directories, the
//! organisation, the profile identifier) have no default, because each default would
//! be one site's value. The keys that name the kit's own parts (the unit prefix, the
//! slice, the SCEP CA's and the DEP certificate's names, the ADE profile's text and the
//! signature namespace) have a default derived from the unit prefix and the
//! organisation, so a site that wants no particular names sets none. `config.example`
//! in this crate documents every key.
//!
//! The ACME keys are read by [`crate::acme::Acme`]; `MDM_ERASE_STATE`,
//! `MDM_ERASE_PRINCIPAL` and `MDM_ERASE_MAX_AGE` are accepted and not read here, so one
//! file serves the whole kit.

use std::net::Ipv4Addr;
use std::str::FromStr;

use crate::dns;
use crate::path::AbsPath;
use crate::settings::{Settings, SettingsError};

/// Every key the settings file may set.
pub const KEYS: &[&str] = &[
    "MDM_DNS_NAME",
    "MDM_BIND",
    "MDM_LAN_CIDR",
    "MDM_USER",
    "MDM_STATE",
    "MDM_ETC",
    "MDM_BIN",
    "MDM_PKG_DIR",
    "MDM_ERASE_STATE",
    "MDM_NANOHUB_PORT",
    "MDM_SCEP_PORT",
    "MDM_DEP_PORT",
    "MDM_MEMORY_MAX",
    "MDM_CPU_QUOTA",
    "MDM_TASKS_MAX",
    "MDM_UNIT_PREFIX",
    "MDM_SLICE",
    "MDM_DEP_NAME",
    "MDM_ORG",
    "MDM_PROFILE_ID",
    "MDM_PROFILE_DISPLAY_NAME",
    "MDM_ADE_PROFILE_NAME",
    "MDM_ADE_ORG_MAGIC",
    "MDM_ADE_SKIP_ITEMS",
    "MDM_LANGUAGE",
    "MDM_REGION",
    "MDM_SCEP_CA_CN",
    "MDM_SCEP_CA_OU",
    "MDM_SCEP_CA_YEARS",
    "MDM_DEP_CERT_CN",
    "MDM_DEP_CERT_DAYS",
    "MDM_ACME_EMAIL",
    "MDM_ACME_SERVER",
    "MDM_ACME_DNS_PROVIDER",
    "MDM_ACME_RESOLVERS",
    "MDM_ACME_PROPAGATION_WAIT",
    "MDM_SIGNATURE_NAMESPACE",
    "MDM_ERASE_PRINCIPAL",
    "MDM_ERASE_MAX_AGE",
];

/// The default unit prefix: the kit's own name.
pub const DEFAULT_UNIT_PREFIX: &str = "hostkeeper-mdm";

/// The Setup Assistant panes an enrolled Mac skips unless `MDM_ADE_SKIP_ITEMS` says
/// otherwise.
pub const DEFAULT_SKIP_ITEMS: [&str; 18] = [
    "Accessibility",
    "Appearance",
    "AppleID",
    "AppStore",
    "Biometric",
    "Diagnostics",
    "DisplayTone",
    "FileVault",
    "iCloudDiagnostics",
    "iCloudStorage",
    "Intelligence",
    "Privacy",
    "ScreenTime",
    "Siri",
    "TermsOfAddress",
    "TOS",
    "UnlockWithWatch",
    "Wallpaper",
];

/// The settings the unit, profile and command builders read, checked. [`MdmConfig::parse`]
/// is the only way to make one, and every field is read through an accessor, so a value
/// that did not pass the parser's checks cannot reach a builder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdmConfig {
    dns_name: String,
    bind: Ipv4Addr,
    lan: Cidr,
    user: String,
    state: AbsPath,
    etc: AbsPath,
    bin: AbsPath,
    pkg_dir: AbsPath,
    nanohub_port: u16,
    scep_port: u16,
    dep_port: u16,
    memory_max: String,
    cpu_quota: String,
    tasks_max: String,
    unit_prefix: String,
    slice: String,
    dep_name: String,
    org: String,
    profile_id: String,
    profile_display_name: String,
    ade: AdeSettings,
    scep_ca: ScepCa,
    dep_cert: DepCert,
    signature_namespace: String,
}

impl MdmConfig {
    /// The name the Macs dial; the TLS certificate is for it.
    pub fn dns_name(&self) -> &str {
        &self.dns_name
    }

    /// The LAN address the HTTPS proxy binds: private, inside [`MdmConfig::lan`].
    pub fn bind(&self) -> Ipv4Addr {
        self.bind
    }

    /// The LAN, the only network besides loopback the proxy talks to.
    pub fn lan(&self) -> Cidr {
        self.lan
    }

    /// The services' system user.
    pub fn user(&self) -> &str {
        &self.user
    }

    /// State: database, SCEP CA, DEP token, TLS key.
    pub fn state(&self) -> &AbsPath {
        &self.state
    }

    /// Unit inputs and the env files.
    pub fn etc(&self) -> &AbsPath {
        &self.etc
    }

    /// The pinned binaries.
    pub fn bin(&self) -> &AbsPath {
        &self.bin
    }

    /// The pkg directory: the hostkeeper pkgs the proxy serves at `/pkg/<sha256>.pkg`, root's,
    /// and readable by every user (a release pkg is public).
    pub fn pkg_dir(&self) -> &AbsPath {
        &self.pkg_dir
    }

    /// NanoHUB's loopback port, which neither other port equals.
    pub fn nanohub_port(&self) -> u16 {
        self.nanohub_port
    }

    /// The SCEP server's loopback port, which neither other port equals.
    pub fn scep_port(&self) -> u16 {
        self.scep_port
    }

    /// NanoDEP's depserver loopback port, which neither other port equals.
    pub fn dep_port(&self) -> u16 {
        self.dep_port
    }

    /// The slice's memory cap, as systemd writes it: `1G`.
    pub fn memory_max(&self) -> &str {
        &self.memory_max
    }

    /// The slice's CPU cap: `100%`.
    pub fn cpu_quota(&self) -> &str {
        &self.cpu_quota
    }

    /// The slice's task cap: `256`.
    pub fn tasks_max(&self) -> &str {
        &self.tasks_max
    }

    /// The prefix of every unit name.
    pub fn unit_prefix(&self) -> &str {
        &self.unit_prefix
    }

    /// The slice's name, ending in `.slice`.
    pub fn slice(&self) -> &str {
        &self.slice
    }

    /// NanoDEP's name for the Apple Business Manager token.
    pub fn dep_name(&self) -> &str {
        &self.dep_name
    }

    /// The organisation shown with the SCEP CA and the profiles.
    pub fn org(&self) -> &str {
        &self.org
    }

    /// The enrollment profile's identifier, reverse-DNS (`com.example.hostkeeper.mdm`).
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// The enrollment profile's name in System Settings.
    pub fn profile_display_name(&self) -> &str {
        &self.profile_display_name
    }

    /// The ADE profile: name, org_magic, language, region, skipped panes.
    pub fn ade(&self) -> &AdeSettings {
        &self.ade
    }

    /// The SCEP CA's subject and validity.
    pub fn scep_ca(&self) -> &ScepCa {
        &self.scep_ca
    }

    /// The DEP token certificate's name and validity.
    pub fn dep_cert(&self) -> &DepCert {
        &self.dep_cert
    }

    /// The ssh signature namespace of an erase request.
    pub fn signature_namespace(&self) -> &str {
        &self.signature_namespace
    }
}

/// The ADE profile's settings, part of an [`MdmConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdeSettings {
    profile_name: String,
    org_magic: String,
    language: String,
    region: String,
    skip_items: Vec<String>,
}

impl AdeSettings {
    /// The profile's name at Apple.
    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// The profile's `org_magic`.
    pub fn org_magic(&self) -> &str {
        &self.org_magic
    }

    /// The language, two lower-case letters.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The region, two upper-case letters.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// The Setup Assistant panes skipped, in the order given, at least one.
    pub fn skip_items(&self) -> &[String] {
        &self.skip_items
    }
}

/// The SCEP CA's subject and validity, part of an [`MdmConfig`]. The CA is the issuer of
/// every Mac's MDM identity certificate, so it is fixed before the first Mac enrolls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScepCa {
    common_name: String,
    organizational_unit: String,
    years: u32,
}

impl ScepCa {
    /// The common name.
    pub fn common_name(&self) -> &str {
        &self.common_name
    }

    /// The organizational unit.
    pub fn organizational_unit(&self) -> &str {
        &self.organizational_unit
    }

    /// The validity in years, 1 to 30.
    pub fn years(&self) -> u32 {
        self.years
    }
}

/// The DEP token certificate's name and validity, part of an [`MdmConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepCert {
    common_name: String,
    days: u32,
}

impl DepCert {
    /// The common name.
    pub fn common_name(&self) -> &str {
        &self.common_name
    }

    /// The validity in days, 1 to 365.
    pub fn days(&self) -> u32 {
        self.days
    }
}

/// A private IPv4 network, `a.b.c.d/bits`, that stays inside the RFC 1918 block of its
/// address: at least 8 bits in 10/8, 12 in 172.16/12 and 16 in 192.168/16, and at most 30.
/// [`Cidr::new`] and its [`FromStr`] are the only ways to make one. A wider network would be
/// read by systemd as public space: the /8 of an address in 192.168/16 is the /8 of 192.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: Ipv4Addr,
    bits: u8,
}

/// Why an address and a prefix length are not a [`Cidr`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CidrError {
    /// Not `a.b.c.d/bits`, with the bits as decimal digits.
    #[error("{0:?} is not a.b.c.d/bits")]
    Malformed(String),
    /// An address outside the three private blocks.
    #[error("{0} is not in a private (RFC 1918) block")]
    NotPrivate(Ipv4Addr),
    /// A prefix length wider than the address's private block, or above 30.
    #[error("{addr}/{bits}: the prefix is not from {min} to 30 bits")]
    Bits {
        /// The address.
        addr: Ipv4Addr,
        /// The prefix length given.
        bits: u8,
        /// The least the address's private block allows.
        min: u8,
    },
}

impl Cidr {
    /// The network of `addr` with a prefix of `bits`, or why it is not a private one.
    pub fn new(addr: Ipv4Addr, bits: u8) -> Result<Cidr, CidrError> {
        if !addr.is_private() {
            return Err(CidrError::NotPrivate(addr));
        }
        let min = match addr.octets()[0] {
            10 => 8,
            172 => 12,
            _ => 16,
        };
        if !(min..=30).contains(&bits) {
            return Err(CidrError::Bits { addr, bits, min });
        }
        Ok(Cidr { addr, bits })
    }

    /// The address the network was given with (its host bits are kept).
    pub fn addr(self) -> Ipv4Addr {
        self.addr
    }

    /// The prefix length, 8 to 30.
    pub fn bits(self) -> u8 {
        self.bits
    }

    /// Whether `a` is inside the network.
    pub fn contains(self, a: Ipv4Addr) -> bool {
        // `bits` is 8 to 30 (`Cidr::new`), so the shift is 2 to 24 and cannot overflow.
        let mask = u32::MAX << (32 - u32::from(self.bits));
        u32::from(a) & mask == u32::from(self.addr) & mask
    }
}

impl FromStr for Cidr {
    type Err = CidrError;

    /// `a.b.c.d/bits`, checked by [`Cidr::new`].
    fn from_str(text: &str) -> Result<Cidr, CidrError> {
        let malformed = || CidrError::Malformed(text.to_owned());
        let (addr, bits) = text.split_once('/').ok_or_else(malformed)?;
        let addr: Ipv4Addr = addr.parse().map_err(|_| malformed())?;
        let digits = !bits.is_empty() && bits.bytes().all(|b| b.is_ascii_digit());
        let bits: u8 = bits.parse().ok().filter(|_| digits).ok_or_else(malformed)?;
        Cidr::new(addr, bits)
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.bits)
    }
}

/// Why the settings were refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The file is not `KEY=VALUE` over the closed key set, or lacks a required key.
    #[error(transparent)]
    Settings(#[from] SettingsError),
    /// A key whose value is not acceptable.
    #[error("MDM_{key}={value}: {why}")]
    Bad {
        /// The key without its `MDM_` prefix.
        key: &'static str,
        /// The value read.
        value: String,
        /// What is wrong with it.
        why: &'static str,
    },
}

pub(crate) fn bad(key: &'static str, value: &str, why: &'static str) -> ConfigError {
    ConfigError::Bad {
        key,
        value: value.to_owned(),
        why,
    }
}

/// Refuse `value` of `key` with `why` unless `ok`.
pub(crate) fn check(
    key: &'static str,
    value: &str,
    ok: bool,
    why: &'static str,
) -> Result<(), ConfigError> {
    if ok {
        Ok(())
    } else {
        Err(bad(key, value, why))
    }
}

/// Reads keys by their name without the `MDM_` prefix.
pub(crate) struct Reader<'a>(pub(crate) &'a Settings);

impl Reader<'_> {
    /// A required key's value.
    pub(crate) fn get(&self, key: &str) -> Result<String, ConfigError> {
        Ok(self.0.require(&format!("MDM_{key}"))?.to_owned())
    }

    /// A key's value, `default` when the file does not set it.
    pub(crate) fn get_or(&self, key: &str, default: &str) -> Result<String, ConfigError> {
        Ok(self.0.get_or(&format!("MDM_{key}"), default)?)
    }

    fn path(&self, key: &'static str) -> Result<AbsPath, ConfigError> {
        checked_path(key, &self.get(key)?)
    }

    /// A path, `default` when the file does not set it.
    fn path_or(&self, key: &'static str, default: &str) -> Result<AbsPath, ConfigError> {
        checked_path(key, &self.get_or(key, default)?)
    }
    fn port(&self, key: &'static str) -> Result<u16, ConfigError> {
        let text = self.get(key)?;
        let port = text.parse::<u16>().ok().filter(|p| *p != 0);
        port.ok_or(bad(key, &text, "not a port from 1 to 65535"))
    }

    /// A systemd cap: digits from 1 (no leading zero), then one of `suffixes`.
    fn capped(
        &self,
        key: &'static str,
        suffixes: &[char],
        why: &'static str,
    ) -> Result<String, ConfigError> {
        let text = self.get(key)?;
        check(key, &text, is_count(&text, suffixes), why)?;
        Ok(text)
    }

    /// A whole number in `1..=max`, `default` when the file does not set it.
    fn number(&self, key: &'static str, default: u32, max: u32) -> Result<u32, ConfigError> {
        let text = self.get_or(key, &default.to_string())?;
        let n = text.parse::<u32>().ok().filter(|n| (1..=max).contains(n));
        n.ok_or(bad(key, &text, "not a whole number in the allowed range"))
    }

    /// A name of letters, digits and `extra`, `default` when the file does not set it.
    fn name(
        &self,
        key: &'static str,
        default: &str,
        extra: &str,
        why: &'static str,
    ) -> Result<String, ConfigError> {
        let text = self.get_or(key, default)?;
        check(key, &text, is_name(&text, extra), why)?;
        Ok(text)
    }

    /// Free text without a control character, `default` when the file does not set it.
    fn text(&self, key: &'static str, default: &str) -> Result<String, ConfigError> {
        let text = self.get_or(key, default)?;
        check(
            key,
            &text,
            !text.chars().any(char::is_control),
            "holds a control character",
        )?;
        Ok(text)
    }
}

impl MdmConfig {
    /// Check the settings in `text`.
    pub fn parse(text: &str) -> Result<MdmConfig, ConfigError> {
        let settings = Settings::parse(text, KEYS)?;
        let r = Reader(&settings);
        let dns_name = r.get("DNS_NAME")?;
        check(
            "DNS_NAME",
            &dns_name,
            is_dns_name(&dns_name),
            "not a lower-case DNS name with a dot (at most 63 characters a label, 253 in all)",
        )?;
        let text = r.get("BIND")?;
        let bind = lan_address(&text).ok_or(bad(
            "BIND",
            &text,
            "not a private LAN address (an RFC 1918 range): the proxy binds the LAN only, never 0.0.0.0, loopback or another range",
        ))?;
        let text = r.get("LAN_CIDR")?;
        let lan = text.parse::<Cidr>().map_err(|_| bad(
            "LAN_CIDR",
            &text,
            "not a private IPv4 network in CIDR form inside one RFC 1918 block (at least 8 bits in 10/8, 12 in 172.16/12, 16 in 192.168/16; at most 30)",
        ))?;
        check(
            "BIND",
            &bind.to_string(),
            lan.contains(bind),
            "not inside MDM_LAN_CIDR (a container bridge's or another network's address?)",
        )?;
        let user = r.get("USER")?;
        check(
            "USER",
            &user,
            is_user_name(&user),
            "not a user name ([a-z_][a-z0-9_-]{0,30})",
        )?;
        // The units' sandboxing assumes an account without privilege.
        check(
            "USER",
            &user,
            !["root", "toor"].contains(&user.as_str()),
            "a superuser account: the services run unprivileged",
        )?;
        let (state, etc, bin) = (r.path("STATE")?, r.path("ETC")?, r.path("BIN")?);
        // The state is the service user's to write: no other directory may be inside it,
        // or equal to it, or contain it (a binary the user can replace is not pinned).
        for (key, path, other, why) in [
            ("ETC", &etc, &state, "equal to, inside or around MDM_STATE"),
            ("BIN", &bin, &state, "equal to, inside or around MDM_STATE"),
            ("BIN", &bin, &etc, "equal to, inside or around MDM_ETC"),
        ] {
            let clash = nested(path.as_str(), other.as_str());
            check(key, path.as_str(), !clash, why)?;
        }
        let (nanohub_port, scep_port, dep_port) = (
            r.port("NANOHUB_PORT")?,
            r.port("SCEP_PORT")?,
            r.port("DEP_PORT")?,
        );
        // The three services listen on loopback side by side: two on one port would leave
        // one of them unable to start, or the proxy sending its routes to the other.
        for (key, port, other, why) in [
            (
                "SCEP_PORT",
                scep_port,
                nanohub_port,
                "the same port as MDM_NANOHUB_PORT",
            ),
            (
                "DEP_PORT",
                dep_port,
                nanohub_port,
                "the same port as MDM_NANOHUB_PORT",
            ),
            (
                "DEP_PORT",
                dep_port,
                scep_port,
                "the same port as MDM_SCEP_PORT",
            ),
        ] {
            check(key, &port.to_string(), port != other, why)?;
        }
        let memory_max = r.capped("MEMORY_MAX", &['K', 'M', 'G'], "not like 1G")?;
        let cpu_quota = r.capped("CPU_QUOTA", &['%'], "not like 100%")?;
        let tasks_max = r.capped("TASKS_MAX", &[], "not a number from 1")?;
        let unit_prefix = r.get_or("UNIT_PREFIX", DEFAULT_UNIT_PREFIX)?;
        check(
            "UNIT_PREFIX",
            &unit_prefix,
            is_unit_stem(&unit_prefix),
            "not lower-case letters, digits and - from a letter, at most 40 characters",
        )?;
        // The pkg directory is served to anyone on the LAN and read by the gate: no directory
        // of the kit may be it, hold it or be inside it.
        let pkg_dir = r.path_or("PKG_DIR", &format!("/var/lib/{unit_prefix}-pkg"))?;
        for (other, why) in [
            (&state, "equal to, inside or around MDM_STATE"),
            (&etc, "equal to, inside or around MDM_ETC"),
            (&bin, "equal to, inside or around MDM_BIN"),
        ] {
            let clash = nested(pkg_dir.as_str(), other.as_str());
            check("PKG_DIR", pkg_dir.as_str(), !clash, why)?;
        }
        let slice = r.get_or("SLICE", &format!("{unit_prefix}.slice"))?;
        let stem = slice.strip_suffix(".slice").unwrap_or_default();
        check(
            "SLICE",
            &slice,
            is_unit_stem(stem),
            "not a slice name: lower-case letters, digits and - from a letter, then .slice",
        )?;
        // systemd's own slices (`system`, `user`, `machine`, `init`, and what nests in
        // them) are not the kit's to cap; the kit's slice is its prefix or below it.
        let top = stem.split('-').next().unwrap_or_default();
        check(
            "SLICE",
            &slice,
            !["system", "user", "machine", "init"].contains(&top),
            "a slice of systemd's own",
        )?;
        check(
            "SLICE",
            &slice,
            stem == unit_prefix || stem.starts_with(&format!("{unit_prefix}-")),
            "not MDM_UNIT_PREFIX or a slice below it (MDM_UNIT_PREFIX-...)",
        )?;
        let dep_name = r.get("DEP_NAME")?;
        check(
            "DEP_NAME",
            &dep_name,
            is_name(&dep_name, "._-"),
            "not a name of letters, digits, . _ - that starts with a letter or digit",
        )?;
        let org = r.get("ORG")?;
        check(
            "ORG",
            &org,
            !org.chars().any(char::is_control),
            "holds a control character",
        )?;
        let profile_id = r.get("PROFILE_ID")?;
        check(
            "PROFILE_ID",
            &profile_id,
            is_dns_name(&profile_id),
            "not a reverse-DNS identifier (lower-case labels joined by dots, at most 63 characters a label, 253 in all)",
        )?;
        let profile_display_name = r.text("PROFILE_DISPLAY_NAME", &format!("{org} MDM"))?;
        let ade = AdeSettings {
            profile_name: r.text("ADE_PROFILE_NAME", &format!("{org} MDM"))?,
            org_magic: r.name(
                "ADE_ORG_MAGIC",
                &unit_prefix,
                "._-",
                "not a name of letters, digits, . _ - that starts with a letter or digit",
            )?,
            language: r.get_or("LANGUAGE", "en")?,
            region: r.get_or("REGION", "US")?,
            skip_items: skip_items(&r.get_or("ADE_SKIP_ITEMS", &DEFAULT_SKIP_ITEMS.join(" "))?)?,
        };
        check(
            "LANGUAGE",
            &ade.language,
            is_letters(&ade.language, char::is_ascii_lowercase),
            "not two lower-case letters",
        )?;
        check(
            "REGION",
            &ade.region,
            is_letters(&ade.region, char::is_ascii_uppercase),
            "not two upper-case letters",
        )?;
        let scep_ca = ScepCa {
            common_name: r.text("SCEP_CA_CN", &format!("{org} MDM SCEP CA"))?,
            organizational_unit: r.text("SCEP_CA_OU", &unit_prefix)?,
            years: r.number("SCEP_CA_YEARS", 10, 30)?,
        };
        let dep_cert = DepCert {
            common_name: r.name(
                "DEP_CERT_CN",
                &unit_prefix,
                "._-",
                "not a name of letters, digits, . _ - that starts with a letter or digit",
            )?,
            days: r.number("DEP_CERT_DAYS", 7, 365)?,
        };
        let signature_namespace = r.name(
            "SIGNATURE_NAMESPACE",
            &format!("{unit_prefix}-erase"),
            "._-",
            "not a name of letters, digits, . _ - that starts with a letter or digit",
        )?;
        Ok(MdmConfig {
            dns_name,
            bind,
            lan,
            user,
            state,
            etc,
            bin,
            pkg_dir,
            nanohub_port,
            scep_port,
            dep_port,
            memory_max,
            cpu_quota,
            tasks_max,
            unit_prefix,
            slice,
            dep_name,
            org,
            profile_id,
            profile_display_name,
            ade,
            scep_ca,
            dep_cert,
            signature_namespace,
        })
    }
}

/// `text` as the path of `key`: an absolute path of characters the units can carry.
pub(crate) fn checked_path(key: &'static str, text: &str) -> Result<AbsPath, ConfigError> {
    check(
        key,
        text,
        is_word(text, "/._-"),
        "a path the units cannot carry (letters, digits, / . _ -)",
    )?;
    AbsPath::new(text).map_err(|_| {
        bad(
            key,
            text,
            "not an absolute path with no empty, . or .. part",
        )
    })
}

/// The skipped panes: words of letters and digits, at least one.
fn skip_items(text: &str) -> Result<Vec<String>, ConfigError> {
    let items: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    let ok = !items.is_empty() && items.iter().all(|i| is_word(i, ""));
    check(
        "ADE_SKIP_ITEMS",
        text,
        ok,
        "not Setup Assistant pane names (letters and digits) separated by spaces",
    )?;
    Ok(items)
}

/// A private LAN address: one of the RFC 1918 ranges.
pub fn lan_address(text: &str) -> Option<Ipv4Addr> {
    let a: Ipv4Addr = text.parse().ok()?;
    a.is_private().then_some(a)
}

/// Whether `a` and `b` are the same directory or one is inside the other.
fn nested(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// A lower-case DNS name with at least one dot, within DNS's length limits (63 octets a
/// label, 253 the name); no label starts or ends with `-`.
fn is_dns_name(name: &str) -> bool {
    dns::is_host_name(name, dns::Case::Lower, 2)
}

/// A whole number from 1, without leading zeros, then one of `suffixes` (none when
/// `suffixes` is empty).
fn is_count(text: &str, suffixes: &[char]) -> bool {
    let digits = if suffixes.is_empty() {
        Some(text)
    } else {
        text.strip_suffix(suffixes)
    };
    digits.is_some_and(|d| {
        !d.is_empty() && !d.starts_with('0') && d.chars().all(|c| c.is_ascii_digit())
    })
}

pub(crate) fn is_user_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 31
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Lower-case letters, digits and `-`, starting with a letter, at most 40 characters:
/// what a systemd unit name's stem is made of here.
pub(crate) fn is_unit_stem(name: &str) -> bool {
    let mut chars = name.chars();
    // systemd reads `-` in a slice name as a nesting step and refuses an empty one.
    name.len() <= 40
        && !name.ends_with('-')
        && !name.contains("--")
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A word that does not start with `-` (a flag to the program that takes it) or `.` (a
/// path step in the URL that carries it).
pub(crate) fn is_name(text: &str, extra: &str) -> bool {
    is_word(text, extra) && !text.starts_with(['-', '.'])
}

/// Only letters, digits and the characters of `extra` (the callers hold values that
/// are not empty: `require` refuses an empty one).
pub(crate) fn is_word(text: &str, extra: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// Exactly two ASCII letters, all passing `case`.
fn is_letters(text: &str, case: fn(&char) -> bool) -> bool {
    text.chars().count() == 2 && text.chars().all(|c| case(&c))
}
