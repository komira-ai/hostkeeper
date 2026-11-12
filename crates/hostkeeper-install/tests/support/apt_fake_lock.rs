//! The fake's answers for the host's package manager state, which no step's own check explains:
//! the dpkg lock held for a number of tries of a verb, and an interrupted dpkg, each in the words
//! of the tool that meets it (dpkg's own for the purge, apt's for apt-get and apt-mark).

use hostkeeper_install::repository::ToolOutput;

use super::{State, out};

/// What a tool prints when the lock is held for one more of the tries of `verb`; `None` once
/// those are spent. dpkg says it its own way, apt and apt-mark theirs.
pub fn busy(s: &mut State, verb: &'static str) -> Option<ToolOutput> {
    let apt = verb != "purge";
    let left = s.faults.busy.get_mut(verb).filter(|left| **left > 0)?;
    *left -= 1;
    Some(match apt {
        true => out(
            100,
            "",
            "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4321 \
             (unattended-upgr)\nE: Unable to acquire the dpkg frontend lock \
             (/var/lib/dpkg/lock-frontend), is another process using it?\n",
        ),
        false => out(
            2,
            "",
            "dpkg: error: dpkg frontend lock was locked by another process with pid 4321\n",
        ),
    })
}

/// What a tool prints when the lock is held for one more try of `verb`, else when dpkg was
/// interrupted; `None` when neither.
pub fn busy_or_interrupted(s: &mut State, verb: &'static str) -> Option<ToolOutput> {
    let apt = verb != "purge";
    if let Some(output) = busy(s, verb) {
        return Some(output);
    }
    s.faults.interrupted.then(|| {
        let text = "dpkg was interrupted, you must manually run 'dpkg --configure -a' to \
                    correct the problem.";
        match apt {
            true => out(100, "", &format!("E: {text}\n")),
            false => out(2, "", &format!("dpkg: error: {text}\n")),
        }
    })
}
