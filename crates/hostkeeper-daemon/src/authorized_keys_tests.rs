//! The tests of `authorized_keys`: the managed block as text, and the file in a scratch home owned
//! by the test's own user, never the real `~/.ssh` or `/root`.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use hostkeeper_testkit::Scratch;

use super::*;
use crate::key_fixture_tests::key;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn my_group() -> u32 {
    rustix::process::getegid().as_raw()
}

/// An account whose home is a new directory in `scratch`, as the test's own user and group.
fn account(scratch: &Scratch) -> Account {
    let home = scratch.path().join("home");
    fs::create_dir(&home).expect("the home");
    Account {
        name: "ops".to_owned(),
        uid: me(),
        gid: my_group(),
        home,
    }
}

fn ssh(account: &Account) -> PathBuf {
    account.home.join(".ssh")
}

fn file(account: &Account) -> PathBuf {
    ssh(account).join("authorized_keys")
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("the metadata").mode() & 0o7777
}

fn keys(fills: &[u8]) -> Vec<String> {
    fills.iter().map(|fill| key(*fill, "ops laptop")).collect()
}

/// The text of a file holding the block of `keys`.
fn block(keys: &[String]) -> String {
    let mut text = format!("{BEGIN}\n");
    for key in keys {
        text.push_str(&format!("{key}\n"));
    }
    text.push_str(&format!("{END}\n"));
    text
}

/// [`merged`] over the text of a file of valid UTF-8.
fn merge(current: &str, keys: &[String]) -> Result<String, BlockError> {
    merged(current.as_bytes(), keys).map(|bytes| String::from_utf8(bytes).expect("text"))
}

/// [`block_keys`] over the text of a file of valid UTF-8.
fn keys_of(current: &str) -> Result<Vec<String>, BlockError> {
    block_keys(current.as_bytes())
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Proves a file with no block gets the block at its end, after a newline when the text did not
/// end with one, and every line before it is kept byte for byte.
#[test]
fn a_file_with_no_block_gets_the_block_at_its_end() {
    let set = keys(&[1, 2]);
    assert_eq!(merge("", &set), Ok(block(&set)));
    assert_eq!(
        merge("# hand\nssh-rsa AAAA hand\n", &set),
        Ok(format!("# hand\nssh-rsa AAAA hand\n{}", block(&set)))
    );
    assert_eq!(
        merge("ssh-rsa AAAA no newline", &set),
        Ok(format!("ssh-rsa AAAA no newline\n{}", block(&set)))
    );
}

/// Proves the block is replaced by exactly the keys given, and the lines before and after it,
/// a line with a carriage return and a last line with no newline among them, are kept byte for
/// byte and in place.
#[test]
fn the_block_is_replaced_and_every_other_line_is_kept_in_place() {
    let before = "# before\r\nssh-rsa BEFORE  with  spaces\n\n";
    let after = "ssh-rsa AFTER\r\n# no newline at the end";
    let current = format!("{before}{}{after}", block(&keys(&[1, 2, 3])));
    let set = keys(&[4, 2]);
    assert_eq!(
        merge(&current, &set),
        Ok(format!("{before}{}{after}", block(&set)))
    );
}

/// Proves the empty set takes the block's lines away and nothing else, and a text with no block is
/// given back as it is.
#[test]
fn the_empty_set_removes_the_block_and_nothing_else() {
    let current = format!("# before\n{}# after\n", block(&keys(&[1])));
    assert_eq!(merge(&current, &[]), Ok("# before\n# after\n".to_owned()));
    assert_eq!(
        merge("# only hand keys", &[]),
        Ok("# only hand keys".to_owned())
    );
    assert_eq!(merge("", &[]), Ok(String::new()));
}

/// Proves the markers are whole lines compared without a trailing carriage return, and a line that
/// only starts with a marker's text is not one.
#[test]
fn a_marker_is_a_whole_line_without_its_carriage_return() {
    let current = format!("{BEGIN}\r\nssh-rsa OLD\r\n{END}\r\nafter\r\n");
    assert_eq!(
        merge(&current, &keys(&[1])),
        Ok(format!("{}after\r\n", block(&keys(&[1]))))
    );
    let lookalike = format!("{BEGIN} and more\n{END} and more\n");
    assert_eq!(
        merge(&lookalike, &keys(&[1])),
        Ok(format!("{lookalike}{}", block(&keys(&[1]))))
    );
    let no_newline = format!("{BEGIN}\nold\n{END}");
    assert_eq!(merge(&no_newline, &keys(&[1])), Ok(block(&keys(&[1]))));
}

/// Proves markers that do not make one block are refused by the rule they break, with the line:
/// a begin with no end, an end with no begin (before any block, or after a whole one), and a second
/// begin (inside an open block, or after a whole one).
#[test]
fn markers_that_do_not_make_one_block_are_refused() {
    let set = keys(&[1]);
    let cases = [
        (format!("a\n{BEGIN}\nb\n"), BlockError::NoEnd { begin: 2 }),
        (
            format!("a\n{END}\n"),
            BlockError::EndWithoutBegin { line: 2 },
        ),
        (
            format!("{BEGIN}\n{END}\n{END}\n"),
            BlockError::EndWithoutBegin { line: 3 },
        ),
        (
            format!("{BEGIN}\nx\n{BEGIN}\n{END}\n"),
            BlockError::SecondBegin { line: 3 },
        ),
        (
            format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n"),
            BlockError::SecondBegin { line: 3 },
        ),
    ];
    for (text, why) in cases {
        assert_eq!(merge(&text, &set), Err(why), "{text:?}");
        assert_eq!(merge(&text, &[]), Err(why), "{text:?}");
        assert_eq!(keys_of(&text), Err(why), "{text:?}");
    }
    assert_eq!(
        BlockError::NoEnd { begin: 2 }.to_string(),
        "the block that begins at line 2 has no end line"
    );
    assert_eq!(
        BlockError::EndWithoutBegin { line: 3 }.to_string(),
        "line 3 ends a block that was not begun"
    );
    assert_eq!(
        BlockError::SecondBegin { line: 3 }.to_string(),
        "line 3 begins a second block"
    );
}

/// Proves the keys of a block are read as written, none when there is no block, and a carriage
/// return at the end of a key line stays part of it, so a block written so is not the set sent.
#[test]
fn the_keys_of_a_block_are_read_as_written() {
    let set = keys(&[1, 2]);
    assert_eq!(keys_of(&format!("hand\n{}", block(&set))), Ok(set.clone()));
    assert_eq!(keys_of("hand\n"), Ok(Vec::new()));
    assert_eq!(keys_of(&format!("{BEGIN}\n{END}\n")), Ok(Vec::new()));
    let crlf = format!("{BEGIN}\r\n{}\r\n{END}\r\n", set[0]);
    assert_eq!(keys_of(&crlf), Ok(vec![format!("{}\r", set[0])]));
}

/// Proves a new file is made with its directory: `.ssh` mode 0700 and `authorized_keys` mode 0600,
/// owned by the account, holding the block alone, and no temporary file is left.
#[test]
fn keys_are_written_into_a_new_file_and_a_new_directory() {
    let scratch = scratch("ak-new");
    let account = account(&scratch);
    let set = keys(&[1, 2]);
    assert_eq!(put(&account, &set).expect("put"), Put::Written);
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        block(&set)
    );
    assert_eq!(mode(&ssh(&account)), 0o700);
    assert_eq!(mode(&file(&account)), 0o600);
    let meta = fs::metadata(file(&account)).expect("meta");
    assert_eq!((meta.uid(), meta.gid()), (me(), my_group()));
    assert_eq!(names(&ssh(&account)), ["authorized_keys"]);
}

/// Proves a key added by hand before the block and another after it are kept byte for byte when
/// the block changes, and a key dropped from the set is dropped from the block.
#[test]
fn hand_keys_outside_the_block_are_kept_and_a_removed_key_is_removed() {
    let scratch = scratch("ak-hand");
    let account = account(&scratch);
    let before = "ssh-rsa BEFORE hand key\n";
    let after = "ssh-rsa AFTER hand key\n";
    fs::create_dir(ssh(&account)).expect("ssh");
    fs::write(
        file(&account),
        format!("{before}{}{after}", block(&keys(&[1, 2]))),
    )
    .expect("file");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o600)).expect("mode");
    fs::set_permissions(ssh(&account), fs::Permissions::from_mode(0o700)).expect("mode");
    let set = keys(&[2]);
    assert_eq!(put(&account, &set).expect("put"), Put::Written);
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        format!("{before}{}{after}", block(&set))
    );
}

/// Proves the empty set takes the block away and keeps the hand keys, and with no file (and no
/// `.ssh`) it makes none.
#[test]
fn the_empty_set_removes_the_block_and_makes_no_file() {
    let scratch = scratch("ak-empty");
    let account = account(&scratch);
    assert_eq!(put(&account, &[]).expect("put"), Put::Unchanged);
    assert!(!ssh(&account).exists(), "no .ssh made for nothing");

    fs::create_dir(ssh(&account)).expect("ssh");
    assert_eq!(put(&account, &[]).expect("put"), Put::Unchanged);
    assert!(!file(&account).exists(), "no file made for nothing");

    assert_eq!(put(&account, &keys(&[1])).expect("put"), Put::Written);
    let mut text = fs::read_to_string(file(&account)).expect("file");
    text.push_str("ssh-rsa AFTER hand key\n");
    fs::write(file(&account), text).expect("append");
    assert_eq!(put(&account, &[]).expect("put"), Put::Written);
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        "ssh-rsa AFTER hand key\n"
    );
}

/// Proves a file that holds the block already, owned by the account with mode 0600, is not
/// written: the same inode, and `Unchanged`.
#[test]
fn an_unchanged_set_leaves_the_file_as_it_is() {
    let scratch = scratch("ak-same");
    let account = account(&scratch);
    let set = keys(&[1, 2]);
    assert_eq!(put(&account, &set).expect("put"), Put::Written);
    let inode = fs::metadata(file(&account)).expect("meta").ino();
    assert_eq!(put(&account, &set).expect("put"), Put::Unchanged);
    assert_eq!(fs::metadata(file(&account)).expect("meta").ino(), inode);
}

/// Proves a file holding the block with a mode open to the group or others is written again,
/// mode 0600, though its text is right.
#[test]
fn a_file_with_a_loose_mode_is_rewritten_with_mode_0600() {
    let scratch = scratch("ak-mode");
    let account = account(&scratch);
    let set = keys(&[1]);
    put(&account, &set).expect("put");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o644)).expect("mode");
    assert_eq!(put(&account, &set).expect("put"), Put::Written);
    assert_eq!(mode(&file(&account)), 0o600);
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        block(&set)
    );
}

/// Proves a `.ssh` that the group or others may write has those bits cleared, and its other bits
/// are kept.
#[test]
fn a_writable_ssh_directory_has_its_group_and_other_write_bits_cleared() {
    let scratch = scratch("ak-dir-mode");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    fs::set_permissions(ssh(&account), fs::Permissions::from_mode(0o777)).expect("mode");
    put(&account, &keys(&[1])).expect("put");
    assert_eq!(mode(&ssh(&account)), 0o755);
    put(&account, &keys(&[2])).expect("put");
    assert_eq!(mode(&ssh(&account)), 0o755, "a mode already closed is left");
}

/// Proves a `.ssh` that is a link, even to a directory the account owns, is refused and nothing
/// is written through it.
#[test]
fn a_symlinked_ssh_directory_is_refused() {
    let scratch = scratch("ak-ssh-link");
    let account = account(&scratch);
    let outside = scratch.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    symlink(&outside, ssh(&account)).expect("link");
    let error = put(&account, &keys(&[1])).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("{} is not a directory", ssh(&account).display())
    );
    assert!(names(&outside).is_empty());
    assert!(!held(&account, &operator_keys::digest(&[])));
}

/// Proves an `authorized_keys` that is a link is refused and the file it points at is not written;
/// so is a directory at its path.
#[test]
fn a_file_that_is_a_link_or_a_directory_is_refused() {
    let scratch = scratch("ak-file-link");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    let outside = scratch.path().join("outside");
    fs::write(&outside, "elsewhere\n").expect("outside");
    symlink(&outside, file(&account)).expect("link");
    let error = put(&account, &keys(&[1])).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("{} is not a regular file", file(&account).display())
    );
    assert_eq!(
        fs::read_to_string(&outside).expect("outside"),
        "elsewhere\n"
    );
    assert!(!held(&account, &operator_keys::digest(&[])));

    fs::remove_file(file(&account)).expect("unlink");
    fs::create_dir(file(&account)).expect("a directory");
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::NotAFile { .. })
    ));
}

/// Proves a `.ssh` another user owns is refused, never taken over, and nothing is written in it.
#[test]
fn a_ssh_directory_of_another_account_is_refused() {
    let scratch = scratch("ak-other-owner");
    let mut account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    account.uid = me() + 1;
    let error = put(&account, &keys(&[1])).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{} is owned by user {}, not by the login account's user {}",
            ssh(&account).display(),
            me(),
            me() + 1
        )
    );
    assert!(names(&ssh(&account)).is_empty());
    assert!(!held(&account, &operator_keys::digest(&[])));
}

/// Proves a file whose markers do not make one block is refused with the file byte-identical and
/// no temporary file left, and the error names the file and the line, never a key.
#[test]
fn a_malformed_block_is_refused_and_the_file_is_left_as_it_is() {
    let scratch = scratch("ak-malformed");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    let text = format!("ssh-rsa HAND\n{BEGIN}\n{}\n", key(9, "ops laptop"));
    fs::write(file(&account), &text).expect("file");
    let error = put(&account, &keys(&[1])).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!(
            "{}: the block that begins at line 2 has no end line",
            file(&account).display()
        )
    );
    assert_eq!(fs::read_to_string(file(&account)).expect("file"), text);
    assert_eq!(names(&ssh(&account)), ["authorized_keys"]);
    assert!(!held(&account, &operator_keys::digest(&[])));
}

/// Proves a home that is missing, or is a file, is refused.
#[test]
fn a_home_that_is_not_a_directory_is_refused() {
    let scratch = scratch("ak-home");
    let mut account = account(&scratch);
    let missing = scratch.path().join("missing");
    account.home = missing.clone();
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::Io { path, .. }) if path == missing
    ));
    let plain = scratch.path().join("plain");
    fs::write(&plain, "").expect("a file");
    account.home = plain.clone();
    assert_eq!(
        put(&account, &keys(&[1])).expect_err("refused").to_string(),
        format!("{} is not a directory", plain.display())
    );
    assert!(!held(&account, &operator_keys::digest(&[])));
}

/// Proves a `.ssh` that is a regular file is refused.
#[test]
fn a_ssh_path_that_is_a_file_is_refused() {
    let scratch = scratch("ak-ssh-file");
    let account = account(&scratch);
    fs::write(ssh(&account), "").expect("a file");
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::NotADirectory { .. })
    ));
}

/// Proves a hand line that is not UTF-8 (a Latin-1 byte, which sshd accepts), before and after the
/// block, is kept byte for byte through a put, a key removal and the empty set, and the set is
/// held all the while.
///
/// Catches: a file read as text, so one such line made every set fail and the file never held.
#[test]
fn hand_lines_that_are_not_utf8_are_kept_byte_for_byte_and_the_set_is_held() {
    let scratch = scratch("ak-latin1");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    let before: &[u8] = b"ssh-rsa BEFORE caf\xe9\n";
    let after: &[u8] = b"ssh-rsa AFTER na\xefve\n";
    let around = |block: &[u8]| [before, block, after].concat();
    fs::write(file(&account), around(block(&keys(&[3])).as_bytes())).expect("file");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o600)).expect("mode");

    let both = keys(&[1, 2]);
    assert_eq!(put(&account, &both).expect("put"), Put::Written);
    assert_eq!(
        fs::read(file(&account)).expect("file"),
        around(block(&both).as_bytes())
    );
    assert!(held(&account, &operator_keys::digest(&both)));

    let one = keys(&[2]);
    assert_eq!(put(&account, &one).expect("put"), Put::Written);
    assert_eq!(
        fs::read(file(&account)).expect("file"),
        around(block(&one).as_bytes())
    );
    assert!(held(&account, &operator_keys::digest(&one)));
    assert_eq!(put(&account, &one).expect("put"), Put::Unchanged);

    assert_eq!(put(&account, &[]).expect("put"), Put::Written);
    assert_eq!(fs::read(file(&account)).expect("file"), around(&[]));
    assert!(held(&account, &operator_keys::digest(&[])));
}

/// Proves `merged` keeps the bytes of every line outside the block whatever they are, and puts
/// the block after a last line that is not UTF-8 and has no newline.
///
/// Catches: a merge over text that could not hold a line that is not UTF-8.
#[test]
fn merged_keeps_lines_that_are_not_utf8_exactly() {
    let set = keys(&[1]);
    let hand: &[u8] = b"\xe9\xff hand\r\n";
    assert_eq!(
        merged(&[hand, block(&set).as_bytes(), hand].concat(), &[]),
        Ok([hand, hand].concat())
    );
    assert_eq!(
        merged(b"caf\xe9", &set),
        Ok([b"caf\xe9\n".as_slice(), block(&set).as_bytes()].concat())
    );
}

/// Proves a key line of the block that is not UTF-8 is not a set the daemon sent: `block_keys`
/// refuses it with its line, the set is not held, and a put replaces the block.
///
/// Catches: a block read as text that panicked or was taken for the set on such a line.
#[test]
fn a_block_key_line_that_is_not_utf8_is_not_held_and_is_replaced() {
    let scratch = scratch("ak-key-latin1");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    let text = [
        format!("{BEGIN}\n").as_bytes(),
        b"ssh-rsa caf\xe9\n",
        format!("{END}\n").as_bytes(),
    ]
    .concat();
    assert_eq!(block_keys(&text), Err(BlockError::KeyNotText { line: 2 }));
    assert_eq!(
        BlockError::KeyNotText { line: 2 }.to_string(),
        "line 2 of the block is not text"
    );
    fs::write(file(&account), &text).expect("file");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(!held(&account, &operator_keys::digest(&[])));
    let set = keys(&[1]);
    assert_eq!(put(&account, &set).expect("put"), Put::Written);
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        block(&set)
    );
    assert!(held(&account, &operator_keys::digest(&set)));
}

/// Proves a dangling link at `.ssh` or at `authorized_keys` is refused like any link, nothing is
/// made through it, and a link at the home is followed, since it is the account's home.
///
/// Catches: a path looked at with `lstat` and then used by name, which a swapped link steers.
#[test]
fn a_dangling_link_is_refused_and_a_linked_home_is_followed() {
    let scratch = scratch("ak-dangling");
    let account = account(&scratch);
    let nowhere = scratch.path().join("nowhere");
    symlink(&nowhere, ssh(&account)).expect("link");
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::NotADirectory { .. })
    ));
    assert!(!nowhere.exists(), "nothing made through the link");
    assert!(!held(&account, &operator_keys::digest(&[])));
    fs::remove_file(ssh(&account)).expect("unlink");

    fs::create_dir(ssh(&account)).expect("ssh");
    symlink(&nowhere, file(&account)).expect("link");
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::NotAFile { .. })
    ));
    assert!(!nowhere.exists(), "nothing made through the link");
    fs::remove_file(file(&account)).expect("unlink");

    let linked = Account {
        home: scratch.path().join("linked-home"),
        ..account.clone()
    };
    symlink(&account.home, &linked.home).expect("link");
    assert_eq!(put(&linked, &keys(&[1])).expect("put"), Put::Written);
    assert!(held(&linked, &operator_keys::digest(&keys(&[1]))));
    assert_eq!(names(&ssh(&account)), ["authorized_keys"]);
}

/// Proves a named pipe at `authorized_keys` is refused as not a regular file without the daemon
/// waiting on it, in a put and in a held check.
///
/// Catches: a file opened for reading by name that blocks the daemon on a pipe the account made.
#[test]
fn a_pipe_is_refused_without_waiting() {
    let scratch = scratch("ak-pipe");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    hostkeeper_testkit::fifo(&file(&account), 0o600).expect("a pipe");
    assert!(matches!(
        put(&account, &keys(&[1])),
        Err(KeysFileError::NotAFile { .. })
    ));
    assert!(!held(&account, &operator_keys::digest(&[])));
    fs::remove_file(file(&account)).expect("unlink");
}

/// Proves a temporary file a killed run left is replaced, and one that cannot be removed (a
/// directory at its path) fails the write with the file as it was and nothing left.
#[test]
fn a_leftover_temporary_file_is_replaced_and_one_that_cannot_be_removed_fails() {
    let scratch = scratch("ak-leftover");
    let account = account(&scratch);
    fs::create_dir(ssh(&account)).expect("ssh");
    let temporary = ssh(&account).join(format!(".{TEMPORARY}.{}.tmp", std::process::id()));
    fs::write(&temporary, "left by a killed run").expect("leftover");
    assert_eq!(put(&account, &keys(&[1])).expect("put"), Put::Written);
    assert_eq!(names(&ssh(&account)), ["authorized_keys"]);

    fs::create_dir(&temporary).expect("a directory in its place");
    let error = put(&account, &keys(&[2])).expect_err("fails");
    assert!(
        matches!(&error, KeysFileError::Io { path, .. } if *path == file(&account)),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(file(&account)).expect("file"),
        block(&keys(&[1]))
    );
}

/// Proves what a heartbeat reports: the empty set is held with no file, a set is held once
/// written, and a block edited by hand, a key added to it, or the block removed is no longer
/// held; the digest of another set is not held.
#[test]
fn a_set_is_held_while_the_file_holds_its_block() {
    let scratch = scratch("ak-held");
    let account = account(&scratch);
    let set = keys(&[1, 2]);
    let digest = operator_keys::digest(&set);
    assert!(
        held(&account, &operator_keys::digest(&[])),
        "no file: the empty set"
    );
    assert!(!held(&account, &digest));
    put(&account, &set).expect("put");
    assert!(held(&account, &digest));
    assert!(!held(&account, &operator_keys::digest(&keys(&[1]))));

    let text = fs::read_to_string(file(&account)).expect("file");
    fs::write(file(&account), text.replace(&set[1], &key(3, "ops laptop"))).expect("edit");
    assert!(!held(&account, &digest), "a key edited by hand");
    put(&account, &set).expect("put");
    assert!(held(&account, &digest), "put back");
    fs::write(file(&account), "ssh-rsa HAND\n").expect("block removed");
    assert!(!held(&account, &digest));
    assert!(
        held(&account, &operator_keys::digest(&[])),
        "no block: the empty set"
    );
}

/// Proves a set is not held when the file is open to the group or others, is a link, is not
/// text, holds markers that make no block, or when the account's `.ssh` is another user's.
#[test]
fn a_set_is_not_held_by_a_file_that_is_not_the_accounts_own() {
    let scratch = scratch("ak-not-held");
    let mut account = account(&scratch);
    let set = keys(&[1]);
    let digest = operator_keys::digest(&set);
    put(&account, &set).expect("put");
    assert!(held(&account, &digest));

    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o640)).expect("mode");
    assert!(!held(&account, &digest), "a file the group can read");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(held(&account, &digest));

    let kept = fs::read_to_string(file(&account)).expect("file");
    fs::write(file(&account), format!("{kept}{BEGIN}\n")).expect("two begins");
    assert!(!held(&account, &digest), "markers that make no block");
    fs::write(file(&account), [0xff]).expect("binary");
    assert!(!held(&account, &digest), "not text");
    fs::remove_file(file(&account)).expect("unlink");
    fs::write(scratch.path().join("elsewhere"), &kept).expect("elsewhere");
    symlink(scratch.path().join("elsewhere"), file(&account)).expect("link");
    assert!(!held(&account, &digest), "a link");
    fs::remove_file(file(&account)).expect("unlink");
    fs::write(file(&account), &kept).expect("file");
    fs::set_permissions(file(&account), fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(held(&account, &digest));

    account.uid = me() + 1;
    assert!(!held(&account, &digest), "a .ssh of another user");
}

/// Proves a `.ssh` with no file in it holds the empty set and no other, and a `.ssh` that is not
/// there holds it too.
#[test]
fn a_ssh_directory_with_no_file_holds_the_empty_set() {
    let scratch = scratch("ak-no-file");
    let account = account(&scratch);
    let empty = operator_keys::digest(&[]);
    assert!(held(&account, &empty), "no .ssh");
    fs::create_dir(ssh(&account)).expect("ssh");
    assert!(held(&account, &empty), "no file");
    assert!(!held(&account, &operator_keys::digest(&keys(&[1]))));
}
