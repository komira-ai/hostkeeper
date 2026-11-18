//! The tests of `host_identity`: the identity copy written under a scratch directory, owned by the
//! test's own user, with a group file in the scratch directory and tools that make groups as
//! scripted programs. No real pki directory, `/var/lib`, group file or group tool is touched.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use hostkeeper_launchd::LaunchctlOutput;
use hostkeeper_link::testing::{FakeCerts, RecordingLog, identity};
use hostkeeper_link::{CertState, ClientIdentity, PkiCertSource};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::identity_group::IDENTITY_GROUP;
use crate::identity_group::tests::{MakesGroup, scratch};

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn my_gid() -> u32 {
    rustix::process::getegid().as_raw()
}

fn pair(cert: &str, key: &str) -> CertState {
    CertState::Enrolled(ClientIdentity::new(
        cert.as_bytes().to_vec(),
        key.as_bytes().to_vec(),
    ))
}

/// What a test drives: the copy, its certificate source, its log and the tool that makes groups.
struct Rig {
    scratch: Scratch,
    certs: Arc<FakeCerts>,
    log: RecordingLog,
    tool: MakesGroup,
    copy: HostIdentity,
}

impl Rig {
    /// A host whose group file has the group `hostkeeper-identity` with the test's own group's gid.
    fn with_group() -> Rig {
        Rig::new("hi", &format!("{IDENTITY_GROUP}:x:{}:\n", my_gid()))
    }

    fn new(name: &str, group_text: &str) -> Rig {
        let scratch = scratch(name);
        let file = scratch.path().join("group");
        fs::write(&file, group_text).expect("the group file");
        let tool = MakesGroup::making(&file, &format!("{IDENTITY_GROUP}:x:{}:", my_gid()));
        Rig::over(scratch, file, tool)
    }

    fn over(scratch: Scratch, file: PathBuf, tool: MakesGroup) -> Rig {
        let certs = Arc::new(FakeCerts::new(pair("certificate one", "key one")));
        let group = IdentityGroup::in_group_file(
            IDENTITY_GROUP,
            file,
            Box::new(tool.clone()),
            vec!["--system".to_owned(), IDENTITY_GROUP.to_owned()],
        );
        let dir = scratch.path().join("var/lib/hostkeeper/identity");
        let copy = HostIdentity::new(certs.clone(), dir, group, me(), my_gid());
        Rig {
            scratch,
            certs,
            log: RecordingLog::default(),
            tool,
            copy,
        }
    }

    fn step(&mut self) {
        self.copy.after_step(&self.log);
    }

    fn dir(&self) -> PathBuf {
        self.scratch.path().join("var/lib/hostkeeper/identity")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir().join(name)
    }

    fn text(&self, name: &str) -> String {
        fs::read_to_string(self.file(name)).expect("the file")
    }

    fn inode(&self, name: &str) -> u64 {
        fs::metadata(self.file(name)).expect("the file").ino()
    }

    fn mode(&self, name: &str) -> u32 {
        fs::metadata(self.file(name)).expect("the file").mode() & 0o7777
    }

    /// The names in the identity directory.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.dir())
            .expect("the directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .into_string()
                    .expect("a name")
            })
            .collect();
        names.sort();
        names
    }

    fn kept(&self) -> String {
        format!("{IDENTITY_KEPT}{}", self.dir().display())
    }
}

/// Proves the first step makes the directory 0750, the key 0640 and the certificate 0644, each
/// owned by the owner and the group's gid, with the certificate and the key as the source gave
/// them, and logs one line.
#[test]
fn the_first_step_writes_the_key_and_the_certificate_for_the_group() {
    let mut rig = Rig::with_group();
    rig.step();
    assert_eq!(rig.text(CERT_FILE), "certificate one");
    assert_eq!(rig.text(KEY_FILE), "key one");
    assert_eq!(rig.names(), vec!["host.crt", "host.key"]);
    assert_eq!(rig.mode(KEY_FILE), 0o640);
    assert_eq!(rig.mode(CERT_FILE), 0o644);
    let dir = fs::metadata(rig.dir()).expect("the directory");
    assert_eq!(dir.mode() & 0o7777, 0o750);
    for meta in [
        dir,
        fs::metadata(rig.file(KEY_FILE)).expect("the key"),
        fs::metadata(rig.file(CERT_FILE)).expect("the certificate"),
    ] {
        assert_eq!((meta.uid(), meta.gid()), (me(), my_gid()));
    }
    assert_eq!(rig.log.lines(), vec![rig.kept()]);
}

/// Proves a pair that is already there as it should be is not written again (the files keep their
/// inodes, since a write renames a new file over the old one), and that no line is logged for it.
#[test]
fn a_pair_that_is_already_kept_writes_nothing_and_logs_nothing() {
    let mut rig = Rig::with_group();
    rig.step();
    let (key, cert) = (rig.inode(KEY_FILE), rig.inode(CERT_FILE));
    rig.step();
    rig.step();
    assert_eq!((rig.inode(KEY_FILE), rig.inode(CERT_FILE)), (key, cert));
    assert_eq!(rig.log.lines().len(), 1);
}

/// Proves a renewal (a new certificate for the same key) replaces `host.crt` alone.
#[test]
fn a_renewal_replaces_only_the_certificate() {
    let mut rig = Rig::with_group();
    rig.step();
    let key = rig.inode(KEY_FILE);
    let cert = rig.inode(CERT_FILE);
    rig.certs.set(pair("certificate two", "key one"));
    rig.step();
    assert_eq!(rig.text(CERT_FILE), "certificate two");
    assert_eq!(rig.text(KEY_FILE), "key one");
    assert_eq!(rig.inode(KEY_FILE), key);
    assert_ne!(rig.inode(CERT_FILE), cert);
    assert_eq!(rig.names(), vec!["host.crt", "host.key"]);
}

/// Proves a new key (an enrollment again) replaces both files.
#[test]
fn a_new_key_replaces_both_files() {
    let mut rig = Rig::with_group();
    rig.step();
    let (key, cert) = (rig.inode(KEY_FILE), rig.inode(CERT_FILE));
    rig.certs.set(pair("certificate two", "key two"));
    rig.step();
    assert_eq!(rig.text(KEY_FILE), "key two");
    assert_eq!(rig.text(CERT_FILE), "certificate two");
    assert_ne!(rig.inode(KEY_FILE), key);
    assert_ne!(rig.inode(CERT_FILE), cert);
}

/// Proves the key is written before the certificate: with the certificate's name taken by a
/// directory, so that it cannot be replaced, a new key is in place all the same, and no
/// temporary file is left.
///
/// Catches: a certificate written before its key, which would pair a new certificate with an old
/// key for a reader.
#[test]
fn the_key_is_written_before_the_certificate() {
    let mut rig = Rig::with_group();
    rig.step();
    fs::remove_file(rig.file(CERT_FILE)).expect("remove the certificate");
    fs::create_dir(rig.file(CERT_FILE)).expect("a directory in its place");
    rig.certs.set(pair("certificate two", "key two"));
    rig.step();
    assert_eq!(rig.text(KEY_FILE), "key two");
    assert_eq!(rig.names(), vec!["host.crt", "host.key"]);
    let lines = rig.log.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with(&format!(
        "{IDENTITY_NOT_KEPT}{}: ",
        rig.file(CERT_FILE).display()
    )));
}

/// Proves a key that cannot be replaced stops the step before the certificate is touched, so the
/// old pair stays whole.
#[test]
fn a_key_that_cannot_be_written_leaves_the_certificate_as_it_was() {
    let mut rig = Rig::with_group();
    rig.step();
    fs::remove_file(rig.file(KEY_FILE)).expect("remove the key");
    fs::create_dir(rig.file(KEY_FILE)).expect("a directory in its place");
    rig.certs.set(pair("certificate two", "key two"));
    rig.step();
    assert_eq!(rig.text(CERT_FILE), "certificate one");
    assert_eq!(rig.names(), vec!["host.crt", "host.key"]);
    let lines = rig.log.lines();
    assert!(lines[1].starts_with(&format!(
        "{IDENTITY_NOT_KEPT}{}: ",
        rig.file(KEY_FILE).display()
    )));
}

/// Proves a host with no current pair gets nothing (not even the directory), that what an earlier
/// enrollment left stays, and that the line is logged again when the pair comes back.
#[test]
fn a_host_with_no_pair_writes_nothing_and_keeps_what_was_there() {
    let mut rig = Rig::with_group();
    rig.certs.set(CertState::NotEnrolled {
        waiting_for: PathBuf::from("current"),
    });
    rig.step();
    assert!(!rig.dir().exists());
    assert!(rig.log.lines().is_empty());
    rig.certs.set(pair("certificate one", "key one"));
    rig.step();
    rig.certs.set(CertState::NotEnrolled {
        waiting_for: PathBuf::from("current"),
    });
    rig.step();
    assert_eq!(rig.text(KEY_FILE), "key one");
    assert_eq!(rig.text(CERT_FILE), "certificate one");
    rig.certs.set(pair("certificate one", "key one"));
    rig.step();
    assert_eq!(rig.log.lines(), vec![rig.kept(), rig.kept()]);
}

/// Proves a file whose mode was changed, a file whose bytes were changed and a directory whose
/// mode was changed are each put right by the next step.
#[test]
fn a_changed_mode_or_content_is_put_right() {
    let mut rig = Rig::with_group();
    rig.step();
    let key = rig.inode(KEY_FILE);
    fs::set_permissions(rig.file(KEY_FILE), fs::Permissions::from_mode(0o600)).expect("chmod");
    fs::write(rig.file(CERT_FILE), "tampered").expect("write");
    fs::set_permissions(rig.dir(), fs::Permissions::from_mode(0o777)).expect("chmod");
    rig.step();
    assert_eq!(rig.mode(KEY_FILE), 0o640);
    assert_ne!(rig.inode(KEY_FILE), key);
    assert_eq!(rig.text(CERT_FILE), "certificate one");
    let dir = fs::metadata(rig.dir()).expect("the directory");
    assert_eq!(dir.mode() & 0o7777, 0o750);
}

/// Proves a file that is a link to another file is replaced, not written through: the file it
/// pointed to keeps its bytes.
#[test]
fn a_link_in_the_place_of_a_file_is_replaced_and_not_followed() {
    let mut rig = Rig::with_group();
    rig.step();
    let victim = rig.scratch.path().join("victim");
    fs::write(&victim, "untouched").expect("the victim");
    fs::remove_file(rig.file(KEY_FILE)).expect("remove the key");
    symlink(&victim, rig.file(KEY_FILE)).expect("the link");
    rig.step();
    assert_eq!(
        fs::read_to_string(&victim).expect("the victim"),
        "untouched"
    );
    assert_eq!(rig.text(KEY_FILE), "key one");
    assert!(
        fs::symlink_metadata(rig.file(KEY_FILE))
            .expect("the key")
            .is_file()
    );
}

/// Proves a temporary file a crash left, whether a file or a link to another file, is removed and
/// never written through.
#[test]
fn a_leftover_temporary_file_is_removed_and_not_written_through() {
    let mut rig = Rig::with_group();
    rig.step();
    let victim = rig.scratch.path().join("victim");
    fs::write(&victim, "untouched").expect("the victim");
    let pid = std::process::id();
    fs::write(rig.file(&format!(".host.crt.{pid}.tmp")), "left").expect("a leftover");
    symlink(&victim, rig.file(&format!(".host.key.{pid}.tmp"))).expect("a leftover link");
    rig.certs.set(pair("certificate two", "key two"));
    rig.step();
    assert_eq!(
        fs::read_to_string(&victim).expect("the victim"),
        "untouched"
    );
    assert_eq!(rig.names(), vec!["host.crt", "host.key"]);
    assert_eq!(rig.text(KEY_FILE), "key two");
    assert_eq!(rig.text(CERT_FILE), "certificate two");
}

/// Proves a directory that is a link is refused and not followed: nothing is written where it
/// points, and the line names the directory.
#[test]
fn a_linked_identity_directory_is_refused_and_not_followed() {
    let mut rig = Rig::with_group();
    let elsewhere = rig.scratch.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("the target");
    fs::create_dir_all(rig.dir().parent().expect("a parent")).expect("the parent");
    symlink(&elsewhere, rig.dir()).expect("the link");
    rig.step();
    assert_eq!(fs::read_dir(&elsewhere).expect("the target").count(), 0);
    assert_eq!(
        rig.log.lines(),
        vec![format!(
            "{IDENTITY_NOT_KEPT}{} is not a directory",
            rig.dir().display()
        )]
    );
}

/// Proves a regular file in the place of the directory is refused too.
#[test]
fn a_file_in_the_place_of_the_identity_directory_is_refused() {
    let mut rig = Rig::with_group();
    fs::create_dir_all(rig.dir().parent().expect("a parent")).expect("the parent");
    fs::write(rig.dir(), "a file").expect("the file");
    rig.step();
    assert_eq!(
        rig.log.lines(),
        vec![format!(
            "{IDENTITY_NOT_KEPT}{} is not a directory",
            rig.dir().display()
        )]
    );
}

/// Proves a directory that cannot be made, since a parent of it is a file, is an error that names
/// the directory, and the step ends.
#[test]
fn a_parent_that_cannot_be_made_is_an_error() {
    let mut rig = Rig::with_group();
    let blocked = rig.scratch.path().join("blocked");
    fs::write(&blocked, "a file").expect("the file");
    rig.copy.dir = blocked.join("identity");
    rig.step();
    let lines = rig.log.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with(&format!(
        "{IDENTITY_NOT_KEPT}{}: ",
        blocked.join("identity").display()
    )));
}

/// Proves a group the host lacks is made once, by the platform's tool, and the files are then the
/// group's, for the group's modes; the next steps make nothing.
#[test]
fn a_missing_group_is_made_once_and_the_files_are_for_it() {
    let mut rig = Rig::new("hi-make", "root:x:0:\n");
    rig.step();
    rig.step();
    assert_eq!(
        rig.tool.calls(),
        vec![vec!["--system".to_owned(), IDENTITY_GROUP.to_owned()]]
    );
    assert_eq!(rig.mode(KEY_FILE), 0o640);
    assert_eq!(rig.log.lines(), vec![rig.kept()]);
}

/// Proves a group that cannot be made gives files for root alone (the directory 0700, the key
/// 0600, the certificate 0644, the fallback group), one line of why for as long as the reason is
/// the same, and the next step tries again; once the group is there the files are the group's and
/// the next line says so.
#[test]
fn a_group_that_cannot_be_made_gives_files_for_root_alone_until_it_is_there() {
    let scratch = scratch("hi-alone");
    let file = scratch.path().join("group");
    fs::write(&file, "root:x:0:\n").expect("the group file");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::exit(9, "groupadd: boom")));
    let mut rig = Rig::over(scratch, file.clone(), tool);
    rig.step();
    rig.step();
    assert_eq!(rig.tool.calls().len(), 2, "each step tries again");
    assert_eq!(rig.mode(KEY_FILE), 0o600);
    assert_eq!(rig.mode(CERT_FILE), 0o644);
    let dir = fs::metadata(rig.dir()).expect("the directory");
    assert_eq!(dir.mode() & 0o7777, 0o700);
    assert_eq!(dir.gid(), my_gid());
    let alone = format!(
        "{IDENTITY_ROOT_ONLY}making the identity group failed with exit code 9: groupadd: boom"
    );
    assert_eq!(rig.log.lines(), vec![alone.clone()]);
    fs::write(&file, format!("{IDENTITY_GROUP}:x:{}:\n", my_gid())).expect("the group");
    rig.step();
    assert_eq!(rig.mode(KEY_FILE), 0o640);
    let dir = fs::metadata(rig.dir()).expect("the directory");
    assert_eq!(dir.mode() & 0o7777, 0o750);
    assert_eq!(rig.log.lines(), vec![alone, rig.kept()]);
}

/// Proves a group file that cannot be read is a group that cannot be found: files for root alone.
#[test]
fn a_group_file_that_cannot_be_read_gives_files_for_root_alone() {
    let scratch = scratch("hi-nofile");
    let file = scratch.path().join("absent");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let mut rig = Rig::over(scratch, file.clone(), tool);
    rig.step();
    assert_eq!(rig.mode(KEY_FILE), 0o600);
    assert_eq!(
        rig.log.lines(),
        vec![format!(
            "{IDENTITY_ROOT_ONLY}cannot read the group file {}: No such file or directory (os error 2)",
            file.display()
        )]
    );
}

/// Proves the files are given the group's gid, which is not the test's own: a user that is not
/// root cannot give a file to another group, so the step fails and says so; root's files have the
/// gid.
#[test]
fn the_files_are_given_the_gid_the_group_has() {
    let mine = my_gid();
    let other = mine + 1;
    let mut rig = Rig::new("hi-gid", &format!("{IDENTITY_GROUP}:x:{other}:\n"));
    rig.step();
    if me() == 0 {
        assert_eq!(
            fs::metadata(rig.file(KEY_FILE)).expect("the key").gid(),
            other
        );
    } else {
        let lines = rig.log.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            format!(
                "{IDENTITY_NOT_KEPT}{}: Operation not permitted (os error 1)",
                rig.dir().display()
            )
        );
    }
}

/// Proves the copy is made from what the heartbeat loop's certificate source reads of a real pki
/// directory (`current` -> `gen-<n>`, the key as PKCS#8 DER): the certificate as it is in the
/// generation, the key as PKCS#8 PEM of the same key; a renewal (a second generation, the same
/// key) replaces the certificate alone.
#[test]
fn the_copy_is_made_from_the_pki_directory_s_current_generation() {
    let scratch = scratch("hi-pki");
    let pki = scratch.path().join("pki");
    let key = rcgen::KeyPair::generate().expect("a key");
    let generation = |number: u32, name: &str| {
        let params = rcgen::CertificateParams::new(vec![name.to_owned()]).expect("params");
        let cert = params.self_signed(&key).expect("a certificate").pem();
        let dir = pki.join(format!("gen-{number}"));
        fs::create_dir_all(&dir).expect("a generation");
        fs::write(dir.join("cert.pem"), &cert).expect("the certificate");
        fs::write(dir.join("key"), key.serialize_der()).expect("the key");
        let link = pki.join("current.tmp");
        let _ = fs::remove_file(&link);
        symlink(format!("gen-{number}"), &link).expect("the link");
        fs::rename(&link, pki.join("current")).expect("current");
        cert
    };
    let log: Arc<RecordingLog> = Arc::default();
    let source = Arc::new(PkiCertSource::new(&pki, log.clone()));
    let file = scratch.path().join("group");
    fs::write(&file, format!("{IDENTITY_GROUP}:x:{}:\n", my_gid())).expect("the group file");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let group = IdentityGroup::in_group_file(IDENTITY_GROUP, file, Box::new(tool), Vec::new());
    let dir = scratch.path().join("identity");
    let mut copy = HostIdentity::new(source, dir.clone(), group, me(), my_gid());
    copy.after_step(log.as_ref());
    assert!(!dir.exists(), "no current generation, nothing written");
    let first = generation(1, "one.example");
    copy.after_step(log.as_ref());
    assert_eq!(
        fs::read_to_string(dir.join(CERT_FILE)).expect("host.crt"),
        first
    );
    let written = fs::read_to_string(dir.join(KEY_FILE)).expect("host.key");
    assert_eq!(
        rcgen::KeyPair::from_pem(&written)
            .expect("a PEM key")
            .serialize_der(),
        key.serialize_der()
    );
    let (key_inode, cert_inode) = (
        fs::metadata(dir.join(KEY_FILE)).expect("host.key").ino(),
        fs::metadata(dir.join(CERT_FILE)).expect("host.crt").ino(),
    );
    let second = generation(2, "two.example");
    copy.after_step(log.as_ref());
    assert_eq!(
        fs::read_to_string(dir.join(CERT_FILE)).expect("host.crt"),
        second
    );
    assert_eq!(
        fs::metadata(dir.join(KEY_FILE)).expect("host.key").ino(),
        key_inode
    );
    assert_ne!(
        fs::metadata(dir.join(CERT_FILE)).expect("host.crt").ino(),
        cert_inode
    );
}

/// Proves the fake pairs of the other tests are what the module copies: the bytes of the pair
/// the source gives, whatever they are.
#[test]
fn the_bytes_of_the_pair_are_copied_as_they_are() {
    let scratch = scratch("hi-bytes");
    let file = scratch.path().join("group");
    fs::write(&file, format!("{IDENTITY_GROUP}:x:{}:\n", my_gid())).expect("the group file");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let mut rig = Rig::over(scratch, file, tool);
    let tagged = identity("t");
    rig.certs.set(CertState::Enrolled(tagged.clone()));
    rig.step();
    assert_eq!(
        fs::read(rig.file(CERT_FILE)).expect("host.crt"),
        tagged.certificate()
    );
    assert_eq!(
        fs::read(rig.file(KEY_FILE)).expect("host.key"),
        tagged.key()
    );
}

/// Proves a directory with no parent or no name (the root) is refused as not a directory.
#[test]
fn the_root_directory_is_not_a_place_for_the_copy() {
    let mut rig = Rig::with_group();
    rig.copy.dir = PathBuf::from("/");
    rig.step();
    assert_eq!(
        rig.log.lines(),
        vec![format!("{IDENTITY_NOT_KEPT}/ is not a directory")]
    );
}

/// Proves a temporary name taken by a directory, which cannot be removed as a file, ends the step
/// with an error that names the file being written; the old file stays.
#[test]
fn a_leftover_that_cannot_be_removed_is_an_error_and_the_old_file_stays() {
    let mut rig = Rig::with_group();
    rig.step();
    fs::create_dir(rig.file(&format!(".host.crt.{}.tmp", std::process::id())))
        .expect("a directory");
    rig.certs.set(pair("certificate two", "key one"));
    rig.step();
    assert_eq!(rig.text(CERT_FILE), "certificate one");
    let lines = rig.log.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with(&format!(
        "{IDENTITY_NOT_KEPT}{}: ",
        rig.file(CERT_FILE).display()
    )));
}

/// Runs a step of a rig whose parent directory has the mode `mode` (restored afterwards), and
/// the log. A user that is not root is refused by the mode; root is not, and makes the copy.
fn step_under_parent_mode(mode: u32) -> (Vec<String>, String) {
    let mut rig = Rig::with_group();
    let parent = rig.dir().parent().expect("a parent").to_owned();
    fs::create_dir_all(&parent).expect("the parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(mode)).expect("chmod");
    rig.step();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).expect("chmod back");
    (rig.log.lines(), rig.kept())
}

/// Proves each way the parent of the identity directory can refuse the daemon (it cannot be
/// opened, its child cannot be looked up, its child cannot be made) is an error that names the
/// directory; root is refused by none of them.
#[test]
fn a_parent_directory_that_refuses_is_an_error_naming_the_directory() {
    for (name, mode) in [("opened", 0o000), ("looked-up", 0o600), ("made", 0o500)] {
        let (lines, kept) = step_under_parent_mode(mode);
        if me() == 0 {
            assert_eq!(lines, vec![kept], "{name}");
        } else {
            assert_eq!(lines.len(), 1, "{name}");
            assert!(
                lines[0].starts_with(IDENTITY_NOT_KEPT)
                    && lines[0].ends_with("Permission denied (os error 13)"),
                "{name}: {lines:?}"
            );
        }
    }
}
