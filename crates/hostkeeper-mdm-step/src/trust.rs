//! Who may own the directories on the way to a path: what the real host and the strict fake
//! judge alike, and the refusals they give.

use std::io;

use crate::{Output, Trustee};

/// A grant: the components of a directory, and the user trusted on the way to it and below it.
pub(crate) type Grant = (Vec<String>, Trustee);

/// The grant of `dir` to `who`. A `.` or `..` component is kept, so the grant matches no path:
/// every operation refuses such a path.
pub(crate) fn grant(dir: &str, who: Trustee) -> Grant {
    let names = dir
        .split('/')
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect();
    (names, who)
}

/// The users `grants` trust on the way to the path of components `names`: those of every grant
/// of a directory at or above it. The grant is by component, so `/var/lib/svc` grants nothing
/// on `/var/lib/svc2`.
pub(crate) fn granted<'g>(
    grants: &'g [Grant],
    names: &'g [&str],
) -> impl Iterator<Item = &'g Trustee> + 'g {
    grants
        .iter()
        .filter(move |(dir, _)| {
            dir.len() <= names.len() && dir.iter().zip(names).all(|(a, b)| a == b)
        })
        .map(|(_, who)| who)
}

/// The users trusted on the way to the path of components `names`: `everywhere`, then the
/// granted ones, each user that exists by its id (`uid_of` looks a name up). A user that does
/// not exist yet stays a name: a plan that makes it is judged against the name.
pub(crate) fn resolve(
    everywhere: &[u32],
    grants: &[Grant],
    names: &[&str],
    uid_of: impl Fn(&str) -> io::Result<Option<u32>>,
) -> io::Result<Vec<Trustee>> {
    let mut trusted: Vec<Trustee> = everywhere.iter().copied().map(Trustee::Uid).collect();
    for who in granted(grants, names) {
        trusted.push(match who {
            Trustee::Uid(uid) => Trustee::Uid(*uid),
            Trustee::User(name) => uid_of(name)?.map_or_else(|| who.clone(), Trustee::Uid),
        });
    }
    Ok(trusted)
}

/// The ids among `trusted`, and `owner`: who may own a directory on the way to a path that an
/// operation asking for `owner` changes, now.
pub(crate) fn uids(trusted: &[Trustee], owner: Option<u32>) -> Vec<u32> {
    trusted
        .iter()
        .filter_map(|who| match who {
            Trustee::Uid(uid) => Some(*uid),
            Trustee::User(_) => None,
        })
        .chain(owner)
        .collect()
}

/// The id in the third field of a `getent passwd` or `getent group` answer (`name:x:id:...`),
/// when the lookup succeeded.
pub(crate) fn id_field(found: &Output) -> Option<u32> {
    let field = found.stdout.trim().split(':').nth(2);
    field
        .and_then(|f| f.parse().ok())
        .filter(|_| found.code == 0)
}

/// The components of `path`, refusing `.` and `..`.
pub(crate) fn components(path: &str) -> io::Result<Vec<&str>> {
    let names: Vec<&str> = path.split('/').filter(|n| !n.is_empty()).collect();
    match names.iter().any(|n| *n == "." || *n == "..") {
        true => Err(refused(path, "has a . or .. component")),
        false => Ok(names),
    }
}

/// The refusal of the directory `label`, owned by `uid`, on the way to `path`.
pub(crate) fn untrusted(path: &str, label: &str, uid: u32) -> io::Error {
    refused_at(
        path,
        &format!("{label} is owned by uid {uid}, which is not trusted for this path"),
    )
}

/// A refusal that names the path and then what is wrong, as `refused: PATH: WHAT`.
pub(crate) fn refused_at(path: &str, what: &str) -> io::Error {
    io::Error::other(format!("refused: {path}: {what}"))
}

/// A refusal of the path itself, as `refused: PATH WHY`.
pub(crate) fn refused(path: &str, why: &str) -> io::Error {
    io::Error::other(format!("refused: {path} {why}"))
}
