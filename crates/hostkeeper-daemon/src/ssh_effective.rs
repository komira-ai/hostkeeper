//! What sshd's effective configuration says (`sshd -T -C user=...`), and the judgments the
//! lockout-safety rule makes of it (docs/design/ssh-through-coordinator.md section 6, rules 3 and
//! 8).
//!
//! `sshd -T` prints one lowercase keyword and its value per line, a keyword with several values
//! on several lines (`allowusers`, `port`) or one line with the values separated by spaces
//! (`authenticationmethods`). [`Effective::parse`] keeps each line's words under its keyword. The
//! functions here only read that: they judge, they change nothing.

use std::collections::BTreeMap;

/// The settings that decide whether an account can log in, which the daemon records before it
/// changes anything and holds against what is there after (rule 3).
pub const AUTH_KEYWORDS: [&str; 10] = [
    "pubkeyauthentication",
    "authenticationmethods",
    "permitrootlogin",
    "allowusers",
    "allowgroups",
    "denyusers",
    "denygroups",
    "passwordauthentication",
    "kbdinteractiveauthentication",
    "usepam",
];

/// The two keywords the drop-in sets only when passwords are turned off (rule 5).
pub const PASSWORD_KEYWORDS: [&str; 2] = ["passwordauthentication", "kbdinteractiveauthentication"];

/// What sshd would do for one login, as `sshd -T` printed it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effective {
    lines: BTreeMap<String, Vec<Vec<String>>>,
}

/// The paths the drop-in names, which the effective configuration must carry for the
/// certificate login to be ours.
#[derive(Clone, Copy, Debug)]
pub struct Ours<'a> {
    pub ca_keys: &'a str,
    /// The principals file with its `%u`, as the drop-in writes it.
    pub principals: &'a str,
    /// The same path with the login user in place of `%u`.
    pub principals_expanded: &'a str,
    pub revoked: &'a str,
}

/// Why a login is not possible by certificate, as the `detail` of the report: the keyword that
/// fails (one of the three trust keywords when the stock configuration shadows the drop-in's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing(pub &'static str);

impl Failing {
    /// Whether the failing keyword is one the drop-in sets: the stock file wins over it.
    #[must_use]
    pub fn is_shadowed(&self) -> bool {
        matches!(
            self.0,
            "trustedusercakeys" | "authorizedprincipalsfile" | "revokedkeys"
        )
    }
}

impl Effective {
    /// The effective configuration `text` says; a line with a keyword and no value is kept with
    /// no words.
    #[must_use]
    pub fn parse(text: &str) -> Effective {
        let mut lines: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            if let Some(keyword) = words.next() {
                lines
                    .entry(keyword.to_ascii_lowercase())
                    .or_default()
                    .push(words.map(str::to_owned).collect());
            }
        }
        Effective { lines }
    }

    /// Every value word of `keyword`, over all its lines, in order.
    #[must_use]
    pub fn words(&self, keyword: &str) -> Vec<&str> {
        self.lines
            .get(keyword)
            .into_iter()
            .flatten()
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// The first word of `keyword`, if it has one.
    #[must_use]
    pub fn first(&self, keyword: &str) -> Option<&str> {
        self.words(keyword).into_iter().next()
    }

    /// The auth-relevant settings, as recorded for the comparison of rule 3.
    #[must_use]
    pub fn auth_settings(&self) -> BTreeMap<&'static str, Vec<Vec<String>>> {
        AUTH_KEYWORDS
            .iter()
            .map(|&keyword| (keyword, self.lines.get(keyword).cloned().unwrap_or_default()))
            .collect()
    }

    /// The address and port a connection to this sshd goes to: the first `listenaddress` (an
    /// address that is every address is read as the loopback), else the first `port` on the
    /// loopback, else 22.
    #[must_use]
    pub fn listening(&self) -> (String, u16) {
        let port = self
            .first("port")
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(22);
        let Some(listen) = self.first("listenaddress") else {
            return ("127.0.0.1".to_owned(), port);
        };
        let (host, listen_port) = split_listen(listen);
        let host = match host {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "::1",
            other => other,
        };
        (host.to_owned(), listen_port.unwrap_or(port))
    }

    /// The first keyword of the allow and deny lists that keeps `user`, a member of `groups`,
    /// from logging in; none when the lists let the user in.
    #[must_use]
    pub fn excluded(&self, user: &str, groups: &[String]) -> Option<&'static str> {
        let names = |keyword: &str, subject: &str| {
            self.words(keyword)
                .into_iter()
                .any(|pattern| matches_name(pattern, subject))
        };
        let listed = |keyword: &str| !self.words(keyword).is_empty();
        let in_group = |keyword: &str| groups.iter().any(|group| names(keyword, group));
        if names("denyusers", user) {
            Some("denyusers")
        } else if listed("allowusers") && !names("allowusers", user) {
            Some("allowusers")
        } else if in_group("denygroups") {
            Some("denygroups")
        } else if listed("allowgroups") && !in_group("allowgroups") {
            Some("allowgroups")
        } else {
            None
        }
    }

    /// Whether a certificate alone logs `user` in under this configuration (rule 3's first
    /// set), the failing keyword if not; and with `ours`, whether the trust files are the
    /// drop-in's.
    ///
    /// # Errors
    /// The keyword that fails.
    pub fn cert_login(&self, user: &str, ours: Option<&Ours<'_>>) -> Result<(), Failing> {
        if self.first("pubkeyauthentication") != Some("yes") {
            return Err(Failing("pubkeyauthentication"));
        }
        let methods = self.words("authenticationmethods");
        if !methods.is_empty() && !methods.iter().any(|m| matches!(*m, "any" | "publickey")) {
            return Err(Failing("authenticationmethods"));
        }
        if user == "root"
            && !matches!(
                self.first("permitrootlogin"),
                Some("yes" | "prohibit-password" | "without-password")
            )
        {
            return Err(Failing("permitrootlogin"));
        }
        if let Some(ours) = ours {
            let carried = |keyword: &'static str, wanted: [&str; 2]| {
                let words = self.words(keyword);
                if words.len() == 1 && wanted.contains(&words[0]) {
                    Ok(())
                } else {
                    Err(Failing(keyword))
                }
            };
            carried("trustedusercakeys", [ours.ca_keys, ours.ca_keys])?;
            carried(
                "authorizedprincipalsfile",
                [ours.principals, ours.principals_expanded],
            )?;
            carried("revokedkeys", [ours.revoked, ours.revoked])?;
        }
        Ok(())
    }
}

/// An address and the port a `listenaddress` value names: `host`, `host:port`, `[v6]:port`.
fn split_listen(value: &str) -> (&str, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (rest, None),
        };
    }
    match value.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
        _ => (value, None),
    }
}

/// Whether the AllowUsers-style `pattern` names `subject`: `*` and `?` match, and a `user@host`
/// pattern names the user part (the address part cannot be judged, so such a pattern is taken as
/// naming the user).
fn matches_name(pattern: &str, subject: &str) -> bool {
    let user = pattern.split_once('@').map_or(pattern, |(user, _)| user);
    glob(user.as_bytes(), subject.as_bytes())
}

/// `*` and `?` matching of `text` against `pattern`.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| glob(rest, &text[skip..])),
        Some((b'?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some((byte, rest)) => text.first() == Some(byte) && glob(rest, &text[1..]),
    }
}

#[cfg(test)]
#[path = "ssh_effective_tests.rs"]
mod tests;
