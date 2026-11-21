//! The scripted Mac of the tests of `update_account_make` ([`Mac`]): one directory of users, the
//! group `admin` and the group `com.apple.access_ssh`, which its `dscl`, `dseditgroup` and
//! `sysadminctl` read and change as the real programs do for the calls the module makes, and a
//! record of every call (its program, its arguments and its standard input) for an exact
//! comparison. A call it is told to fail exits with a code and writes its standard input (the
//! passwords) to both its streams, or cannot be started.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_launchd::{LaunchctlError, LaunchctlOutput, StdinError};

use super::*;

/// The update account's password: a space, an `=`, a quote, a `$` and a backslash, which an
/// argument list, a shell or a `key=value` reader would treat specially.
pub(super) const PASSWORD: &str = r#"Up date=pa"ss$1\x"#;
/// The administrator's password, which no other byte of a test spells.
pub(super) const ADMIN_PASSWORD: &str = "Adm1n pass=word";
/// The update account.
pub(super) const NAME: &str = "hkupdate";
/// The administrator made at Setup Assistant.
pub(super) const ADMIN: &str = "admin";

/// `words` as owned strings.
pub(super) fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A user of the scripted Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct User {
    pub(super) uid: u32,
    pub(super) hidden: bool,
    pub(super) shell: String,
    pub(super) token: bool,
}

/// One call the scripted Mac was given: the program, its arguments and, for a call through the
/// runner with input, its standard input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Call {
    pub(super) program: &'static str,
    pub(super) args: Vec<String>,
    pub(super) input: Option<String>,
}

/// A call that fails: the program, the words its arguments must all hold, and how it fails.
#[derive(Clone, Debug)]
pub(super) struct Failing {
    pub(super) program: &'static str,
    pub(super) marks: Vec<String>,
    pub(super) how: Fails,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Fails {
    /// It exits with this code, its input on both its streams.
    Exit(i32),
    /// It cannot be started.
    Unrunnable,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub(super) users: BTreeMap<String, User>,
    pub(super) admins: Vec<String>,
    /// The members of `com.apple.access_ssh`, or `None` when the group does not exist.
    pub(super) ssh: Option<Vec<String>>,
    pub(super) calls: Vec<Call>,
    pub(super) failing: Vec<Failing>,
    /// `sysadminctl` exits 0 and does nothing.
    pub(super) inert: bool,
    /// What `sysadminctl -secureTokenStatus` writes instead of its answer.
    pub(super) token_text: Option<String>,
}

/// The scripted Mac. Clones share one directory.
#[derive(Clone, Debug, Default)]
pub(super) struct Mac(Arc<Mutex<State>>);

pub(super) fn ok(stdout: &str, stderr: &str) -> LaunchctlOutput {
    LaunchctlOutput {
        code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

pub(super) fn exit(code: i32, stderr: &str) -> LaunchctlOutput {
    LaunchctlOutput {
        code: Some(code),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// What `dscl` writes for a record that is not there.
pub(super) const NOT_FOUND: &str = "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)\n";

impl Mac {
    /// A Mac with its Setup Assistant administrator, holding a token, and no ssh access group.
    pub(super) fn new() -> Mac {
        let mac = Mac::default();
        mac.add(ADMIN, 501, false, "/bin/zsh", true);
        mac
    }

    pub(super) fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A user `name`, an administrator.
    pub(super) fn add(&self, name: &str, uid: u32, hidden: bool, shell: &str, token: bool) {
        let mut state = self.state();
        state.users.insert(
            name.to_owned(),
            User {
                uid,
                hidden,
                shell: shell.to_owned(),
                token,
            },
        );
        state.admins.push(name.to_owned());
    }

    /// The update account as it should be: hidden, no shell, holding a token.
    pub(super) fn add_ready(&self, token: bool) {
        self.add(NAME, 502, true, NO_SHELL, token);
    }

    pub(super) fn user(&self, name: &str) -> Option<User> {
        self.state().users.get(name).cloned()
    }

    pub(super) fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    /// A call of `program` whose arguments hold every one of `marks` fails `how`.
    pub(super) fn fail(&self, program: &'static str, marks: &[&str], how: Fails) {
        self.state().failing.push(Failing {
            program,
            marks: words(marks),
            how,
        });
    }

    pub(super) fn port(&self, program: &'static str) -> Box<Tool> {
        Box::new(Tool {
            mac: self.clone(),
            program,
        })
    }

    /// The programs of [`Maker`] over this Mac.
    pub(super) fn programs(&self) -> Programs {
        Programs {
            dscl: self.port("dscl"),
            dseditgroup: self.port("dseditgroup"),
            sysadminctl: self.port("sysadminctl"),
            sysadminctl_input: self.port("sysadminctl"),
        }
    }

    /// The answer of `program` to `args` with `input`.
    fn answer(
        &self,
        program: &'static str,
        args: &[String],
        input: Option<&[u8]>,
    ) -> Result<LaunchctlOutput, Fails> {
        let mut state = self.state();
        state.calls.push(Call {
            program,
            args: args.to_vec(),
            input: input.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        });
        let failing = state.failing.iter().find(|failing| {
            failing.program == program && failing.marks.iter().all(|mark| args.contains(mark))
        });
        if let Some(failing) = failing {
            return match failing.how {
                Fails::Exit(code) => {
                    let echoed = input.unwrap_or_default().to_vec();
                    Ok(LaunchctlOutput {
                        code: Some(code),
                        stdout: echoed.clone(),
                        stderr: echoed,
                    })
                }
                Fails::Unrunnable => Err(Fails::Unrunnable),
            };
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match (program, &args[..]) {
            ("dscl", [".", "-list", _, "UniqueID"]) => {
                let lines: String = state
                    .users
                    .iter()
                    .map(|(name, user)| format!("{name} {}\n", user.uid))
                    .collect();
                ok(&lines, "")
            }
            ("dscl", [".", "-read", "/Groups/admin", "GroupMembership"]) => ok(
                &format!("GroupMembership: {}\n", state.admins.join(" ")),
                "",
            ),
            ("dscl", [".", "-read", _, "GroupMembership"]) => match &state.ssh {
                Some(members) => ok(&format!("GroupMembership: {}\n", members.join(" ")), ""),
                None => exit(RECORD_NOT_FOUND, NOT_FOUND),
            },
            ("dscl", [".", "-read", user, keys @ ..]) => {
                let name = user.strip_prefix(&format!("{USERS}/")).unwrap_or_default();
                let Some(found) = state.users.get(name) else {
                    return Ok(exit(RECORD_NOT_FOUND, NOT_FOUND));
                };
                let lines: String = keys
                    .iter()
                    .map(|key| match *key {
                        "RecordName" => format!("RecordName: {name}\n"),
                        "IsHidden" if found.hidden => "IsHidden: 1\n".to_owned(),
                        "UserShell" => format!("UserShell: {}\n", found.shell),
                        "NFSHomeDirectory" => format!("NFSHomeDirectory: {USERS}/{name}\n"),
                        "PrimaryGroupID" => "PrimaryGroupID: 20\n".to_owned(),
                        other => format!("No such key: {other}\n"),
                    })
                    .collect();
                ok(&lines, "")
            }
            ("dscl", [".", "-create", user, key, value]) => {
                let name = user.strip_prefix(&format!("{USERS}/")).unwrap_or_default();
                let found = state.users.get_mut(name).expect("a user");
                if *key == "IsHidden" {
                    found.hidden = *value == "1";
                } else {
                    found.shell = (*value).to_owned();
                }
                ok("", "")
            }
            ("dseditgroup", ["-o", "edit", "-d", name, ..]) => {
                let name = *name;
                state
                    .ssh
                    .iter_mut()
                    .for_each(|members| members.retain(|member| member.as_str() != name));
                ok("", "")
            }
            ("sysadminctl", ["-secureTokenStatus", name]) => {
                let token = state.users.get(*name).is_some_and(|user| user.token);
                let word = if token { "ENABLED" } else { "DISABLED" };
                let said = format!(
                    "2026-11-20 10:00:00.000 sysadminctl[512:4096] Secure token is {word} for user {name}\n"
                );
                ok("", state.token_text.as_deref().unwrap_or(&said))
            }
            ("sysadminctl", ["-addUser", name, "-password", "-", "-admin"]) => {
                if !state.inert {
                    let name = (*name).to_owned();
                    let uid = 502;
                    state.users.insert(
                        name.clone(),
                        User {
                            uid,
                            hidden: false,
                            shell: "/bin/zsh".to_owned(),
                            token: false,
                        },
                    );
                    state.admins.push(name);
                }
                ok("", "Creating new user record\n")
            }
            // The one call left, `sysadminctl -secureTokenOn <name> ...`, whose arguments every test
            // compares whole.
            _ => {
                let name = args.get(1).copied().unwrap_or_default();
                let inert = state.inert;
                if let Some(user) = state.users.get_mut(name).filter(|_| !inert) {
                    user.token = true;
                }
                ok("", "Done!\n")
            }
        })
    }
}

/// One program of the scripted Mac, as either port.
pub(super) struct Tool {
    mac: Mac,
    program: &'static str,
}

impl Launchctl for Tool {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.mac
            .answer(self.program, args, None)
            .map_err(|_| LaunchctlError::Spawn {
                args: args.join(" "),
                why: "No such file or directory (os error 2)".to_owned(),
            })
    }
}

impl StdinRun for Tool {
    fn run(
        &self,
        step: &str,
        args: &[String],
        input: &[u8],
    ) -> Result<LaunchctlOutput, StdinError> {
        self.mac
            .answer(self.program, args, Some(input))
            .map_err(|_| StdinError::Spawn {
                step: step.to_owned(),
                why: "No such file or directory (os error 2)".to_owned(),
            })
    }
}
