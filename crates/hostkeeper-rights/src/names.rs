//! The grammars of the declaration that is left: account names, environment variables,
//! capabilities and address families. B3 rewrites launch.rs and deletes this.
//!
//! Every value that reaches a systemd property is checked here first. systemd reads a property
//! value as a list split on white space, with `-`, `+` and `!` in front of a path meaning
//! "ignore a missing path", "ignore the sandbox for this path" and "invert", with `%` for a
//! specifier and `\` for an escape. A grammar that allows none of those leaves nothing for a
//! value to smuggle in. The service name and path grammars are `hostkeeper-config`'s.

use std::fmt;

pub use hostkeeper_config::names::{
    MAX_PATH, PathError, check_path, check_relative, is_service_name, is_within,
};

/// The most bytes of the value of an environment variable.
/// B3 rewrites launch.rs and deletes this.
pub const MAX_ENV_VALUE: usize = 256;

/// Whether `name` is an account name: a lowercase letter or `_` first, then lowercase letters,
/// digits, `_` and `-`, at most 32 bytes.
/// B3 rewrites launch.rs and deletes this.
pub fn is_account_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
    first
        & (name.len() <= 32)
        & bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Whether `name` can be an environment variable of a hook: `[A-Za-z_][A-Za-z0-9_]*`.
/// B3 rewrites launch.rs and deletes this.
pub fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    first & bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The variable names that change what a program loads or how it resolves names: the dynamic
/// loader's (`LD_*`), the character set and locale search paths, the allocator's (`MALLOC_*`) and
/// the resolver's.
/// B3 rewrites launch.rs and deletes this.
pub const HIJACKING_ENV: &[&str] = &[
    "GCONV_PATH",
    "LOCPATH",
    "NLSPATH",
    "HOSTALIASES",
    "RES_OPTIONS",
    // What a shell, an interpreter or a runtime started by the hook would read before its script.
    "BASH_ENV",
    "ENV",
    "IFS",
    "CDPATH",
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "PROMPT_COMMAND",
    "PERL5LIB",
    "PERL5OPT",
    "PERLLIB",
    "PERL5DB",
    "RUBYLIB",
    "RUBYOPT",
    "NODE_OPTIONS",
    "NODE_PATH",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "CLASSPATH",
    "LUA_PATH",
    "LUA_CPATH",
    "TCLLIBPATH",
    "PHPRC",
    "PHP_INI_SCAN_DIR",
    "GEM_PATH",
    "GEM_HOME",
    // What the C library, TLS libraries, git and the shell read at start.
    "GLIBC_TUNABLES",
    "OPENSSL_CONF",
    "OPENSSL_ENGINES",
    "OPENSSL_MODULES",
    "SSLKEYLOGFILE",
    "ZDOTDIR",
];

/// Whether a hook may be given the variable `name` with `value`: not a name of [`HIJACKING_ENV`],
/// not `LD_*`, `MALLOC_*`, `PYTHON*`, `DYLD_*` or `GIT_*`, and `PATH` only as a list of absolute directories.
/// B3 rewrites launch.rs and deletes this.
pub fn env_allowed(name: &str, value: &str) -> bool {
    let hijack = ["LD_", "MALLOC_", "PYTHON", "DYLD_", "GIT_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        | HIJACKING_ENV.contains(&name);
    let path_ok = (name != "PATH") | value.split(':').all(|dir| check_path(dir).is_ok());
    !hijack & path_ok
}

/// Whether `value` can be an environment value: letters, digits and `. _ / : @ , + = -`, at
/// most [`MAX_ENV_VALUE`] bytes. No white space, quote, `$`, `%` or `\`: systemd would read the
/// property as several words or expand it.
/// B3 rewrites launch.rs and deletes this.
pub fn is_env_value(value: &str) -> bool {
    (value.len() <= MAX_ENV_VALUE)
        & value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:@,+=-".contains(&b))
}

/// Whether `text` is `prefix` followed by one or more of `A-Z`, `0-9` and `_`.
fn is_upper_name(prefix: &str, text: &str) -> bool {
    text.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            & rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

macro_rules! upper_name {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(text: String) -> Result<$name, String> {
                if is_upper_name($prefix, &text) {
                    Ok($name(text))
                } else {
                    Err(format!(
                        "{text:?} is not {} followed by capital letters, digits and `_`",
                        $prefix
                    ))
                }
            }
        }

        impl From<$name> for String {
            fn from(name: $name) -> String {
                name.0
            }
        }

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

upper_name!(
    /// A capability name, as systemd spells it (`CAP_NET_ADMIN`).
    Capability,
    "CAP_"
);
upper_name!(
    /// An address family name, as systemd spells it (`AF_UNIX`).
    Family,
    "AF_"
);
