//! The update account's keys and the update-account file (docs/design/maintenance-updates.md,
//! section 5.5, "Making the account"; docs/design/mac-bootstrap.md, section 4).
//!
//! Four keys beside the farm values name the update account and the administrator that gives it
//! its Secure Token:
//!
//! | Key | What |
//! |---|---|
//! | `update_account_name` | the update account, `hkupdate` in the design |
//! | `update_account_password` | its password |
//! | `admin_name` | the administrator made at Setup Assistant, a holder of a Secure Token |
//! | `admin_password` | that administrator's password |
//!
//! They are given all four or none ([`keys`]). A source with none of them makes no file, as before
//! the keys existed; a source with some of them is refused, naming the first one missing in the
//! order of the table. Each is a string: a name is ASCII letters, digits, `_`, `.` and `-`, and
//! does not start with `-` (the rule of the daemon's reader); a password is not empty and holds no
//! line break; the two names differ (the account cannot give itself a token).
//!
//! **The file.** With the keys, the bootstrap writes [`UPDATE_ACCOUNT_FILE`] (root's, mode 0600)
//! before `installer` runs, so the daemon's first start finds it: the bytes of
//! [`UpdateAccountKeys::file_text`], the layout the daemon's reader takes
//! (`hostkeeper_daemon::update_account`):
//!
//! ```text
//! name=<update_account_name>
//! password=<update_account_password>
//! ```
//!
//! **The administrator's password is not written.** The bootstrap keeps no copy of it: the daemon
//! is to read the two `admin_` keys from the same settings source, under the same trust rule, at
//! the start that makes the account, and use them for that call alone (docs/design/mac-bootstrap.md,
//! section 4).
//!
//! **Nothing prints a password.** [`Password`] has no `Display` and its `Debug` prints
//! `Password(withheld)`; no error of the settings holds a value of these keys, only its name.

use std::fmt;
use std::path::Path;

use crate::settings::SettingsError;

/// Where a Mac keeps its update account: the path the daemon reads.
pub const UPDATE_ACCOUNT_FILE: &str = "/Library/Application Support/hostkeeper/update-account";

/// The largest update-account file the daemon reads.
pub const MAX_FILE_BYTES: usize = 4096;

/// The four keys, in the order a missing one is named.
pub const KEYS: [&str; 4] = [
    "update_account_name",
    "update_account_password",
    "admin_name",
    "admin_password",
];

/// A password of the settings. It has no `Display`, and its `Debug` prints none of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    /// The password's text, for the one place it goes.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(withheld)")
    }
}

/// An account's name and password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub password: Password,
}

/// The update account and the administrator that makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateAccountKeys {
    /// The update account, whose name and password the update-account file holds.
    pub account: Account,
    /// The administrator made at Setup Assistant; its password is not written by the bootstrap.
    pub admin: Account,
}

impl UpdateAccountKeys {
    /// The text of the update-account file: the account's name line and its password line.
    #[must_use]
    pub fn file_text(&self) -> String {
        format!(
            "name={}\npassword={}\n",
            self.account.name,
            self.account.password.expose()
        )
    }
}

/// The values of the four keys in a source, in the order of [`KEYS`].
pub(crate) type Values = [Option<toml::Value>; 4];

/// Whether `name` is an account name the daemon's reader takes.
fn account_name(name: &str) -> bool {
    let allowed = |byte: u8| byte.is_ascii_alphanumeric() | b"_.-".contains(&byte);
    !name.is_empty() & !name.starts_with('-') & name.bytes().all(allowed)
}

/// Why `text`, the value of `key`, breaks its rule; `None` when it keeps it.
fn fault(key: &str, text: &str) -> Option<&'static str> {
    match key.ends_with("_name") {
        true if !account_name(text) => Some(
            "is not an account name: ASCII letters, digits, `_`, `.` and `-`, not starting with `-`",
        ),
        true => None,
        false if text.is_empty() => Some("is empty"),
        false if text.contains('\n') => {
            Some("holds a line break, which a password line cannot hold")
        }
        false => None,
    }
}

/// The keys of `values`, read from `path` (which names it in an error): `None` when none of them
/// is there.
pub(crate) fn keys(
    path: &Path,
    values: Values,
) -> Result<Option<UpdateAccountKeys>, SettingsError> {
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    let invalid = |key: &'static str, why: &str| SettingsError::Invalid {
        path: path.to_path_buf(),
        key,
        why: why.to_owned(),
    };
    let mut texts: [String; 4] = Default::default();
    for ((key, value), slot) in KEYS.into_iter().zip(values).zip(&mut texts) {
        *slot = match value {
            None => {
                return Err(SettingsError::Incomplete {
                    path: path.to_path_buf(),
                    key,
                });
            }
            Some(toml::Value::String(text)) => text,
            Some(_) => return Err(invalid(key, "is not a string")),
        };
        if let Some(why) = fault(key, slot.as_str()) {
            return Err(invalid(key, why));
        }
    }
    let [name, password, admin_name, admin_password] = texts;
    if admin_name == name {
        return Err(invalid(
            "admin_name",
            "is the update account's own name: the account is given its Secure Token by another administrator",
        ));
    }
    let keys = UpdateAccountKeys {
        account: Account {
            name,
            password: Password(password),
        },
        admin: Account {
            name: admin_name,
            password: Password(admin_password),
        },
    };
    if keys.file_text().len() > MAX_FILE_BYTES {
        return Err(invalid(
            "update_account_password",
            "makes, with the name, an update-account file larger than 4096 bytes",
        ));
    }
    Ok(Some(keys))
}
