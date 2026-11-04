//! What is left of the declaration loader: [`Declared`], a declaration with the hook account the
//! loader resolved and the host's declared names, and [`recheck_targets`], the reserved-path rules
//! the step runner repeats before every launch, with the deny list they apply.
//!
//! B3 rewrites launch.rs and deletes this. The loader of `/etc/updater.d`, the account checks,
//! the record-directory check and the conflicts between declarations are gone: the daemon config
//! and the package manifest (`hostkeeper-config`) replace the declaration.
//!
//! The deny list, on what each entry points to when it exists:
//!
//! - no entry of any kind **names** a reserved directory or **contains** one (is an ancestor of
//!   one): `/etc` names, `/` and `/var/lib` contain;
//! - a `write_paths` or `state_dir` entry is not **inside** a reserved directory either, except
//!   under [`HOOKS_ROOT`], where the updater keeps the record directories of the hooks;
//! - a `write_paths` entry must also be below one of [`WRITE_ROOTS`];
//! - for a hook that runs as root, a write path is at or below a directory of a write root named
//!   for its service, no directory above a read or write path can be swapped by an account other
//!   than root, and no write path is in the directory of another declared service.

use std::collections::BTreeSet;
use std::path::Path;

use crate::declaration::{Declaration, Refusal};
/// Whether `path` is `dir` or lies below it, on whole components (`hostkeeper-config`'s).
pub use crate::names::is_within;

/// The directories no entry may name or contain. B3 rewrites launch.rs and deletes this.
pub const RESERVED: &[&str] = &[
    "/",
    "/bin",
    "/boot",
    "/etc",
    "/etc/polkit-1",
    "/etc/sudoers.d",
    "/etc/systemd/system",
    "/etc/updater.d",
    "/lib",
    "/lib/systemd/system",
    "/lib64",
    "/root",
    "/run/systemd/generator",
    "/run/systemd/generator.early",
    "/run/systemd/generator.late",
    "/run/systemd/network",
    "/run/systemd/system",
    "/run/systemd/system.control",
    "/run/systemd/transient",
    "/run/tmpfiles.d",
    "/run/udev",
    "/run/updater",
    "/sbin",
    "/usr",
    "/usr/lib/systemd/system",
    "/usr/share/polkit-1",
    "/var/cache/apt",
    "/var/lib/apt",
    "/var/lib/cloud",
    "/var/lib/dpkg",
    "/var/lib/polkit-1",
    "/var/lib/sudo",
    "/var/lib/updater",
    "/var/spool/cron",
];

/// The trees where nothing is written. B3 rewrites launch.rs and deletes this.
pub const WRITE_FORBIDDEN: [&str; 3] = ["/dev", "/proc", "/sys"];

/// The one place inside the reserved directories where a hook may write: the record directories
/// of the hooks. B3 rewrites launch.rs and deletes this.
pub const HOOKS_ROOT: &str = "/var/lib/updater/hooks";

/// The directories a hook may write below, one component down and anything under that. B3
/// rewrites launch.rs and deletes this.
pub const WRITE_ROOTS: [&str; 4] = ["/var/lib", "/var/cache", "/run", "/opt"];

/// What an entry is for. B3 rewrites launch.rs and deletes this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
    State,
}

/// Why an entry was refused. B3 rewrites launch.rs and deletes this.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    #[error("names the reserved directory {0}")]
    Names(&'static str),
    #[error("contains the reserved directory {0}")]
    Contains(&'static str),
    #[error("is inside the reserved directory {0}, where nothing may be written")]
    Inside(&'static str),
    #[error(
        "is not below a directory a hook may write in (one component under /var/lib, /var/cache, /run or /opt)"
    )]
    NotAllowed,
    #[error(
        "is not at or below a directory named for the service, which a hook that runs as root needs"
    )]
    NotTheServices,
}

/// Whether a hook that runs as root may write `path` for `service`: at or below a directory of a
/// write root whose name is the service's, or the service's followed by `-` and more. B3
/// rewrites launch.rs and deletes this.
pub fn check_service_write(path: &str, service: &str) -> Result<(), Violation> {
    let own = first_below_a_write_root(path).is_some_and(|first| belongs_to(first, service));
    if own {
        Ok(())
    } else {
        Err(Violation::NotTheServices)
    }
}

/// The first component of `path` under the write root it lies in: `svc` for `/var/lib/svc/x`.
fn first_below_a_write_root(path: &str) -> Option<&str> {
    WRITE_ROOTS.iter().find_map(|root| {
        let rest = path.strip_prefix(root)?.strip_prefix('/')?;
        rest.split('/').next()
    })
}

/// Whether the directory name `first` is `service`'s: the name, or the name followed by `-` and
/// more.
fn belongs_to(first: &str, service: &str) -> bool {
    (first == service)
        | first
            .strip_prefix(service)
            .is_some_and(|more| more.starts_with('-'))
}

/// Checks one entry against the deny list. `path` is in the path grammar. B3 rewrites launch.rs
/// and deletes this.
pub fn check(kind: Kind, path: &str) -> Result<(), Violation> {
    if let Some(reserved) = RESERVED.iter().find(|r| **r == path) {
        return Err(Violation::Names(reserved));
    }
    if let Some(reserved) = RESERVED.iter().find(|r| is_within(r, path)) {
        return Err(Violation::Contains(reserved));
    }
    let writes = kind != Kind::Read;
    let in_hooks_root = path != HOOKS_ROOT && is_within(path, HOOKS_ROOT);
    // `/` is every path's ancestor, which the rule on the inside is not about.
    let inside = RESERVED.iter().skip(1).find(|r| is_within(path, r));
    let forbidden = WRITE_FORBIDDEN.iter().find(|dir| is_within(path, dir));
    match (inside, forbidden) {
        (Some(reserved), _) if writes && !in_hooks_root => Err(Violation::Inside(reserved)),
        (_, Some(dir)) if writes => Err(Violation::Inside(dir)),
        // A write path is an allow-list: one component under a write root, and anything below.
        _ if kind == Kind::Write
            && !WRITE_ROOTS
                .iter()
                .any(|root| (path != *root) & is_within(path, root)) =>
        {
            Err(Violation::NotAllowed)
        }
        _ => Ok(()),
    }
}

/// A declaration with what a launch needs besides it: the uid of the hook account and the names
/// of the host's declared services. B3 rewrites launch.rs and deletes this.
#[derive(Clone, Debug)]
pub struct Declared {
    pub(crate) declaration: Declaration,
    pub(crate) hook_uid: u32,
    pub(crate) names: BTreeSet<String>,
}

impl Declared {
    /// The declaration. B3 rewrites launch.rs and deletes this.
    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// The uid of the hook's account. B3 rewrites launch.rs and deletes this.
    pub fn hook_uid(&self) -> u32 {
        self.hook_uid
    }

    /// The names of every declared service of the host, this one included. B3 rewrites
    /// launch.rs and deletes this.
    pub fn names(&self) -> &BTreeSet<String> {
        &self.names
    }

    /// A declaration made by hand, for tests: the hook's account has the uid `hook_uid` and the
    /// host declares these `names`. Only with the `test-support` feature, which this crate's own
    /// tests turn on.
    #[cfg(feature = "test-support")]
    pub fn by_hand(declaration: Declaration, hook_uid: u32, names: &[&str]) -> Declared {
        Declared {
            declaration,
            hook_uid,
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// [`recheck_targets`] for this declaration, with its own hook uid and names. B3 rewrites
    /// launch.rs and deletes this.
    pub fn recheck(&self) -> Result<(), Refusal> {
        recheck_targets(&self.declaration, self.hook_uid, &self.names)
    }
}

/// The reserved-path rules on what each entry points to now, and, for a hook that runs as root,
/// the rules on its own write paths and on the directories above its paths. Each entry is
/// judged as written first (the root rules), then on what it points to when it exists (the deny
/// list, then the root rules again).
/// What the rules read of the host's file system: where a path leads once its links are
/// followed, and the owner and mode of a directory. A launch reads the real file system
/// ([`Host::real`]); the unit tests at the end of this file give a fixed one, so the rules of a
/// root hook are judged without depending on the directories of the machine that runs them.
struct Host<'a> {
    canonical: &'a dyn Fn(&str) -> Option<String>,
    owner_mode: &'a dyn Fn(&Path) -> Option<(u32, u32)>,
}

fn real_canonical(path: &str) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

fn real_owner_mode(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.uid(), meta.mode()))
}

impl Host<'static> {
    fn real() -> Host<'static> {
        Host {
            canonical: &real_canonical,
            owner_mode: &real_owner_mode,
        }
    }
}

fn check_targets(
    declaration: &Declaration,
    root_hook: bool,
    host: &Host<'_>,
) -> Result<(), Refusal> {
    let hook = &declaration.hook;
    let entries = [(Kind::State, "state_dir", &declaration.state_dir)]
        .into_iter()
        .chain(
            hook.read_paths
                .iter()
                .map(|p| (Kind::Read, "hook.read_paths", p)),
        )
        .chain(
            hook.write_paths
                .iter()
                .map(|p| (Kind::Write, "hook.write_paths", p)),
        );
    let refusal = entries.into_iter().find_map(|(kind, field, entry)| {
        // A hook that runs as root follows links as root: nothing above a path it reads or
        // writes may be swapped by anyone else.
        let guarded = root_hook & (kind != Kind::State);
        let own = |path: &str| {
            (root_hook & (kind == Kind::Write))
                .then(|| check_service_write(path, &declaration.name).err())
                .flatten()
                .map(|why| Refusal::Reserved {
                    field,
                    path: path.to_owned(),
                    why,
                })
        };
        let swappable = |path: &str| {
            guarded
                .then(|| swappable_above(path, host))
                .flatten()
                .map(|ancestor| Refusal::Ancestor {
                    field,
                    path: path.to_owned(),
                    ancestor,
                })
        };
        let target = (host.canonical)(entry);
        let reserved = target.as_deref().and_then(|target| {
            check(kind, target).err().map(|why| Refusal::Reserved {
                field,
                path: format!("{entry} (which is {target})"),
                why,
            })
        });
        own(entry)
            .or_else(|| swappable(entry))
            .or(reserved)
            .or_else(|| {
                target
                    .as_deref()
                    .and_then(|target| own(target).or_else(|| swappable(target)))
            })
    });
    refusal.map_or(Ok(()), Err)
}

/// The nearest directory above `path` that an account other than root owns or can write (the
/// group or others): the one that could replace `path` with a link. Directories that do not
/// exist yet are skipped.
fn swappable_above(path: &str, host: &Host<'_>) -> Option<String> {
    Path::new(path).ancestors().skip(1).find_map(|above| {
        let (uid, mode) = (host.owner_mode)(above)?;
        ((uid != 0) | (mode & 0o022 != 0)).then(|| above.display().to_string())
    })
}

/// The reserved-path rules on what each entry points to now, for the step runner to call before
/// every launch: a link can be re-pointed after the load. B3 rewrites launch.rs and deletes this.
pub(crate) fn recheck_targets(
    declaration: &Declaration,
    hook_uid: u32,
    names: &BTreeSet<String>,
) -> Result<(), Refusal> {
    recheck_on(&Host::real(), declaration, hook_uid, names)
}

/// [`recheck_targets`] on the file system `host` shows.
fn recheck_on(
    host: &Host<'_>,
    declaration: &Declaration,
    hook_uid: u32,
    names: &BTreeSet<String>,
) -> Result<(), Refusal> {
    check_targets(declaration, hook_uid == 0, host)?;
    trespass_among(declaration, hook_uid == 0, names, host)
        .map_or(Ok(()), |(what, with)| Err(Refusal::Conflict { what, with }))
}

/// What a root hook and the owner of a directory it writes in fight over.
const TRESPASS: &str = "a write path in the directory of another service";

/// A write path of a hook that runs as root in the directory of another declared service, as
/// written or once its links are followed: the first component under the write root belongs to
/// the declared name among `names` it begins with, the longest one. Returns what the two fight
/// over and the owner.
fn trespass_among(
    declaration: &Declaration,
    root_hook: bool,
    names: &BTreeSet<String>,
    host: &Host<'_>,
) -> Option<(&'static str, String)> {
    let paths = root_hook.then_some(&declaration.hook.write_paths)?;
    paths.iter().find_map(|path| {
        [Some(path.clone()), (host.canonical)(path)]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                let first = first_below_a_write_root(&candidate)?;
                let owner = names
                    .iter()
                    .map(String::as_str)
                    .filter(|name| belongs_to(first, name))
                    .max_by_key(|name| name.len())?;
                (owner != declaration.name).then(|| (TRESPASS, owner.to_owned()))
            })
    })
}

#[cfg(test)]
mod tests {
    //! The rules of a root hook on a fixed file system: what the test names exists, root-owned and
    //! closed unless it says otherwise, and nothing else exists or is a link. B3 rewrites
    //! launch.rs and deletes this.

    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use super::{Host, Violation, recheck_on};
    use crate::declaration::{Declaration, Refusal};

    /// A declaration of `tierstore` whose hook writes `write` and reads nothing.
    fn writing(write: &[&str]) -> Declaration {
        let quoted: Vec<String> = write.iter().map(|path| format!("\"{path}\"")).collect();
        Declaration::parse(&format!(
            "name = \"tierstore\"\nbinary = \"tierstore\"\nstate_dir = \"/var/lib/updater/hooks/tierstore\"\n\
             [hook]\nuser = \"root\"\ngroup = \"root\"\nwrite_paths = [{}]\n\
             [hook.timeout_secs]\nprepare = 1\ncutover = 1\nverify = 1\nrollback = 1\ncleanup = 1\n",
            quoted.join(", ")
        ))
        .expect("a declaration")
    }

    /// The recheck of `declaration` for a hook of uid `uid` among `names`, on a file system where
    /// `/`, `/var` and `/var/lib` are root's and closed, `dirs` adds directories with their owner
    /// and mode, and `links` maps a path to where it leads.
    fn recheck(
        declaration: &Declaration,
        uid: u32,
        names: &[&str],
        dirs: &[(&str, u32, u32)],
        links: &[(&str, &str)],
    ) -> Result<(), Refusal> {
        let mut known: BTreeMap<String, (u32, u32)> = ["/", "/var", "/var/lib"]
            .into_iter()
            .map(|dir| (dir.to_owned(), (0, 0o40755)))
            .collect();
        known.extend(
            dirs.iter()
                .map(|(dir, owner, mode)| ((*dir).to_owned(), (*owner, *mode))),
        );
        let links: BTreeMap<String, String> = links
            .iter()
            .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
            .collect();
        let canonical = |path: &str| links.get(path).cloned();
        let owner_mode = |path: &Path| known.get(path.to_str()?).copied();
        let host = Host {
            canonical: &canonical,
            owner_mode: &owner_mode,
        };
        let names: BTreeSet<String> = names.iter().map(|name| (*name).to_owned()).collect();
        recheck_on(&host, declaration, uid, &names)
    }

    /// Catches the root rule on write paths not being applied at the recheck: a hook that runs
    /// as root may write only below a directory named for its service, whether or not the path
    /// exists; a hook that is not root is not held to it.
    #[test]
    fn a_root_hook_writes_only_in_its_own_directory() {
        let other = writing(&["/var/lib/other/x"]);
        assert_eq!(
            recheck(&other, 0, &["tierstore"], &[], &[]),
            Err(Refusal::Reserved {
                field: "hook.write_paths",
                path: "/var/lib/other/x".to_owned(),
                why: Violation::NotTheServices,
            })
        );
        assert_eq!(recheck(&other, 65534, &["tierstore"], &[], &[]), Ok(()));
        let own = writing(&["/var/lib/tierstore/x", "/var/lib/tierstore-state"]);
        assert_eq!(recheck(&own, 0, &["tierstore"], &[], &[]), Ok(()));
    }

    /// Catches a root hook's path below a directory another account controls: the service's own
    /// directory owned by its service account, or one the group can write, is refused; root's
    /// and closed is accepted.
    #[test]
    fn a_root_hook_path_below_a_directory_root_does_not_control_is_refused() {
        let own = writing(&["/var/lib/tierstore/x"]);
        for (owner, mode) in [(990, 0o40755), (0, 0o40775), (0, 0o40757)] {
            assert_eq!(
                recheck(
                    &own,
                    0,
                    &["tierstore"],
                    &[("/var/lib/tierstore", owner, mode)],
                    &[]
                ),
                Err(Refusal::Ancestor {
                    field: "hook.write_paths",
                    path: "/var/lib/tierstore/x".to_owned(),
                    ancestor: "/var/lib/tierstore".to_owned(),
                }),
                "{owner} {mode:o}"
            );
        }
        let closed = recheck(
            &own,
            0,
            &["tierstore"],
            &[("/var/lib/tierstore", 0, 0o40755)],
            &[],
        );
        assert_eq!(closed, Ok(()));
    }

    /// Catches a link that leads a root hook's write path out of its own directory: the path as
    /// written is the service's, what it leads to is not.
    #[test]
    fn a_root_hook_write_path_that_leads_elsewhere_is_refused_where_it_leads() {
        let linked = writing(&["/var/lib/tierstore/link"]);
        assert_eq!(
            recheck(
                &linked,
                0,
                &["tierstore"],
                &[],
                &[("/var/lib/tierstore/link", "/var/lib/other/y")]
            ),
            Err(Refusal::Reserved {
                field: "hook.write_paths",
                path: "/var/lib/other/y".to_owned(),
                why: Violation::NotTheServices,
            })
        );
        assert_eq!(
            recheck(
                &linked,
                0,
                &["tierstore"],
                &[],
                &[("/var/lib/tierstore/link", "/etc")]
            ),
            Err(Refusal::Reserved {
                field: "hook.write_paths",
                path: "/var/lib/tierstore/link (which is /etc)".to_owned(),
                why: Violation::Names("/etc"),
            })
        );
    }

    /// Catches the dash rule handing a root hook another declared service's directory, as
    /// written or through a link: `tierstore` may not write `/var/lib/tierstore-b/x` when
    /// `tierstore-b` is a service of the host, and may when it is not.
    #[test]
    fn a_root_hook_may_not_write_in_the_directory_of_another_declared_service() {
        let conflict = Err(Refusal::Conflict {
            what: "a write path in the directory of another service",
            with: "tierstore-b".to_owned(),
        });
        let both = ["tierstore", "tierstore-b"];
        let direct = writing(&["/var/lib/tierstore-b/x"]);
        assert_eq!(recheck(&direct, 0, &both, &[], &[]), conflict);
        assert_eq!(recheck(&direct, 0, &["tierstore"], &[], &[]), Ok(()));
        assert_eq!(recheck(&direct, 65534, &both, &[], &[]), Ok(()));
        let linked = writing(&["/var/lib/tierstore-x"]);
        let links = [("/var/lib/tierstore-x", "/var/lib/tierstore-b/y")];
        assert_eq!(recheck(&linked, 0, &both, &[], &links), conflict);
    }
}
