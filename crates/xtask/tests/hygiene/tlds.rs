//! The top-level domains a bare dotted name is read as a host for, and the file suffixes that are
//! never read as one.

/// The generic top-level domains a bare dotted name is read as a host for: the public ones in
/// common use and the private-use ones (`internal`, `local`, `lan`, `corp`, `home`, `intranet`,
/// `private`, `localdomain`). Domains that are also plain English words used as field and key
/// names in code and prose (`name`, `group`, `new`, `link`, `host`, `server`, `build`, ...) are
/// left out: a dotted name ending in one is far more often a path than a host. A URL is read for
/// its host whatever its top-level domain.
#[rustfmt::skip]
pub const GENERIC_TLDS: &[&str] = &[
    "com", "net", "org", "edu", "gov", "mil", "int", "info", "biz", "pro", "mobi", "travel",
    "jobs", "asia", "coop", "aero", "museum", "arpa", "io", "dev", "ai", "app", "cloud", "co",
    "me", "xyz", "tech", "site", "online", "store", "page", "run", "blog", "shop", "website",
    "email", "agency", "studio", "gmbh", "ltd", "llc", "bank", "pub", "club", "icu", "lol", "wtf",
    "ninja", "rocks", "guru", "vip", "wiki", "cam", "finance", "fund", "fyi", "gallery",
    "graphics", "holdings", "industries", "investments", "partners", "photography", "productions",
    "properties", "reviews", "training", "university", "ventures", "vision", "academy",
    "builders", "careers", "foundation", "zone", "click", "internal", "local", "lan", "corp",
    "home", "intranet", "private", "localdomain",
];

/// The two-letter country-code top-level domains a bare dotted name is read as a host for: every
/// one except those of `FILE_SUFFIXES`.
#[rustfmt::skip]
pub const COUNTRY_TLDS: &[&str] = &[
    "ad", "ae", "af", "ag", "ai", "al", "ao", "aq", "as", "at", "au", "aw", "ax", "az", "ba",
    "bb", "bd", "be", "bf", "bg", "bh", "bi", "bj", "bm", "bn", "bo", "br", "bs", "bt", "bw",
    "by", "bz", "ca", "cd", "cf", "cg", "ch", "ci", "ck", "cl", "cm", "cn", "co", "cr", "cu",
    "cv", "cw", "cx", "cy", "cz", "de", "dj", "dk", "dm", "do", "dz", "ec", "ee", "eg", "er",
    "es", "et", "eu", "fi", "fj", "fk", "fm", "fo", "fr", "ga", "gd", "ge", "gf", "gg", "gh",
    "gi", "gl", "gm", "gn", "gp", "gq", "gr", "gs", "gt", "gu", "gw", "gy", "hk", "hm", "hn",
    "hr", "ht", "hu", "id", "ie", "il", "im", "io", "iq", "ir", "is", "it", "je", "jm", "jo",
    "jp", "ke", "kg", "kh", "ki", "km", "kn", "kp", "kr", "kw", "ky", "kz", "lb", "lc", "li",
    "lk", "lr", "ls", "lt", "lu", "lv", "ly", "ma", "mc", "me", "mg", "mh", "mm", "mn", "mo",
    "mp", "mq", "mr", "ms", "mt", "mu", "mv", "mw", "mx", "my", "mz", "na", "nc", "ne", "nf",
    "ng", "ni", "nl", "no", "np", "nr", "nu", "nz", "om", "pa", "pe", "pf", "pg", "ph", "pk",
    "pn", "pr", "pt", "pw", "qa", "re", "ro", "ru", "rw", "sa", "sb", "sc", "sd", "se", "sg",
    "si", "sk", "sl", "sm", "sn", "sr", "ss", "st", "su", "sx", "sy", "sz", "tc", "td", "tf",
    "tg", "th", "tj", "tk", "tl", "tm", "tn", "to", "tr", "tt", "tv", "tw", "tz", "ua", "ug",
    "uk", "us", "uy", "uz", "va", "vc", "ve", "vg", "vi", "vn", "vu", "wf", "ws", "ye", "yt",
    "za", "zm", "zw",
];

/// The last labels that name a file, not a host, though some are also top-level domains (`rs` is
/// Serbia, `md` Moldova, `sh` Saint Helena, `py` Paraguay, `so` Somalia): a bare dotted name that
/// ends in one is a file name. These are the extensions of the sources, manifests, locks, text
/// and objects of this kind of repository.
#[rustfmt::skip]
pub const FILE_SUFFIXES: &[&str] = &[
    "rs", "md", "sh", "toml", "yml", "yaml", "json", "lock", "txt", "py", "c", "h", "o", "a",
    "so", "cc", "pl", "ml", "mk", "pm", "sv", "ps", "in", "am", "ac", "ar", "la",
];

/// True when `label` is a listed top-level domain.
pub fn is_tld(label: &str) -> bool {
    GENERIC_TLDS.contains(&label) || COUNTRY_TLDS.contains(&label)
}
