//! A service whose `identityAccount` changed: the files of the old account are not trusted and
//! are replaced.

use std::os::unix::fs::MetadataExt;

use hostkeeper_enroll::{FilesError, IdentityDir, IdentityFile, IdentityFiles, Owner, Wanted};

use crate::support::{SERVICE, World, me};

/// The identity directory for a test that is not root, which cannot give a file to another user:
/// what it is asked to write for `account` it writes for the test's own user. A read is the real
/// one, so the files of the test's user are another user's to a reader that expects `account`.
struct Reowned {
    dir: IdentityDir,
    account: Owner,
}

impl Reowned {
    fn own(&self, owner: Owner) -> Owner {
        if owner == self.account { me() } else { owner }
    }
}

impl IdentityFiles for Reowned {
    fn prepare(&self, service: &str, owner: Owner) -> Result<(), FilesError> {
        self.dir.prepare(service, self.own(owner))
    }

    fn read(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
    ) -> Result<Option<Vec<u8>>, FilesError> {
        self.dir.read(service, file, owner)
    }

    fn write(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        self.dir.write(service, file, self.own(owner), bytes)
    }

    fn remove(&self, service: &str) -> Result<(), FilesError> {
        self.dir.remove(service)
    }

    fn services(&self) -> Result<Vec<String>, FilesError> {
        self.dir.services()
    }
}

/// Proves a service whose account changed is not stuck on the files of the old account: its key
/// and certificate, owned by the old user, are not read, a new key is made and a new certificate
/// is asked for and installed at mode 0600 in the service's directory, the old key's bytes are
/// gone, and the service reports no error.
///
/// Catches: a read that fails on the old owner, which left the service retrying forever.
#[test]
fn a_service_whose_account_changed_gets_a_new_key_and_certificate() {
    let mut world = World::new("account-changed");
    world.keeper.step(&[Wanted {
        account: Ok(me()),
        ..crate::support::wanted(SERVICE)
    }]);
    let old_key = world.read(SERVICE, IdentityFile::Key).expect("a key");
    let old_cert = world.read(SERVICE, IdentityFile::Cert).expect("a cert");

    let account = Owner {
        uid: me().uid + 1,
        gid: me().gid,
    };
    let mut keeper = world.keeper_over(Box::new(Reowned {
        dir: IdentityDir::owned_by(&world.root(), me()),
        account,
    }));
    keeper.step(&[Wanted {
        account: Ok(account),
        ..crate::support::wanted(SERVICE)
    }]);

    assert_eq!(world.report(SERVICE).1, None);
    assert_eq!(world.issuer.calls().len(), 2);
    let new_key = world.read(SERVICE, IdentityFile::Key).expect("a new key");
    let new_cert = world.read(SERVICE, IdentityFile::Cert).expect("a new cert");
    assert_ne!(new_key, old_key);
    assert_ne!(new_cert, old_cert);
    for file in [IdentityFile::Key, IdentityFile::Cert, IdentityFile::Ca] {
        let meta = std::fs::metadata(world.path(SERVICE, file)).expect("installed");
        assert_eq!(meta.mode() & 0o7777, 0o600, "{}", file.name());
    }
    let dir = std::fs::metadata(world.root().join(SERVICE)).expect("the directory");
    assert_eq!((dir.mode() & 0o7777, dir.gid()), (0o750, me().gid));
    let leftovers = std::fs::read_dir(world.root().join(SERVICE))
        .expect("the directory")
        .count();
    assert_eq!(leftovers, 3);
}
