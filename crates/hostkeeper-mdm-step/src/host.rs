//! The host a plan is made against and carried out on.

use std::io;

use crate::RUNUSER;

/// What kind of thing a path is. A link is not followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// Anything else, a symbolic link included.
    Other,
}

/// What `stat` says of a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// The kind.
    pub kind: Kind,
    /// The permission bits, `0o7777` at most.
    pub mode: u32,
    /// The owning user.
    pub uid: u32,
    /// The owning group.
    pub gid: u32,
    /// The size in bytes.
    pub size: u64,
}

/// A program and its arguments. The program is an absolute path; there is no shell and no
/// environment, so nothing a name holds is interpreted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    /// The program.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

impl Cmd {
    /// A command running `program`.
    pub fn new(program: &str) -> Cmd {
        Cmd {
            program: program.to_owned(),
            args: Vec::new(),
        }
    }

    /// With one more argument.
    #[must_use]
    pub fn arg(mut self, arg: &str) -> Cmd {
        self.args.push(arg.to_owned());
        self
    }

    /// With these more arguments.
    #[must_use]
    pub fn args<S: AsRef<str>>(mut self, args: &[S]) -> Cmd {
        self.args.extend(args.iter().map(|a| a.as_ref().to_owned()));
        self
    }

    /// The same command run as `user` (`runuser -u USER -- PROGRAM ARGS`).
    #[must_use]
    pub fn as_user(self, user: &str) -> Cmd {
        Cmd::new(RUNUSER)
            .arg("-u")
            .arg(user)
            .arg("--")
            .arg(&self.program)
            .args(&self.args)
    }

    /// The program and its arguments as one line, for a plan.
    pub fn line(&self) -> String {
        let mut words = vec![self.program.as_str()];
        words.extend(self.args.iter().map(String::as_str));
        words.join(" ")
    }

    /// The program and the arguments, as `argv`.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.clone()];
        argv.extend(self.args.clone());
        argv
    }
}

/// What a program printed and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The exit status; -1 when a signal ended the program.
    pub code: i32,
    /// What it wrote to standard output.
    pub stdout: String,
    /// What it wrote to standard error.
    pub stderr: String,
}

/// A user trusted on the way to a path, besides the users a host trusts everywhere and the
/// owner a step asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trustee {
    /// The user with this id.
    Uid(u32),
    /// The user of this name, by the id it has when a path is walked: nobody while it does not
    /// exist, and the user a plan makes once the plan has made it.
    User(String),
}

/// What is on the way to a path, as [`execute`](crate::execute) checks a plan against it before
/// the plan's first step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Way {
    /// The root's name in a refusal.
    pub root: String,
    /// The owner of the root, then of each directory on the way to the path (not the path
    /// itself) that is there, in order, up to the first that is not.
    pub owners: Vec<u32>,
    /// The users trusted on the way to the path, besides the owner a step asks for: those
    /// trusted everywhere and those granted a directory at or above it. A [`Trustee::User`]
    /// that exists is given as its [`Trustee::Uid`].
    pub trusted: Vec<Trustee>,
}

/// The machine: files, programs and the effective user. Paths are absolute.
pub trait Host {
    /// The effective user id.
    fn euid(&self) -> u32;

    /// The effective group id.
    fn egid(&self) -> u32;

    /// `path` with every symbolic link on the way resolved. The part of the path that does
    /// not exist yet is kept as given.
    ///
    /// # Errors
    /// The system's error, other than "not found"; too many links.
    fn canonical(&self, path: &str) -> io::Result<String>;

    /// `lstat` of `path` (a link on the way is refused, a link at the end is reported as
    /// such); `None` when there is nothing there.
    ///
    /// # Errors
    /// The system's error, other than "not found".
    fn stat(&self, path: &str) -> io::Result<Option<Meta>>;

    /// The bytes of the file at `path`; `None` when there is nothing there. The real host
    /// follows no link: not one on the way, and not the file itself.
    ///
    /// # Errors
    /// The system's error, other than "not found" (a permission error included).
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>>;

    /// Like [`Host::read`], but a file longer than `limit` bytes is an error of kind
    /// `FileTooLarge`; no more than `limit + 1` bytes are read. The real host opens the file
    /// once and reads that descriptor, so a swapped name cannot slip past the limit.
    ///
    /// # Errors
    /// As [`Host::read`]; the file is too large.
    fn read_limited(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>>;

    /// The names in the directory at `path`, sorted, without `.` and `..`; `None` when nothing
    /// is there. The real host follows no link: not one on the way, and not the directory
    /// itself.
    ///
    /// # Errors
    /// The system's error, other than "not found"; `path` is not a directory; a name is not
    /// UTF-8.
    fn list(&self, path: &str) -> io::Result<Option<Vec<String>>>;

    /// Writes `bytes` to `path` with exactly `mode`, owned by `owner` (uid, gid), replacing
    /// any file there in one step.
    ///
    /// # Errors
    /// The system's error.
    fn write(&self, path: &str, mode: u32, owner: (u32, u32), bytes: &[u8]) -> io::Result<()>;

    /// Makes the directory `path` (and any parent that is missing, as root's 0755) with
    /// exactly `mode`, owned by `owner`; one that is there is brought to them.
    ///
    /// # Errors
    /// The system's error.
    fn mkdir(&self, path: &str, mode: u32, owner: (u32, u32)) -> io::Result<()>;

    /// Removes the file at `path`; nothing there is not an error.
    ///
    /// # Errors
    /// The system's error, other than "not found".
    fn remove(&self, path: &str) -> io::Result<()>;

    /// Runs `cmd` to its end.
    ///
    /// # Errors
    /// The program could not be started (a program that ran and failed is an [`Output`]).
    fn run(&self, cmd: &Cmd) -> io::Result<Output>;

    /// What is on the way to `path`, walked without following a link and without judging an
    /// owner; `None` for a host that judges no owner (the plain fake).
    ///
    /// # Errors
    /// As every walk: `path` names no file or has a `.` or `..` component; a link or a file is
    /// on the way; the system's error, other than "not found"; a user lookup could not be
    /// started.
    fn way(&self, path: &str) -> io::Result<Option<Way>>;
}
